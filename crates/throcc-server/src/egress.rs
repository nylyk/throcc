use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_proto::frame::{Flags, Header};
use throcc_proto::ids::MediaId;
use throcc_proto::messages::{Event, ServerMessage, TrackStats};
use tokio::sync::mpsc;

use crate::rooms::Outgoing;

/// Queued video beyond this is dropped, in whole units that haven't started sending.
pub const VIDEO_BUDGET: Duration = Duration::from_millis(150);
const LINK_STATS_INTERVAL: Duration = Duration::from_secs(2);
const INBOX: usize = 8192;
/// About 5 s of 20 mic tracks: a cap against a receiver that stops reading, not a policy.
const MAX_QUEUED_AUDIO: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Video,
}

pub struct Packet {
    pub kind: TrackKind,
    pub header: Header,
    pub bytes: Bytes,
}

/// How ingress reaches one receiver. Offering never waits.
#[derive(Clone)]
pub struct EgressHandle {
    inbox: mpsc::Sender<Packet>,
    pub inbox_drops: Arc<AtomicU64>,
}

impl EgressHandle {
    pub fn offer(&self, packet: Packet) {
        if self.inbox.try_send(packet).is_err() {
            self.inbox_drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct VideoUnit {
    media_id: MediaId,
    packets: VecDeque<Bytes>,
    bytes: usize,
    arrived: Instant,
    started: bool,
    complete: bool,
    discardable: bool,
}

#[derive(Default)]
struct Counters {
    forwarded: u32,
    dropped_units: u32,
}

/// One receiver's queues: audio drains first, video goes in whole access units.
#[derive(Default)]
pub struct Queues {
    audio: VecDeque<(MediaId, Bytes)>,
    video: VecDeque<VideoUnit>,
    /// Tracks whose current unit was dropped; their fragments are discarded until the next first.
    dropping: HashMap<MediaId, ()>,
    stats: BTreeMap<MediaId, Counters>,
    /// Audio dropped by the safety cap, which only a receiver that stops reading reaches.
    pub audio_overflow: u64,
}

impl Queues {
    pub fn push(&mut self, packet: Packet, now: Instant) {
        let media_id = packet.header.media_id;
        match packet.kind {
            TrackKind::Audio => {
                if self.audio.len() >= MAX_QUEUED_AUDIO {
                    self.audio.pop_front();
                    self.audio_overflow += 1;
                }
                self.audio.push_back((media_id, packet.bytes));
            }
            TrackKind::Video => self.push_video(packet, now),
        }
    }

    fn push_video(&mut self, packet: Packet, now: Instant) {
        let flags = packet.header.flags.defined();
        let media_id = packet.header.media_id;
        if flags.contains(Flags::FIRST) {
            self.dropping.remove(&media_id);
            self.close_open_unit(media_id);
            self.video.push_back(VideoUnit {
                media_id,
                packets: VecDeque::new(),
                bytes: 0,
                arrived: now,
                started: false,
                complete: false,
                discardable: flags.contains(Flags::DISCARDABLE),
            });
        } else if self.dropping.contains_key(&media_id) {
            return;
        }
        let Some(unit) = self
            .video
            .iter_mut()
            .rev()
            .find(|u| u.media_id == media_id && !u.complete)
        else {
            return;
        };
        unit.bytes += packet.bytes.len();
        unit.packets.push_back(packet.bytes);
        unit.complete = flags.contains(Flags::LAST);
    }

    /// Ends the track's unfinished unit, whose last fragment was lost upstream: a started
    /// unit sends what it has, one not yet started is dropped.
    fn close_open_unit(&mut self, media_id: MediaId) {
        let Some(index) = self
            .video
            .iter()
            .position(|u| u.media_id == media_id && !u.complete)
        else {
            return;
        };
        if self.video[index].started {
            self.video[index].complete = true;
            if self.video[index].packets.is_empty() {
                self.video.remove(index);
            }
        } else {
            self.video.remove(index);
            self.stats.entry(media_id).or_default().dropped_units += 1;
        }
    }

    /// Drops video until the queue fits the budget: first discardable units that haven't
    /// started, then the oldest that haven't started.
    pub fn enforce_budget(&mut self, now: Instant) {
        while self.over_budget(now) {
            let victim = self
                .video
                .iter()
                .position(|u| !u.started && u.discardable)
                .or_else(|| self.video.iter().position(|u| !u.started));
            let Some(index) = victim else { return };
            let Some(unit) = self.video.remove(index) else {
                return;
            };
            if !unit.complete {
                self.dropping.insert(unit.media_id, ());
            }
            self.stats.entry(unit.media_id).or_default().dropped_units += 1;
        }
    }

    /// Queued video is measured as the wait of the oldest unit not yet sending. Quinn's
    /// buffer stays full, so the congestion controller keeps probing for more rate.
    fn over_budget(&self, now: Instant) -> bool {
        self.video
            .iter()
            .filter(|u| !u.started)
            .map(|u| u.arrived)
            .min()
            .is_some_and(|t| now.duration_since(t) > VIDEO_BUDGET)
    }

    pub fn pop(&mut self) -> Option<Bytes> {
        if let Some((media_id, bytes)) = self.audio.pop_front() {
            self.stats.entry(media_id).or_default().forwarded += 1;
            return Some(bytes);
        }
        let index = self.video.iter().position(|u| !u.packets.is_empty())?;
        let unit = self.video.get_mut(index)?;
        let bytes = unit.packets.pop_front()?;
        if !unit.started {
            unit.started = true;
            self.stats.entry(unit.media_id).or_default().forwarded += 1;
        }
        if unit.complete && unit.packets.is_empty() {
            self.video.remove(index);
        }
        Some(bytes)
    }

    fn take_stats(&mut self) -> Vec<TrackStats> {
        std::mem::take(&mut self.stats)
            .into_iter()
            .map(|(media_id, c)| TrackStats {
                media_id,
                forwarded: c.forwarded,
                dropped_units: c.dropped_units,
            })
            .collect()
    }
}

/// Starts the egress task for one receiver and returns the handle ingress offers packets to.
pub fn spawn(connection: quinn::Connection, control: mpsc::Sender<Outgoing>) -> EgressHandle {
    let (inbox, mut rx) = mpsc::channel(INBOX);
    let handle = EgressHandle {
        inbox,
        inbox_drops: Arc::default(),
    };
    tokio::spawn(async move {
        let mut queues = Queues::default();
        let mut next_stats = Instant::now() + LINK_STATS_INTERVAL;
        loop {
            while let Ok(packet) = rx.try_recv() {
                queues.push(packet, Instant::now());
            }
            let now = Instant::now();
            queues.enforce_budget(now);
            if now >= next_stats {
                next_stats = now + LINK_STATS_INTERVAL;
                let tracks = queues.take_stats();
                if !tracks.is_empty() {
                    let stats =
                        Outgoing::Message(ServerMessage::Event(Event::LinkStats { tracks }));
                    control.try_send(stats).ok();
                }
            }
            match queues.pop() {
                Some(bytes) => {
                    if connection.send_datagram_wait(bytes).await.is_err() {
                        return;
                    }
                }
                None => {
                    let wait = next_stats.saturating_duration_since(Instant::now());
                    match tokio::time::timeout(wait, rx.recv()).await {
                        Ok(Some(packet)) => queues.push(packet, Instant::now()),
                        Ok(None) => return,
                        Err(_) => {}
                    }
                }
            }
        }
    });
    handle
}

#[cfg(test)]
mod tests {
    use throcc_proto::ids::Epoch;

    use super::*;

    fn packet(kind: TrackKind, media: u32, seq: u32, flags: Flags, len: usize) -> Packet {
        Packet {
            kind,
            header: Header {
                media_id: MediaId(media),
                epoch: Epoch(0),
                seq,
                flags,
            },
            bytes: Bytes::from(vec![seq as u8; len]),
        }
    }

    fn unit(
        queues: &mut Queues,
        media: u32,
        first_seq: u32,
        fragments: u32,
        discardable: bool,
        now: Instant,
    ) {
        for i in 0..fragments {
            let flags = Flags::default()
                .with(Flags::FIRST, i == 0)
                .with(Flags::LAST, i == fragments - 1)
                .with(Flags::DISCARDABLE, discardable);
            queues.push(
                packet(TrackKind::Video, media, first_seq + i, flags, 1000),
                now,
            );
        }
    }

    #[test]
    fn audio_drains_before_video() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, false, now);
        queues.push(
            packet(
                TrackKind::Audio,
                2,
                0,
                Flags::FIRST.with(Flags::LAST, true),
                10,
            ),
            now,
        );
        assert_eq!(queues.pop().unwrap().len(), 10);
        assert_eq!(queues.pop().unwrap().len(), 1000);
    }

    #[test]
    fn drops_discardable_then_oldest_but_never_a_started_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 3, false, now);
        unit(&mut queues, 1, 3, 3, true, now);
        unit(&mut queues, 1, 6, 3, false, now);
        unit(
            &mut queues,
            1,
            9,
            3,
            false,
            now + Duration::from_millis(100),
        );
        queues.pop();
        queues.enforce_budget(now + Duration::from_millis(200));
        let left: Vec<u8> = std::iter::from_fn(|| queues.pop()).map(|b| b[0]).collect();
        assert_eq!(left, vec![1, 2, 9, 10, 11]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 2);
    }

    #[test]
    fn stale_units_are_dropped_and_their_tail_discarded() {
        let start = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 1000), start);
        let later = start + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(later);
        queues.push(packet(TrackKind::Video, 1, 1, Flags::LAST, 1000), later);
        assert!(queues.pop().is_none());
        unit(&mut queues, 1, 2, 1, false, later);
        assert_eq!(queues.pop().unwrap()[0], 2);
    }

    #[test]
    fn a_unit_whose_last_fragment_was_lost_does_not_stay_queued() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 1000), now);
        queues.pop();
        queues.push(packet(TrackKind::Video, 1, 2, Flags::FIRST, 1000), now);
        unit(&mut queues, 1, 3, 1, false, now);
        let sent: Vec<u8> = std::iter::from_fn(|| queues.pop()).map(|b| b[0]).collect();
        assert_eq!(sent, vec![3]);
        assert!(queues.video.is_empty());
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn queued_audio_has_a_hard_cap() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in 0..MAX_QUEUED_AUDIO as u32 + 10 {
            queues.push(
                packet(
                    TrackKind::Audio,
                    2,
                    seq,
                    Flags::FIRST.with(Flags::LAST, true),
                    10,
                ),
                now,
            );
        }
        assert_eq!(queues.audio.len(), MAX_QUEUED_AUDIO);
        assert_eq!(queues.audio_overflow, 10);
    }

    #[test]
    fn audio_is_never_dropped_by_the_budget() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in 0..500 {
            queues.push(
                packet(
                    TrackKind::Audio,
                    2,
                    seq,
                    Flags::FIRST.with(Flags::LAST, true),
                    100,
                ),
                now,
            );
        }
        queues.enforce_budget(now + Duration::from_secs(5));
        assert_eq!(std::iter::from_fn(|| queues.pop()).count(), 500);
    }
}
