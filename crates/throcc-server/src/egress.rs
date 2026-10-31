use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
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
/// About 150 ms of 16 watched 20 Mbps shares. Over this, even started units are dropped.
const MAX_QUEUED_VIDEO_BYTES: usize = 8 * 1024 * 1024;
/// Two seconds of 20 mic tracks at 50 frames/s: a cap against a receiver that stops
/// reading, not a drop policy.
const MAX_QUEUED_AUDIO: usize = 2000;
/// A unit's fragment limit: a 1440p keyframe at 18 Mbps is well under 1000.
const MAX_UNIT_FRAGMENTS: usize = 4096;
/// A started unit that gets no fragment for this long is given up on.
const UNIT_IDLE: Duration = Duration::from_secs(1);
/// Unit starts remembered per track, to place late fragments.
const HISTORY: usize = 32;

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
    /// Packets that found the inbox full. Locked only when that happens.
    inbox_drops: Arc<Mutex<InboxDrops>>,
}

impl EgressHandle {
    pub fn offer(&self, packet: Packet) {
        let lost = Lost {
            kind: packet.kind,
            media_id: packet.header.media_id,
            seq: packet.header.seq,
            first: packet.header.flags.defined().contains(Flags::FIRST),
        };
        if self.inbox.try_send(packet).is_err() {
            self.inbox_drops
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .record(lost);
        }
    }
}

/// A packet that found the inbox full.
#[derive(Clone, Copy, Debug)]
struct Lost {
    kind: TrackKind,
    media_id: MediaId,
    seq: u32,
    first: bool,
}

#[derive(Default)]
struct InboxDrops {
    lost: Vec<Lost>,
    /// Losses beyond what `lost` holds, counted one unit per packet.
    overflow: BTreeMap<MediaId, u32>,
}

impl InboxDrops {
    fn record(&mut self, lost: Lost) {
        if self.lost.len() < INBOX {
            self.lost.push(lost);
        } else {
            *self.overflow.entry(lost.media_id).or_default() += 1;
        }
    }
}

/// What became of a unit, by its first `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Queued(u64),
    Sent,
    Dropped,
}

struct VideoUnit {
    id: u64,
    media_id: MediaId,
    packets: VecDeque<Bytes>,
    /// Bytes of `packets`.
    bytes: usize,
    received: usize,
    arrived: Instant,
    last_fragment: Instant,
    started: bool,
    /// The last fragment has arrived.
    complete: bool,
    discardable: bool,
    /// A fragment of a unit already sent, forwarded on its own and never counted as a drop.
    stray: bool,
    first_seq: u32,
    /// A resent copy of a unit seen before. Counted only once no other copy is queued.
    copy: bool,
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
    history: HashMap<MediaId, BTreeMap<u32, Fate>>,
    next_id: u64,
    stats: BTreeMap<MediaId, Counters>,
    video_bytes: usize,
}

impl Queues {
    pub fn push(&mut self, packet: Packet, now: Instant) {
        let media_id = packet.header.media_id;
        match packet.kind {
            TrackKind::Audio => {
                if self.audio.len() >= MAX_QUEUED_AUDIO
                    && let Some((dropped, _)) = self.audio.pop_front()
                {
                    self.stats.entry(dropped).or_default().dropped_units += 1;
                }
                self.audio.push_back((media_id, packet.bytes));
            }
            TrackKind::Video => self.push_video(packet, now),
        }
    }

    /// Places a fragment in the unit whose first `seq` is the closest at or below its own,
    /// so fragments reordered on the way in still join the right unit.
    fn push_video(&mut self, packet: Packet, now: Instant) {
        let Header {
            media_id,
            seq,
            flags,
            ..
        } = packet.header;
        let flags = flags.defined();
        if flags.contains(Flags::FIRST) {
            let id = self.next_id;
            self.next_id += 1;
            let history = self.history.entry(media_id).or_default();
            let copy = match history.get(&seq) {
                Some(Fate::Dropped) => {
                    let counters = self.stats.entry(media_id).or_default();
                    counters.dropped_units = counters.dropped_units.saturating_sub(1);
                    false
                }
                seen => seen.is_some(),
            };
            self.video_bytes += packet.bytes.len();
            let unit = VideoUnit {
                id,
                media_id,
                bytes: packet.bytes.len(),
                packets: VecDeque::from([packet.bytes]),
                received: 1,
                arrived: now,
                last_fragment: now,
                started: false,
                complete: flags.contains(Flags::LAST),
                discardable: flags.contains(Flags::DISCARDABLE),
                stray: false,
                first_seq: seq,
                copy,
            };
            self.video.push_back(unit);
            remember(history, seq, Fate::Queued(id));
            return;
        }
        let fate = self
            .history
            .get(&media_id)
            .and_then(|h| h.range(..=seq).next_back())
            .map(|(_, fate)| *fate);
        match fate {
            Some(Fate::Queued(id)) => {
                if let Some(unit) = self.video.iter_mut().find(|u| u.id == id)
                    && unit.received < MAX_UNIT_FRAGMENTS
                {
                    unit.bytes += packet.bytes.len();
                    self.video_bytes += packet.bytes.len();
                    unit.packets.push_back(packet.bytes);
                    unit.received += 1;
                    unit.last_fragment = now;
                    unit.complete |= flags.contains(Flags::LAST);
                }
            }
            Some(Fate::Sent) => {
                self.video_bytes += packet.bytes.len();
                self.video.push_back(VideoUnit {
                    id: u64::MAX,
                    media_id,
                    bytes: packet.bytes.len(),
                    packets: VecDeque::from([packet.bytes]),
                    received: 1,
                    arrived: now,
                    last_fragment: now,
                    started: false,
                    complete: true,
                    discardable: true,
                    stray: true,
                    first_seq: seq,
                    copy: false,
                });
            }
            Some(Fate::Dropped) | None => {}
        }
    }

    /// Accounts for a packet that never reached the queues. A lost video fragment drops
    /// the rest of its unit, and the unit counts as dropped once.
    fn lose(&mut self, lost: Lost) {
        let Lost {
            kind,
            media_id,
            seq,
            first,
        } = lost;
        if kind == TrackKind::Audio {
            self.stats.entry(media_id).or_default().dropped_units += 1;
            return;
        }
        let history = self.history.entry(media_id).or_default();
        if first {
            if !history.contains_key(&seq) {
                remember(history, seq, Fate::Dropped);
                self.stats.entry(media_id).or_default().dropped_units += 1;
            }
            return;
        }
        let found = history
            .range(..=seq)
            .next_back()
            .map(|(first_seq, fate)| (*first_seq, *fate));
        match found {
            Some((_, Fate::Queued(id))) => self.drop_unit(id),
            Some((first_seq, Fate::Sent)) => {
                remember(history, first_seq, Fate::Dropped);
                self.stats.entry(media_id).or_default().dropped_units += 1;
            }
            Some((_, Fate::Dropped)) => {}
            None => {
                remember(history, seq, Fate::Dropped);
                self.stats.entry(media_id).or_default().dropped_units += 1;
            }
        }
    }

    /// Drops video until the queue fits the budget: first discardable units that haven't
    /// started, then the oldest that haven't started. Also gives up on started units
    /// whose remaining fragments stopped arriving.
    pub fn enforce_budget(&mut self, now: Instant) {
        let idle: Vec<u64> = self
            .video
            .iter()
            .filter(|u| {
                u.started && u.packets.is_empty() && now.duration_since(u.last_fragment) > UNIT_IDLE
            })
            .map(|u| u.id)
            .collect();
        for id in idle {
            self.finish(id, Fate::Sent);
        }
        while self.over_budget(now) {
            let Some(victim) = self.unstarted_victim() else {
                break;
            };
            self.drop_unit(victim);
        }
        while self.video_bytes > MAX_QUEUED_VIDEO_BYTES {
            let Some(victim) = self.unstarted_victim().or(self.video.front().map(|u| u.id)) else {
                return;
            };
            self.drop_unit(victim);
        }
    }

    fn unstarted_victim(&self) -> Option<u64> {
        self.video
            .iter()
            .find(|u| !u.started && u.discardable)
            .or_else(|| self.video.iter().find(|u| !u.started))
            .map(|u| u.id)
    }

    /// Drops a unit, counting it unless another copy of it is still queued.
    fn drop_unit(&mut self, id: u64) {
        let Some(unit) = self.video.iter().find(|u| u.id == id) else {
            return;
        };
        let (media_id, first_seq, uncounted) =
            (unit.media_id, unit.first_seq, unit.stray || unit.copy);
        self.finish(id, Fate::Dropped);
        let twin = self
            .video
            .iter_mut()
            .find(|u| u.media_id == media_id && u.first_seq == first_seq && !u.stray);
        if let Some(twin) = twin {
            twin.copy = false;
        } else if !uncounted {
            self.stats.entry(media_id).or_default().dropped_units += 1;
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

    fn finish(&mut self, id: u64, fate: Fate) {
        let index = self.video.iter().position(|u| u.id == id);
        let Some(unit) = index.and_then(|i| self.video.remove(i)) else {
            return;
        };
        self.video_bytes -= unit.bytes;
        if let Some(history) = self.history.get_mut(&unit.media_id) {
            for f in history.values_mut().filter(|f| **f == Fate::Queued(id)) {
                *f = fate;
            }
        }
    }

    pub fn pop(&mut self) -> Option<Bytes> {
        if let Some((media_id, bytes)) = self.audio.pop_front() {
            self.stats.entry(media_id).or_default().forwarded += 1;
            return Some(bytes);
        }
        let unit = self.video.iter_mut().find(|u| !u.packets.is_empty())?;
        let bytes = unit.packets.pop_front()?;
        unit.bytes -= bytes.len();
        self.video_bytes -= bytes.len();
        if !unit.started {
            unit.started = true;
            if !unit.stray && !unit.copy {
                self.stats.entry(unit.media_id).or_default().forwarded += 1;
            }
        }
        if unit.complete && unit.packets.is_empty() {
            let id = unit.id;
            self.finish(id, Fate::Sent);
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

/// Records the fate of the unit starting at `first_seq`, keeping the last [`HISTORY`].
fn remember(history: &mut BTreeMap<u32, Fate>, first_seq: u32, fate: Fate) {
    history.insert(first_seq, fate);
    while history.len() > HISTORY {
        history.pop_first();
    }
}

/// Starts the egress task for one receiver and returns the handle ingress offers packets to.
pub fn spawn(connection: quinn::Connection, control: mpsc::Sender<Outgoing>) -> EgressHandle {
    let (inbox, mut rx) = mpsc::channel(INBOX);
    let handle = EgressHandle {
        inbox,
        inbox_drops: Arc::default(),
    };
    let inbox_drops = handle.inbox_drops.clone();
    tokio::spawn(async move {
        let mut queues = Queues::default();
        let mut next_stats = Instant::now() + LINK_STATS_INTERVAL;
        loop {
            let InboxDrops { lost, overflow } =
                std::mem::take(&mut *inbox_drops.lock().unwrap_or_else(PoisonError::into_inner));
            // A lost first fragment is placed before the drain, so the rest of its unit is
            // discarded on arrival. Other losses wait until the start of their unit is queued.
            let (first, rest): (Vec<Lost>, Vec<Lost>) = lost.into_iter().partition(|l| l.first);
            for lost in first {
                queues.lose(lost);
            }
            while let Ok(packet) = rx.try_recv() {
                queues.push(packet, Instant::now());
            }
            for lost in rest {
                queues.lose(lost);
            }
            for (media_id, count) in overflow {
                queues.stats.entry(media_id).or_default().dropped_units += count;
            }
            let now = Instant::now();
            queues.enforce_budget(now);
            if now >= next_stats {
                next_stats = now + LINK_STATS_INTERVAL;
                if let Ok(permit) = control.try_reserve() {
                    let tracks = queues.take_stats();
                    if !tracks.is_empty() {
                        permit.send(Outgoing::Message(ServerMessage::Event(Event::LinkStats {
                            tracks,
                        })));
                    }
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

    fn fragment_flags(i: u32, count: u32, discardable: bool) -> Flags {
        Flags::default()
            .with(Flags::FIRST, i == 0)
            .with(Flags::LAST, i == count - 1)
            .with(Flags::DISCARDABLE, discardable)
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
            let flags = fragment_flags(i, fragments, discardable);
            queues.push(
                packet(TrackKind::Video, media, first_seq + i, flags, 1000),
                now,
            );
        }
    }

    fn drain(queues: &mut Queues) -> Vec<u8> {
        std::iter::from_fn(|| queues.pop()).map(|b| b[0]).collect()
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
        assert_eq!(drain(&mut queues), vec![1, 2, 9, 10, 11]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 2);
    }

    #[test]
    fn a_dropped_copy_counts_only_when_no_copy_is_left() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 3, false, now);
        unit(&mut queues, 1, 0, 3, false, now + Duration::from_millis(60));
        unit(
            &mut queues,
            1,
            3,
            2,
            false,
            now + Duration::from_millis(100),
        );
        let late = now + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(late);
        assert_eq!(queues.stats.get(&MediaId(1)).map(|s| s.dropped_units), None);
        queues.enforce_budget(late + Duration::from_millis(100));
        assert_eq!(drain(&mut queues), Vec::<u8>::new());
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 2);
    }

    #[test]
    fn a_copy_of_a_sent_unit_is_neither_forwarded_nor_dropped_in_the_stats() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, false, now);
        assert_eq!(drain(&mut queues), vec![0, 1]);
        unit(&mut queues, 1, 0, 2, false, now);
        queues.enforce_budget(now + VIDEO_BUDGET + Duration::from_millis(1));
        assert_eq!(drain(&mut queues), Vec::<u8>::new());
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (1, 0));
    }

    #[test]
    fn a_copy_arriving_after_its_original_was_dropped_undoes_the_drop() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, false, now);
        let late = now + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(late);
        unit(&mut queues, 1, 0, 2, false, late);
        assert_eq!(drain(&mut queues), vec![0, 1]);
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (1, 0));
    }

    #[test]
    fn fragments_of_a_dropped_unit_are_discarded() {
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
    fn fragments_reordered_upstream_join_their_own_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in [0, 1, 3, 2, 4, 5] {
            let flags = fragment_flags(seq % 3, 3, false);
            queues.push(packet(TrackKind::Video, 1, seq, flags, 1000), now);
        }
        assert_eq!(drain(&mut queues), vec![0, 1, 2, 3, 4, 5]);
        assert!(queues.video.is_empty());
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 0);
    }

    #[test]
    fn a_late_fragment_of_a_sent_unit_is_still_forwarded() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 1000), now);
        queues.push(packet(TrackKind::Video, 1, 2, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), vec![0, 2]);
        queues.push(packet(TrackKind::Video, 1, 1, Flags::default(), 1000), now);
        assert_eq!(drain(&mut queues), vec![1]);
    }

    #[test]
    fn a_unit_whose_tail_never_arrives_is_given_up_after_a_second() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 1000), now);
        queues.pop();
        queues.enforce_budget(now + UNIT_IDLE + Duration::from_millis(1));
        assert!(queues.video.is_empty());
    }

    #[test]
    fn a_unit_cannot_grow_without_bound() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 10), now);
        for seq in 1..MAX_UNIT_FRAGMENTS as u32 * 2 {
            queues.push(packet(TrackKind::Video, 1, seq, Flags::default(), 10), now);
        }
        assert_eq!(queues.video[0].packets.len(), MAX_UNIT_FRAGMENTS);
    }

    #[test]
    fn queued_audio_has_a_hard_cap_that_is_reported() {
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
        let stats = queues.take_stats();
        assert_eq!(stats[0].dropped_units, 10);
    }

    fn lost(kind: TrackKind, media: u32, seq: u32, first: bool) -> Lost {
        Lost {
            kind,
            media_id: MediaId(media),
            seq,
            first,
        }
    }

    #[test]
    fn a_fragment_lost_at_the_inbox_drops_the_rest_of_its_unit_once() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for (seq, flags) in [(0, Flags::FIRST), (2, Flags::default())] {
            queues.push(packet(TrackKind::Video, 1, seq, flags, 1000), now);
        }
        queues.lose(lost(TrackKind::Video, 1, 1, false));
        queues.lose(lost(TrackKind::Video, 1, 3, false));
        queues.push(packet(TrackKind::Video, 1, 4, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), Vec::<u8>::new());
        unit(&mut queues, 1, 5, 2, false, now);
        assert_eq!(drain(&mut queues), vec![5, 6]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
        assert_eq!(queues.video_bytes, 0);
    }

    #[test]
    fn a_lost_first_fragment_discards_the_fragments_after_it() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, false, now);
        queues.lose(lost(TrackKind::Video, 1, 2, true));
        for (seq, flags) in [(3, Flags::default()), (4, Flags::LAST)] {
            queues.push(packet(TrackKind::Video, 1, seq, flags, 1000), now);
        }
        assert_eq!(drain(&mut queues), vec![0, 1]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn a_unit_sent_with_a_hole_counts_as_dropped_once() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 1000), now);
        queues.push(packet(TrackKind::Video, 1, 2, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), vec![0, 2]);
        queues.lose(lost(TrackKind::Video, 1, 1, false));
        queues.lose(lost(TrackKind::Video, 1, 1, false));
        queues.lose(lost(TrackKind::Audio, 2, 0, true));
        let stats = queues.take_stats();
        assert_eq!(stats[0].dropped_units, 1);
        assert_eq!(stats[1].dropped_units, 1);
    }

    #[test]
    fn queued_video_bytes_are_capped_even_for_a_started_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(TrackKind::Video, 1, 0, Flags::FIRST, 4096), now);
        queues.pop();
        let fragments = (MAX_QUEUED_VIDEO_BYTES / 4096) as u32 + 10;
        for seq in 1..fragments {
            queues.push(
                packet(TrackKind::Video, 1, seq, Flags::default(), 4096),
                now,
            );
        }
        assert!(queues.video_bytes > MAX_QUEUED_VIDEO_BYTES);
        queues.enforce_budget(now);
        assert!(queues.video.is_empty());
        assert_eq!(queues.video_bytes, 0);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
        queues.push(
            packet(TrackKind::Video, 1, fragments, Flags::LAST, 4096),
            now,
        );
        assert!(queues.pop().is_none());
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
