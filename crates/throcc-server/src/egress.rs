use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_proto::frame::{Flags, Header};
use throcc_proto::ids::MediaId;
use throcc_proto::limits::{MAX_ROOM_MEMBERS, MAX_ROOM_SHARES};
use throcc_proto::messages::{Event, Recovery, ServerMessage, TrackStats};
use tokio::sync::mpsc;

use crate::lock;
use crate::rooms::Outbound;
use crate::sfu::MAX_INGRESS_BYTES_PER_SECOND;

/// Queued video beyond this is dropped, in whole units that haven't started sending.
pub const VIDEO_BUDGET: Duration = Duration::from_millis(150);
const RECEIVE_STATS_INTERVAL: Duration = Duration::from_secs(2);
const INBOX: usize = 8192;
/// The video budget of every share in a room at the ingress limit. Over this, even started
/// units are dropped.
const MAX_QUEUED_VIDEO_BYTES: usize =
    (MAX_ROOM_SHARES as f64 * MAX_INGRESS_BYTES_PER_SECOND * VIDEO_BUDGET.as_secs_f64()) as usize;
/// Two seconds of every audio track a full room sends one receiver, at 50 frames/s. Reaching
/// it costs the longest track a packet.
const MAX_QUEUED_AUDIO: usize = 2 * 50 * (MAX_ROOM_MEMBERS - 1 + MAX_ROOM_SHARES);
/// A unit's fragment limit: a 1440p keyframe at 15 Mbps is well under 1000.
const MAX_UNIT_FRAGMENTS: usize = 4096;
/// A started unit that gets no fragment for this long is given up on.
const UNIT_IDLE: Duration = Duration::from_secs(1);
const IDLE_CHECK: Duration = Duration::from_millis(100);
/// Unit starts remembered per track, to place late fragments.
const HISTORY: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    /// Video, and how its sender heals a picture a loss broke.
    Video(Recovery),
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
    /// The tracks this receiver holds back until a keyframe, as of the egress task's last
    /// change to them.
    held: Arc<Mutex<Vec<MediaId>>>,
}

impl EgressHandle {
    /// The tracks waiting for a keyframe for this receiver.
    pub fn held(&self) -> Vec<MediaId> {
        lock(&self.held).clone()
    }

    pub fn offer(&self, packet: Packet) {
        if let Err(e) = self.inbox.try_send(packet) {
            let packet = e.into_inner();
            lock(&self.inbox_drops).record(Lost {
                kind: packet.kind,
                media_id: packet.header.media_id,
                seq: packet.header.seq,
                first: packet.header.flags.contains(Flags::FIRST),
            });
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
    /// Turned away by a hold: never counted, so a copy undoes nothing.
    HeldBack,
}

struct VideoUnit {
    id: u64,
    media_id: MediaId,
    packets: VecDeque<Bytes>,
    received: usize,
    arrived: Instant,
    last_fragment: Instant,
    started: bool,
    /// The last fragment has arrived.
    complete: bool,
    /// A fragment of a unit already sent, forwarded on its own and never counted as a drop.
    stray: bool,
    first_seq: u32,
    /// A resent copy of a unit seen before. Counted only once no other copy is queued.
    copy: bool,
    keyframe: bool,
}

/// The `seq` span of an `Idr` track's lost units. A unit from before it still decodes;
/// nothing after it does until a keyframe from its end on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    first: u32,
    last: u32,
}

impl VideoUnit {
    fn new(id: u64, media_id: MediaId, first_seq: u32, bytes: Bytes, now: Instant) -> Self {
        Self {
            id,
            media_id,
            packets: VecDeque::from([bytes]),
            received: 1,
            arrived: now,
            last_fragment: now,
            started: false,
            complete: false,
            stray: false,
            first_seq,
            copy: false,
            keyframe: false,
        }
    }
}

#[derive(Default)]
struct Counters {
    forwarded: u32,
    dropped_units: u32,
}

/// One receiver's audio, a queue per track, served one packet per track in turn. At the
/// cap the longest queue makes room, so a track that floods only ever loses its own
/// packets.
#[derive(Default)]
struct AudioQueues {
    tracks: HashMap<MediaId, VecDeque<Bytes>>,
    /// Tracks with packets queued, in the order they are served.
    turns: VecDeque<MediaId>,
    len: usize,
}

impl AudioQueues {
    /// Queues a packet. Returns the track that lost its oldest packet to make room, if one did.
    fn push(&mut self, media_id: MediaId, bytes: Bytes) -> Option<MediaId> {
        let evicted = if self.len >= MAX_QUEUED_AUDIO {
            self.evict_from_longest()
        } else {
            None
        };
        let queue = self.tracks.entry(media_id).or_default();
        if queue.is_empty() {
            self.turns.push_back(media_id);
        }
        queue.push_back(bytes);
        self.len += 1;
        evicted
    }

    fn evict_from_longest(&mut self) -> Option<MediaId> {
        let (&media_id, queue) = self.tracks.iter_mut().max_by_key(|(_, q)| q.len())?;
        queue.pop_front();
        self.len -= 1;
        if queue.is_empty() {
            self.tracks.remove(&media_id);
            self.turns.retain(|id| *id != media_id);
        }
        Some(media_id)
    }

    fn pop(&mut self) -> Option<(MediaId, Bytes)> {
        let media_id = self.turns.pop_front()?;
        let queue = self.tracks.get_mut(&media_id)?;
        let bytes = queue.pop_front()?;
        self.len -= 1;
        if queue.is_empty() {
            self.tracks.remove(&media_id);
        } else {
            self.turns.push_back(media_id);
        }
        Some((media_id, bytes))
    }
}

/// One receiver's queues: audio drains first, video goes in whole access units.
///
/// Video units are kept by id, which is their order of arrival, so that every step costs
/// at most a logarithm of the queue however many units a sender makes. Started units are
/// all older than waiting ones: `pop` serves the oldest unit that holds packets, and a
/// waiting unit is never empty.
#[derive(Default)]
pub struct Queues {
    audio: AudioQueues,
    started: BTreeMap<u64, VideoUnit>,
    waiting: BTreeMap<u64, VideoUnit>,
    /// Started units that still hold packets.
    sending: BTreeSet<u64>,
    /// Waiting strays in id order. Units that have since left are skipped when met at the
    /// front.
    strays: VecDeque<u64>,
    /// Queued units that aren't strays, by track and first `seq`, to find another copy.
    copies: HashMap<(MediaId, u32), BTreeSet<u64>>,
    next_idle_check: Option<Instant>,
    history: HashMap<MediaId, BTreeMap<u32, Fate>>,
    /// Video tracks seen since the last [`Queues::forget_quiet_tracks`].
    active_tracks: HashSet<MediaId>,
    /// Video tracks whose sender heals a broken picture only with a keyframe.
    idr_tracks: HashSet<MediaId>,
    /// Of those, the ones that lost a unit since their last keyframe. Nothing newer than
    /// the loss is queued until a keyframe, since nothing in between would decode.
    held: BTreeMap<MediaId, Hold>,
    held_changed: bool,
    /// The first `seq` of each video track's newest keyframe that started sending.
    sent_keyframes: HashMap<MediaId, u32>,
    next_id: u64,
    stats: BTreeMap<MediaId, Counters>,
    video_bytes: usize,
}

impl Queues {
    pub fn push(&mut self, packet: Packet, now: Instant) {
        match packet.kind {
            TrackKind::Audio => {
                if let Some(evicted) = self.audio.push(packet.header.media_id, packet.bytes) {
                    self.count_drop(evicted);
                }
            }
            TrackKind::Video(recovery) => {
                self.learn(packet.header.media_id, recovery);
                self.push_video(packet, now);
            }
        }
    }

    fn learn(&mut self, media_id: MediaId, recovery: Recovery) {
        match recovery {
            Recovery::Idr => self.idr_tracks.insert(media_id),
            Recovery::Refresh => {
                self.release(media_id);
                self.idr_tracks.remove(&media_id)
            }
        };
    }

    /// Counts the lost video unit starting at `seq`, and holds its track back if only a
    /// keyframe heals it.
    fn break_track(&mut self, media_id: MediaId, seq: u32) {
        self.count_drop(media_id);
        self.hold(media_id, seq);
    }

    /// Holds an `Idr` track back from the unit lost at `seq` on. Its queued units after the
    /// loss go, up to a keyframe queued after it. A keyframe from after the loss, queued or
    /// already sent, heals the picture: then there is nothing to hold. The loss already
    /// counted, so neither these nor the units the hold turns away count again.
    fn hold(&mut self, media_id: MediaId, seq: u32) {
        let healed = self.sent_keyframes.get(&media_id).is_some_and(|&k| k > seq);
        if !self.idr_tracks.contains(&media_id) || healed {
            return;
        }
        let mut after: Vec<&VideoUnit> = self
            .waiting
            .values()
            .filter(|u| u.media_id == media_id && u.first_seq >= seq && !u.stray)
            .collect();
        after.sort_by_key(|u| u.first_seq);
        let keyframe = after.iter().position(|u| u.keyframe);
        let undecodable: Vec<u64> = after[..keyframe.unwrap_or(after.len())]
            .iter()
            .map(|u| u.id)
            .collect();
        for id in undecodable {
            self.finish(id, Fate::Dropped);
        }
        if keyframe.is_some() {
            return;
        }
        let hold = self.held.entry(media_id).or_insert_with(|| {
            self.held_changed = true;
            Hold {
                first: seq,
                last: seq,
            }
        });
        hold.first = hold.first.min(seq);
        hold.last = hold.last.max(seq);
    }

    /// Holds a track that lost packets nobody recorded. They may be from after anything
    /// seen, so the hold starts past the newest unit.
    fn hold_after_newest(&mut self, media_id: MediaId) {
        let newest = self
            .history
            .get(&media_id)
            .and_then(|h| h.keys().next_back());
        if let Some(&newest) = newest {
            self.hold(media_id, newest.saturating_add(1));
        }
    }

    fn release(&mut self, media_id: MediaId) {
        if self.held.remove(&media_id).is_some() {
            self.held_changed = true;
        }
    }

    /// The held tracks, if they changed since the last call.
    fn take_held_change(&mut self) -> Option<Vec<MediaId>> {
        std::mem::take(&mut self.held_changed).then(|| self.held.keys().copied().collect())
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
        self.active_tracks.insert(media_id);
        let keyframe = flags.contains(Flags::KEYFRAME);
        if flags.contains(Flags::FIRST) {
            let history = self.history.entry(media_id).or_default();
            if history.get(&seq) == Some(&Fate::HeldBack) {
                return;
            }
            if let Some(hold) = self.held.get(&media_id)
                && seq >= hold.first
            {
                if !keyframe || seq < hold.last {
                    remember(history, seq, Fate::HeldBack);
                    return;
                }
                self.release(media_id);
            }
            let id = self.new_id();
            let history = self.history.entry(media_id).or_default();
            let copy = match history.get(&seq) {
                Some(Fate::Dropped) => {
                    let counters = self.stats.entry(media_id).or_default();
                    counters.dropped_units = counters.dropped_units.saturating_sub(1);
                    false
                }
                seen => seen.is_some(),
            };
            remember(history, seq, Fate::Queued(id));
            self.copies.entry((media_id, seq)).or_default().insert(id);
            self.queue_unit(VideoUnit {
                complete: flags.contains(Flags::LAST),
                copy,
                keyframe,
                ..VideoUnit::new(id, media_id, seq, packet.bytes, now)
            });
            return;
        }
        let fate = self
            .history
            .get(&media_id)
            .and_then(|h| h.range(..=seq).next_back())
            .map(|(_, fate)| *fate);
        match fate {
            Some(Fate::Queued(id)) => {
                let Some(unit) = self.unit_mut(id) else {
                    return;
                };
                if unit.received >= MAX_UNIT_FRAGMENTS {
                    // Forwarding only part of it would break whole-unit delivery.
                    self.drop_unit(id);
                    return;
                }
                let len = packet.bytes.len();
                unit.packets.push_back(packet.bytes);
                unit.received += 1;
                unit.last_fragment = now;
                unit.complete |= flags.contains(Flags::LAST);
                if unit.started {
                    self.sending.insert(id);
                }
                self.video_bytes += len;
            }
            Some(Fate::Sent) => {
                let id = self.new_id();
                self.queue_unit(VideoUnit {
                    complete: true,
                    stray: true,
                    ..VideoUnit::new(id, media_id, seq, packet.bytes, now)
                });
            }
            Some(Fate::Dropped | Fate::HeldBack) | None => {}
        }
    }

    fn queue_unit(&mut self, unit: VideoUnit) {
        self.video_bytes += unit.packets.iter().map(Bytes::len).sum::<usize>();
        if unit.stray {
            self.strays.push_back(unit.id);
        }
        self.waiting.insert(unit.id, unit);
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
        let TrackKind::Video(recovery) = kind else {
            self.count_drop(media_id);
            return;
        };
        self.learn(media_id, recovery);
        self.active_tracks.insert(media_id);
        let history = self.history.entry(media_id).or_default();
        if first {
            if !history.contains_key(&seq) {
                remember(history, seq, Fate::Dropped);
                self.break_track(media_id, seq);
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
                self.break_track(media_id, first_seq);
            }
            Some((_, Fate::Dropped | Fate::HeldBack)) => {}
            None => {
                remember(history, seq, Fate::Dropped);
                self.break_track(media_id, seq);
            }
        }
    }

    /// Drops video until the queue fits the budget: first strays that haven't started, then
    /// the oldest units that haven't started. Also gives up on started units
    /// whose remaining fragments stopped arriving.
    pub fn enforce_budget(&mut self, now: Instant) {
        self.give_up_idle_units(now);
        while self.over_budget(now) {
            let Some(victim) = self.unstarted_victim() else {
                break;
            };
            self.drop_unit(victim);
        }
        while self.video_bytes > MAX_QUEUED_VIDEO_BYTES {
            let oldest = self.started.first_key_value().map(|(id, _)| *id);
            let Some(victim) = self.unstarted_victim().or(oldest) else {
                return;
            };
            self.drop_unit(victim);
        }
    }

    /// Checks at most every [`IDLE_CHECK`], since a unit waits [`UNIT_IDLE`] anyway.
    fn give_up_idle_units(&mut self, now: Instant) {
        if self.next_idle_check.is_some_and(|t| now < t) {
            return;
        }
        self.next_idle_check = Some(now + IDLE_CHECK);
        let idle: Vec<(u64, MediaId, u32, bool)> = self
            .started
            .values()
            .filter(|u| u.packets.is_empty() && now.duration_since(u.last_fragment) > UNIT_IDLE)
            .map(|u| {
                let cut_short = !u.complete && !u.copy && !u.stray;
                (u.id, u.media_id, u.first_seq, cut_short)
            })
            .collect();
        for (id, media_id, first_seq, cut_short) in idle {
            self.finish(id, Fate::Sent);
            // A unit sent without its end doesn't decode, nor does anything after it.
            if cut_short {
                self.hold(media_id, first_seq);
            }
        }
    }

    fn unstarted_victim(&mut self) -> Option<u64> {
        while let Some(&id) = self.strays.front() {
            if self.waiting.contains_key(&id) {
                return Some(id);
            }
            self.strays.pop_front();
        }
        self.waiting.first_key_value().map(|(id, _)| *id)
    }

    /// Drops a unit, counting it unless another copy of it is still queued. Its track is
    /// held back unless a copy of it was sent or still is queued: a dropped stray breaks
    /// the unit it belongs to.
    fn drop_unit(&mut self, id: u64) {
        let Some(unit) = self.unit_mut(id) else {
            return;
        };
        let (media_id, first_seq, stray, copy) =
            (unit.media_id, unit.first_seq, unit.stray, unit.copy);
        self.finish(id, Fate::Dropped);
        let twin = self
            .copies
            .get(&(media_id, first_seq))
            .and_then(|ids| ids.first().copied());
        if let Some(twin) = twin.and_then(|twin| self.unit_mut(twin)) {
            twin.copy = false;
        } else if stray {
            self.hold(media_id, first_seq);
        } else if !copy {
            self.break_track(media_id, first_seq);
        }
    }

    /// Queued video is measured as the wait of the oldest unit not yet sending, which is
    /// the first of them. Quinn's buffer stays full, so the congestion controller keeps
    /// probing for more rate.
    fn over_budget(&self, now: Instant) -> bool {
        self.waiting
            .first_key_value()
            .is_some_and(|(_, u)| now.duration_since(u.arrived) > VIDEO_BUDGET)
    }

    fn finish(&mut self, id: u64, fate: Fate) {
        let Some(unit) = self
            .started
            .remove(&id)
            .or_else(|| self.waiting.remove(&id))
        else {
            return;
        };
        self.video_bytes -= unit.packets.iter().map(Bytes::len).sum::<usize>();
        self.sending.remove(&id);
        if !unit.stray {
            let key = (unit.media_id, unit.first_seq);
            if let Some(ids) = self.copies.get_mut(&key) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.copies.remove(&key);
                }
            }
        }
        if let Some(f) = self
            .history
            .get_mut(&unit.media_id)
            .and_then(|history| history.get_mut(&unit.first_seq))
            && *f == Fate::Queued(id)
        {
            *f = fate;
        }
    }

    /// Forgets the history of tracks that were quiet since the last call and have nothing
    /// queued, so a long-lived receiver keeps none for tracks that have ended.
    fn forget_quiet_tracks(&mut self) {
        let queued: HashSet<MediaId> = self.units().map(|u| u.media_id).collect();
        let kept = |id: &MediaId| self.active_tracks.contains(id) || queued.contains(id);
        self.history.retain(|id, _| kept(id));
        self.idr_tracks.retain(|id| kept(id));
        self.sent_keyframes.retain(|id, _| kept(id));
        let held = self.held.len();
        self.held.retain(|id, _| kept(id));
        self.held_changed |= self.held.len() != held;
        self.active_tracks.clear();
    }

    fn unit_mut(&mut self, id: u64) -> Option<&mut VideoUnit> {
        match self.started.get_mut(&id) {
            Some(unit) => Some(unit),
            None => self.waiting.get_mut(&id),
        }
    }

    fn units(&self) -> impl Iterator<Item = &VideoUnit> {
        self.started.values().chain(self.waiting.values())
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn count_drop(&mut self, media_id: MediaId) {
        self.stats.entry(media_id).or_default().dropped_units += 1;
    }

    pub fn pop(&mut self) -> Option<Bytes> {
        if let Some((media_id, bytes)) = self.audio.pop() {
            self.stats.entry(media_id).or_default().forwarded += 1;
            return Some(bytes);
        }
        let id = match self.sending.first() {
            Some(id) => *id,
            None => {
                let (id, mut unit) = self.waiting.pop_first()?;
                while self.strays.front().is_some_and(|stray| *stray <= id) {
                    self.strays.pop_front();
                }
                unit.started = true;
                if unit.keyframe {
                    let newest = self.sent_keyframes.entry(unit.media_id).or_default();
                    *newest = (*newest).max(unit.first_seq);
                }
                if !unit.stray && !unit.copy {
                    self.stats.entry(unit.media_id).or_default().forwarded += 1;
                }
                self.started.insert(id, unit);
                id
            }
        };
        let unit = self.started.get_mut(&id)?;
        let bytes = unit.packets.pop_front()?;
        self.video_bytes -= bytes.len();
        if !unit.packets.is_empty() {
            self.sending.insert(id);
        } else {
            self.sending.remove(&id);
            if unit.complete {
                self.finish(id, Fate::Sent);
            }
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
pub fn spawn(connection: quinn::Connection, control: mpsc::Sender<Outbound>) -> EgressHandle {
    let (inbox, mut rx) = mpsc::channel(INBOX);
    let handle = EgressHandle {
        inbox,
        inbox_drops: Arc::default(),
        held: Arc::default(),
    };
    let inbox_drops = handle.inbox_drops.clone();
    let held = handle.held.clone();
    tokio::spawn(async move {
        let mut queues = Queues::default();
        let mut next_stats = Instant::now() + RECEIVE_STATS_INTERVAL;
        // Swapped with the shared list each round, so neither side regrows from empty.
        let mut lost = Vec::new();
        loop {
            let overflow = {
                let mut drops = lock(&inbox_drops);
                std::mem::swap(&mut drops.lost, &mut lost);
                std::mem::take(&mut drops.overflow)
            };
            // A lost first fragment is placed before the drain, so the rest of its unit is
            // discarded on arrival. Other losses wait until the start of their unit is queued.
            for first in lost.iter().filter(|l| l.first) {
                queues.lose(*first);
            }
            while let Ok(packet) = rx.try_recv() {
                queues.push(packet, Instant::now());
            }
            for rest in lost.iter().filter(|l| !l.first) {
                queues.lose(*rest);
            }
            lost.clear();
            for (media_id, count) in overflow {
                queues.stats.entry(media_id).or_default().dropped_units += count;
                queues.hold_after_newest(media_id);
            }
            let now = Instant::now();
            queues.enforce_budget(now);
            if now >= next_stats {
                next_stats = now + RECEIVE_STATS_INTERVAL;
                queues.forget_quiet_tracks();
                if let Ok(permit) = control.try_reserve() {
                    let tracks = queues.take_stats();
                    if !tracks.is_empty() {
                        permit.send(Outbound::Message(ServerMessage::Event(
                            Event::ReceiveStats { tracks },
                        )));
                    }
                }
            }
            if let Some(tracks) = queues.take_held_change() {
                *lock(&held) = tracks;
            }
            match queues.pop() {
                // A datagram too large for this receiver's path is its own loss; only a
                // connection that is gone ends the task, which nothing would restart.
                Some(bytes) => match connection.send_datagram_wait(bytes).await {
                    Ok(()) | Err(quinn::SendDatagramError::TooLarge) => {}
                    Err(_) => return,
                },
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

    const VIDEO: TrackKind = TrackKind::Video(Recovery::Refresh);

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

    fn audio(media: u32, seq: u32) -> Packet {
        packet(TrackKind::Audio, media, seq, Flags::FIRST | Flags::LAST, 10)
    }

    fn fragment_flags(i: u32, count: u32) -> Flags {
        Flags::default()
            .with(Flags::FIRST, i == 0)
            .with(Flags::LAST, i == count - 1)
    }

    fn unit(queues: &mut Queues, media: u32, first_seq: u32, fragments: u32, now: Instant) {
        for i in 0..fragments {
            let flags = fragment_flags(i, fragments);
            queues.push(packet(VIDEO, media, first_seq + i, flags, 1000), now);
        }
    }

    fn drain(queues: &mut Queues) -> Vec<u8> {
        std::iter::from_fn(|| queues.pop()).map(|b| b[0]).collect()
    }

    #[test]
    fn audio_drains_before_video() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, now);
        queues.push(audio(2, 0), now);
        assert_eq!(queues.pop().unwrap().len(), 10);
        assert_eq!(queues.pop().unwrap().len(), 1000);
    }

    #[test]
    fn drops_strays_then_oldest_but_never_a_started_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, now);
        assert_eq!(drain(&mut queues), vec![0, 1]);
        let later = now + Duration::from_millis(100);
        unit(&mut queues, 1, 3, 3, now);
        unit(&mut queues, 1, 6, 3, now);
        queues.push(packet(VIDEO, 1, 2, Flags::default(), 1000), later);
        unit(&mut queues, 1, 9, 3, later);
        queues.pop();
        queues.enforce_budget(now + Duration::from_millis(200));
        assert_eq!(drain(&mut queues), vec![4, 5, 9, 10, 11]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn a_dropped_copy_counts_only_when_no_copy_is_left() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 3, now);
        unit(&mut queues, 1, 0, 3, now + Duration::from_millis(60));
        unit(&mut queues, 1, 3, 2, now + Duration::from_millis(100));
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
        unit(&mut queues, 1, 0, 2, now);
        assert_eq!(drain(&mut queues), vec![0, 1]);
        unit(&mut queues, 1, 0, 2, now);
        queues.enforce_budget(now + VIDEO_BUDGET + Duration::from_millis(1));
        assert_eq!(drain(&mut queues), Vec::<u8>::new());
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (1, 0));
    }

    #[test]
    fn a_copy_arriving_after_its_original_was_dropped_undoes_the_drop() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, now);
        let late = now + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(late);
        unit(&mut queues, 1, 0, 2, late);
        assert_eq!(drain(&mut queues), vec![0, 1]);
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (1, 0));
    }

    const IDR_VIDEO: TrackKind = TrackKind::Video(Recovery::Idr);

    /// Pushes a one-fragment unit of track 1 sent as `kind`, whose bytes are its `seq`.
    fn whole(queues: &mut Queues, kind: TrackKind, seq: u32, keyframe: bool, now: Instant) {
        let flags = (Flags::FIRST | Flags::LAST).with(Flags::KEYFRAME, keyframe);
        queues.push(packet(kind, 1, seq, flags, 1000), now);
    }

    /// Sends a keyframe and a unit, then has the next unit wait out the budget.
    fn drop_after_two(queues: &mut Queues, kind: TrackKind, now: Instant) -> Instant {
        whole(queues, kind, 0, true, now);
        whole(queues, kind, 1, false, now);
        assert_eq!(drain(queues), [0, 1]);
        whole(queues, kind, 2, false, now);
        let late = now + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(late);
        late
    }

    #[test]
    fn an_idr_track_is_held_from_a_drop_to_its_next_keyframe() {
        let mut queues = Queues::default();
        let late = drop_after_two(&mut queues, IDR_VIDEO, Instant::now());
        assert_eq!(queues.take_held_change(), Some(vec![MediaId(1)]));
        whole(&mut queues, IDR_VIDEO, 3, false, late);
        assert!(drain(&mut queues).is_empty());
        whole(&mut queues, IDR_VIDEO, 4, true, late);
        whole(&mut queues, IDR_VIDEO, 5, false, late);
        assert_eq!(drain(&mut queues), [4, 5]);
        assert_eq!(queues.take_held_change(), Some(vec![]));
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (4, 1));
    }

    #[test]
    fn a_hold_drops_the_deltas_queued_after_the_loss() {
        let now = Instant::now();
        let mut queues = Queues::default();
        whole(&mut queues, IDR_VIDEO, 0, true, now);
        assert_eq!(drain(&mut queues), [0]);
        whole(&mut queues, IDR_VIDEO, 1, false, now);
        whole(&mut queues, IDR_VIDEO, 2, false, now + VIDEO_BUDGET);
        queues.enforce_budget(now + VIDEO_BUDGET + Duration::from_millis(1));
        assert!(drain(&mut queues).is_empty());
        assert_eq!(queues.take_held_change(), Some(vec![MediaId(1)]));
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn a_keyframe_queued_after_the_loss_leaves_nothing_to_hold() {
        let now = Instant::now();
        let mut queues = Queues::default();
        whole(&mut queues, IDR_VIDEO, 1, false, now);
        whole(&mut queues, IDR_VIDEO, 2, false, now + VIDEO_BUDGET);
        whole(&mut queues, IDR_VIDEO, 3, true, now + VIDEO_BUDGET);
        queues.enforce_budget(now + VIDEO_BUDGET + Duration::from_millis(1));
        whole(&mut queues, IDR_VIDEO, 4, false, now + VIDEO_BUDGET);
        assert_eq!(drain(&mut queues), [3, 4]);
        assert_eq!(queues.take_held_change(), None);
    }

    /// Lost first fragments are placed before the inbox drains, so units from before the
    /// loss can come after it, a keyframe among them.
    #[test]
    fn units_from_before_the_loss_pass_and_their_keyframe_heals_nothing() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.lose(Lost {
            kind: IDR_VIDEO,
            media_id: MediaId(1),
            seq: 5,
            first: true,
        });
        whole(&mut queues, IDR_VIDEO, 3, true, now);
        whole(&mut queues, IDR_VIDEO, 4, false, now);
        whole(&mut queues, IDR_VIDEO, 6, false, now);
        assert_eq!(drain(&mut queues), [3, 4]);
        assert_eq!(queues.take_held_change(), Some(vec![MediaId(1)]));
        whole(&mut queues, IDR_VIDEO, 7, true, now);
        assert_eq!(drain(&mut queues), [7]);
    }

    #[test]
    fn a_unit_given_up_without_its_end_holds_its_track() {
        let now = Instant::now();
        let mut queues = Queues::default();
        let start = (Flags::FIRST).with(Flags::KEYFRAME, true);
        queues.push(packet(IDR_VIDEO, 1, 0, start, 1000), now);
        assert_eq!(drain(&mut queues), [0]);
        queues.enforce_budget(now + UNIT_IDLE + IDLE_CHECK);
        whole(&mut queues, IDR_VIDEO, 2, false, now + UNIT_IDLE);
        assert!(drain(&mut queues).is_empty());
        assert_eq!(queues.take_held_change(), Some(vec![MediaId(1)]));
    }

    #[test]
    fn a_refresh_track_is_never_held() {
        let mut queues = Queues::default();
        let late = drop_after_two(&mut queues, VIDEO, Instant::now());
        assert_eq!(queues.take_held_change(), None);
        whole(&mut queues, VIDEO, 3, false, late);
        assert_eq!(drain(&mut queues), [3]);
    }

    #[test]
    fn a_track_turning_to_refresh_is_let_go() {
        let mut queues = Queues::default();
        let late = drop_after_two(&mut queues, IDR_VIDEO, Instant::now());
        whole(&mut queues, VIDEO, 3, false, late);
        assert_eq!(drain(&mut queues), [3]);
        assert_eq!(queues.take_held_change(), Some(vec![]));
    }

    #[test]
    fn a_keyframe_copy_lets_go_of_the_hold_its_original_caused() {
        let now = Instant::now();
        let mut queues = Queues::default();
        whole(&mut queues, IDR_VIDEO, 0, true, now);
        let late = now + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(late);
        whole(&mut queues, IDR_VIDEO, 0, true, late);
        whole(&mut queues, IDR_VIDEO, 1, false, late);
        assert_eq!(drain(&mut queues), [0, 1]);
        let counters = &queues.stats[&MediaId(1)];
        assert_eq!((counters.forwarded, counters.dropped_units), (2, 0));
    }

    #[test]
    fn a_quiet_track_is_let_go() {
        let mut queues = Queues::default();
        drop_after_two(&mut queues, IDR_VIDEO, Instant::now());
        queues.take_held_change();
        queues.forget_quiet_tracks();
        queues.forget_quiet_tracks();
        assert_eq!(queues.take_held_change(), Some(vec![]));
    }

    #[test]
    fn a_keyframe_sent_after_a_unit_cut_short_leaves_nothing_to_hold() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(IDR_VIDEO, 1, 1, Flags::FIRST, 1000), now);
        whole(&mut queues, IDR_VIDEO, 2, true, now);
        assert_eq!(drain(&mut queues), [1, 2]);
        queues.enforce_budget(now + UNIT_IDLE + IDLE_CHECK);
        whole(&mut queues, IDR_VIDEO, 3, false, now + UNIT_IDLE);
        assert_eq!(drain(&mut queues), [3]);
        assert_eq!(queues.take_held_change(), None);
    }

    #[test]
    fn a_copy_of_a_unit_held_back_undoes_no_drop() {
        let mut queues = Queues::default();
        let late = drop_after_two(&mut queues, IDR_VIDEO, Instant::now());
        whole(&mut queues, IDR_VIDEO, 3, false, late);
        whole(&mut queues, IDR_VIDEO, 4, true, late);
        whole(&mut queues, IDR_VIDEO, 3, false, late);
        assert_eq!(drain(&mut queues), [4]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn losses_nobody_recorded_hold_from_past_the_newest_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        whole(&mut queues, IDR_VIDEO, 0, true, now);
        queues.hold_after_newest(MediaId(1));
        whole(&mut queues, IDR_VIDEO, 1, false, now);
        assert_eq!(drain(&mut queues), [0]);
        assert_eq!(queues.take_held_change(), Some(vec![MediaId(1)]));
    }

    #[test]
    fn fragments_of_a_dropped_unit_are_discarded() {
        let start = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 1000), start);
        let later = start + VIDEO_BUDGET + Duration::from_millis(1);
        queues.enforce_budget(later);
        queues.push(packet(VIDEO, 1, 1, Flags::LAST, 1000), later);
        assert!(queues.pop().is_none());
        unit(&mut queues, 1, 2, 1, later);
        assert_eq!(queues.pop().unwrap()[0], 2);
    }

    #[test]
    fn fragments_reordered_upstream_join_their_own_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in [0, 1, 3, 2, 4, 5] {
            let flags = fragment_flags(seq % 3, 3);
            queues.push(packet(VIDEO, 1, seq, flags, 1000), now);
        }
        assert_eq!(drain(&mut queues), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(queues.units().count(), 0);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 0);
    }

    #[test]
    fn a_late_fragment_of_a_sent_unit_is_still_forwarded() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 1000), now);
        queues.push(packet(VIDEO, 1, 2, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), vec![0, 2]);
        queues.push(packet(VIDEO, 1, 1, Flags::default(), 1000), now);
        assert_eq!(drain(&mut queues), vec![1]);
    }

    #[test]
    fn a_unit_whose_tail_never_arrives_is_given_up_after_a_second() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 1000), now);
        queues.pop();
        queues.enforce_budget(now + UNIT_IDLE + Duration::from_millis(1));
        assert_eq!(queues.units().count(), 0);
    }

    #[test]
    fn a_unit_over_the_fragment_limit_is_dropped_whole() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 10), now);
        assert_eq!(queues.pop().unwrap()[0], 0);
        let past_limit = MAX_UNIT_FRAGMENTS as u32 + 5;
        for seq in 1..past_limit {
            queues.push(packet(VIDEO, 1, seq, Flags::default(), 10), now);
        }
        queues.push(packet(VIDEO, 1, past_limit, Flags::LAST, 10), now);
        assert_eq!(queues.units().count(), 0);
        assert_eq!(queues.video_bytes, 0);
        assert!(queues.pop().is_none());
        queues.enforce_budget(now + UNIT_IDLE * 2);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn the_history_of_a_quiet_track_is_forgotten() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, now);
        drain(&mut queues);
        queues.push(packet(VIDEO, 2, 0, Flags::FIRST, 10), now);
        queues.forget_quiet_tracks();
        assert_eq!(queues.history.len(), 2);
        unit(&mut queues, 3, 0, 1, now);
        queues.forget_quiet_tracks();
        let mut kept: Vec<_> = queues.history.keys().copied().collect();
        kept.sort();
        assert_eq!(kept, vec![MediaId(2), MediaId(3)]);
        drain(&mut queues);
        queues.forget_quiet_tracks();
        assert_eq!(queues.history.len(), 1);
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
            queues.push(packet(VIDEO, 1, seq, flags, 1000), now);
        }
        queues.lose(lost(VIDEO, 1, 1, false));
        queues.lose(lost(VIDEO, 1, 3, false));
        queues.push(packet(VIDEO, 1, 4, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), Vec::<u8>::new());
        unit(&mut queues, 1, 5, 2, now);
        assert_eq!(drain(&mut queues), vec![5, 6]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
        assert_eq!(queues.video_bytes, 0);
    }

    #[test]
    fn a_lost_first_fragment_discards_the_fragments_after_it() {
        let now = Instant::now();
        let mut queues = Queues::default();
        unit(&mut queues, 1, 0, 2, now);
        queues.lose(lost(VIDEO, 1, 2, true));
        for (seq, flags) in [(3, Flags::default()), (4, Flags::LAST)] {
            queues.push(packet(VIDEO, 1, seq, flags, 1000), now);
        }
        assert_eq!(drain(&mut queues), vec![0, 1]);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
    }

    #[test]
    fn a_unit_sent_with_a_hole_counts_as_dropped_once() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 1000), now);
        queues.push(packet(VIDEO, 1, 2, Flags::LAST, 1000), now);
        assert_eq!(drain(&mut queues), vec![0, 2]);
        queues.lose(lost(VIDEO, 1, 1, false));
        queues.lose(lost(VIDEO, 1, 1, false));
        queues.lose(lost(TrackKind::Audio, 2, 0, true));
        let stats = queues.take_stats();
        assert_eq!(stats[0].dropped_units, 1);
        assert_eq!(stats[1].dropped_units, 1);
    }

    #[test]
    fn queued_video_bytes_are_capped_even_for_a_started_unit() {
        let now = Instant::now();
        let mut queues = Queues::default();
        queues.push(packet(VIDEO, 1, 0, Flags::FIRST, 4096), now);
        queues.pop();
        let fragments = (MAX_QUEUED_VIDEO_BYTES / 4096) as u32 + 10;
        for seq in 1..fragments {
            queues.push(packet(VIDEO, 1, seq, Flags::default(), 4096), now);
        }
        assert!(queues.video_bytes > MAX_QUEUED_VIDEO_BYTES);
        queues.enforce_budget(now);
        assert_eq!(queues.units().count(), 0);
        assert_eq!(queues.video_bytes, 0);
        assert_eq!(queues.stats[&MediaId(1)].dropped_units, 1);
        queues.push(packet(VIDEO, 1, fragments, Flags::LAST, 4096), now);
        assert!(queues.pop().is_none());
    }

    #[test]
    fn audio_tracks_take_turns() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in 0..3 {
            queues.push(audio(1, seq), now);
        }
        queues.push(audio(2, 7), now);
        assert_eq!(drain(&mut queues), vec![0, 7, 1, 2]);
    }

    #[test]
    fn a_flooding_audio_track_loses_only_its_own_packets_at_the_cap() {
        let now = Instant::now();
        let mut queues = Queues::default();
        let (flood, speaker) = (1, 2);
        for seq in 0..MAX_QUEUED_AUDIO as u32 * 3 {
            queues.push(audio(flood, seq), now);
            if seq % 100 == 0 {
                queues.push(audio(speaker, seq / 100), now);
            }
        }
        let mut heard = Vec::new();
        while let Some((media_id, bytes)) = queues.audio.pop() {
            if media_id == MediaId(speaker) {
                heard.push(bytes[0]);
            }
        }
        let spoken = (MAX_QUEUED_AUDIO * 3).div_ceil(100);
        assert_eq!(heard, (0..spoken as u8).collect::<Vec<u8>>());
        let stats = queues.take_stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].media_id, MediaId(flood));
        assert_eq!(
            stats[0].dropped_units as usize,
            MAX_QUEUED_AUDIO * 2 + spoken
        );
    }

    /// One second of a flood of the smallest units into a receiver that drains 20,000
    /// datagrams a second, after one unit was sent, so fragments that start no unit queue
    /// as strays. Every step must stay cheap however long the queue gets.
    fn flood(flags: Flags) -> Duration {
        let start = Instant::now();
        let mut queues = Queues::default();
        let first = Flags::FIRST | Flags::LAST;
        queues.push(packet(VIDEO, 1, 0, first, 1), start);
        queues.pop();
        let mut seq = 1;
        for ms in 0..1000 {
            let now = start + Duration::from_millis(ms);
            for _ in 0..200 {
                queues.push(packet(VIDEO, 1, seq, flags, 1), now);
                seq += 1;
            }
            for _ in 0..20 {
                queues.enforce_budget(now);
                queues.pop();
            }
        }
        start.elapsed()
    }

    #[test]
    fn a_flood_of_tiny_units_costs_little_per_unit() {
        for flags in [Flags::FIRST | Flags::LAST, Flags::default(), Flags::FIRST] {
            let took = flood(flags);
            assert!(took < Duration::from_secs(5), "{flags:?}: {took:?}");
        }
    }

    #[test]
    fn a_long_share_keeps_no_bookkeeping_for_units_already_sent() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for first_seq in (0..10_000).step_by(3) {
            unit(&mut queues, 1, first_seq, 2, now);
            drain(&mut queues);
            let late = packet(VIDEO, 1, first_seq + 2, Flags::default(), 1000);
            queues.push(late, now);
            drain(&mut queues);
        }
        assert!(queues.strays.is_empty());
        assert!(queues.copies.is_empty() && queues.sending.is_empty());
        assert_eq!(queues.units().count(), 0);
    }

    #[test]
    fn audio_is_never_dropped_by_the_budget() {
        let now = Instant::now();
        let mut queues = Queues::default();
        for seq in 0..500 {
            queues.push(audio(2, seq), now);
        }
        queues.enforce_budget(now + Duration::from_secs(5));
        assert_eq!(std::iter::from_fn(|| queues.pop()).count(), 500);
    }
}
