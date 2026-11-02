use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_crypto::hpke::EncKeyPair;
use throcc_crypto::media::{MediaError, TrackKey};
use throcc_proto::Fingerprint;
use throcc_proto::frame::{Flags, HEADER_BYTES, Header, TAG_BYTES};
use throcc_proto::ids::{Epoch, MediaId, RoomId, UserId};
use throcc_proto::messages::{Event, Placed, Recovery, Request};
use tokio::sync::mpsc;
use tracing::warn;

use super::fragment::{Reassembler, ReplayWindow, Seen, Unit, fragment};
use super::reorder::{Output, Reorder};
use crate::contacts::{Contacts, UnsavedPins};
use crate::keys::{EpochFloors, KeyManager, KeyNotice};
use crate::lock;

/// How long ciphertext waits for a sender key that opens it before it counts as a
/// decrypt failure.
pub const PENDING_KEY_WAIT: Duration = Duration::from_millis(500);
/// Video older than this when its turn comes is dropped whole, before it gets a `seq`.
pub const SEND_BUDGET: Duration = Duration::from_millis(100);
const TRACK_INBOX: usize = 1024;
const SEND_QUEUE: usize = 64;
const MAX_PENDING: usize = 512;
const TICK: Duration = Duration::from_millis(20);
/// Audio has no reorder stage, so a unit can complete before an earlier one; fragments
/// this far behind the newest unit are kept for that.
const AUDIO_STRAGGLER_SEQS: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
}

/// A decrypted access unit from a remote track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub timestamp: u32,
    pub flags: Flags,
    pub data: Vec<u8>,
}

impl From<Unit> for Received {
    fn from(unit: Unit) -> Self {
        Self {
            timestamp: unit.timestamp,
            flags: unit.flags,
            data: unit.data,
        }
    }
}

/// An access unit to send on one of our tracks.
pub struct Outgoing {
    pub media_id: MediaId,
    pub data: Bytes,
    pub timestamp: u32,
    pub keyframe: bool,
    pub enqueued: Instant,
}

impl Outgoing {
    /// A unit enqueued now.
    pub fn new(media_id: MediaId, data: Bytes, timestamp: u32, keyframe: bool) -> Self {
        Self {
            media_id,
            data,
            timestamp,
            keyframe,
            enqueued: Instant::now(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub duplicates: AtomicU64,
    pub too_old: AtomicU64,
    pub unknown_track: AtomicU64,
    pub malformed: AtomicU64,
    pub decrypt_failures: AtomicU64,
    pub track_overflow: AtomicU64,
    pub late_units: AtomicU64,
    pub abandoned_units: AtomicU64,
    pub send_queue_full: AtomicU64,
    pub send_dropped_units: AtomicU64,
    pub sent_datagrams: AtomicU64,
    /// Bytes sealed for sending on each of our tracks, counting each datagram once.
    pub sent_bytes: Mutex<HashMap<MediaId, u64>>,
}

/// Totals for one remote track, counting each datagram once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arrived {
    pub datagrams: u64,
    /// Datagrams that never arrived, from the track's replay window.
    pub lost: u64,
    pub bytes: u64,
}

pub struct KeyState {
    pub keys: KeyManager,
    pub contacts: Contacts,
}

/// One remote track's replay window, which outlives subscriptions until the room changes,
/// and its track task's inbox while subscribed.
#[derive(Default)]
struct Sink {
    /// Holds only datagrams that authenticated, so forged ones can't move it.
    window: ReplayWindow,
    inbox: Option<mpsc::Sender<Bytes>>,
    bytes: u64,
}

impl Sink {
    /// Records an authenticated datagram of `len` bytes, unless it was seen before.
    fn accept(&mut self, seq: u32, len: usize) -> Seen {
        let seen = self.window.check(seq);
        if seen == Seen::New {
            self.bytes += len as u64;
        }
        seen
    }
}

type Sinks = Arc<Mutex<HashMap<MediaId, Sink>>>;

/// Moves media between this client's tracks and the connection, end-to-end encrypted.
pub struct MediaEngine {
    keys: Arc<Mutex<KeyState>>,
    pins: Arc<UnsavedPins>,
    sinks: Sinks,
    audio: mpsc::Sender<Outgoing>,
    video: mpsc::Sender<Outgoing>,
    requests: mpsc::UnboundedSender<Request>,
    pub stats: Arc<Stats>,
}

impl MediaEngine {
    /// Starts the receive loop and the send task on `connection`. Returns the engine, the
    /// control requests it needs sent (sender keys, keyframe requests), and our tracks whose
    /// encoder must recover because the send queue dropped a reference frame.
    pub fn start(
        connection: quinn::Connection,
        me: UserId,
        enc: EncKeyPair,
        server: Fingerprint,
        contacts: Contacts,
        floors: EpochFloors,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<Request>,
        mpsc::Receiver<MediaId>,
    ) {
        let pins = contacts.unsaved();
        let keys = Arc::new(Mutex::new(KeyState {
            keys: KeyManager::new(me, enc, server, floors),
            contacts,
        }));
        let sinks: Sinks = Arc::default();
        let stats = Arc::new(Stats::default());
        let (requests, requests_rx) = mpsc::unbounded_channel();
        let (audio, audio_rx) = mpsc::channel(SEND_QUEUE);
        let (video, video_rx) = mpsc::channel(SEND_QUEUE);
        let (recovery_tx, recovery_rx) = mpsc::channel(SEND_QUEUE);

        tokio::spawn(receive_loop(
            connection.clone(),
            sinks.clone(),
            stats.clone(),
        ));
        tokio::spawn(send_loop(
            connection,
            keys.clone(),
            audio_rx,
            video_rx,
            recovery_tx,
            stats.clone(),
        ));
        let ticker = Arc::downgrade(&keys);
        tokio::spawn(async move {
            while let Some(keys) = ticker.upgrade() {
                lock(&keys).keys.tick(Instant::now());
                drop(keys);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });

        (
            Self {
                keys,
                pins,
                sinks,
                audio,
                video,
                requests,
                stats,
            },
            requests_rx,
            recovery_rx,
        )
    }

    pub fn keys(&self) -> MutexGuard<'_, KeyState> {
        lock(&self.keys)
    }

    /// Applies our placement after `SetRoom`.
    pub fn placed(&self, placed: &Placed) -> Vec<KeyNotice> {
        let (room_changed, (requests, notices)) = self.with_keys(|keys, contacts, now| {
            let room_changed = keys.room() != placed.room;
            let applied = keys.placed(
                placed.room,
                placed.epoch,
                placed.tracks.clone(),
                &placed.peers,
                contacts,
                now,
            );
            (room_changed, applied)
        });
        if room_changed {
            lock(&self.sinks).clear();
        }
        self.send_requests(requests);
        notices
    }

    /// Applies a control event that concerns media. Returns key warnings for the user.
    pub fn event(&self, event: &Event) -> Vec<KeyNotice> {
        match event {
            Event::RoomState { room, epoch, peers } => self
                .rekey(|keys, contacts, now| keys.room_state(*room, *epoch, peers, contacts, now)),
            Event::SenderKey {
                from,
                epoch,
                wrapped,
            } => {
                if let Err(e) = self
                    .keys()
                    .keys
                    .sender_key(*from, *epoch, wrapped, Instant::now())
                {
                    warn!(from = from.0, "rejected a sender key: {e}");
                }
                Vec::new()
            }
            Event::PresenceChanged(p) => self.set_presence(p.user, p.room),
            Event::PresenceEnded(user) => self.set_presence(*user, None),
            _ => Vec::new(),
        }
    }

    /// Notes where `user` is, and applies the room state again when that changes who is
    /// in our room.
    pub fn set_presence(&self, user: UserId, room: Option<RoomId>) -> Vec<KeyNotice> {
        self.rekey(|keys, contacts, now| {
            if keys.set_presence(user, room) {
                keys.reapply_room_state(contacts, now)
            } else {
                (None, Vec::new())
            }
        })
    }

    /// Applies `f` to the keys, then sends the request it makes.
    fn rekey(
        &self,
        f: impl FnOnce(&mut KeyManager, &mut Contacts, Instant) -> (Option<Request>, Vec<KeyNotice>),
    ) -> Vec<KeyNotice> {
        let (request, notices) = self.with_keys(f);
        self.send_requests(request);
        notices
    }

    /// Marks `user`'s pinned key as checked by us, or not.
    pub fn set_verified(&self, user: UserId, verified: bool) {
        self.with_keys(|_, contacts, _| contacts.set_verified(user, verified));
    }

    /// Notes the names pinned people are shown with.
    pub fn remember_names<'a>(&self, shown: impl IntoIterator<Item = (UserId, &'a str)>) {
        self.with_keys(|_, contacts, _| {
            for (user, name) in shown {
                contacts.remember_name(user, name);
            }
        });
    }

    /// Why pins stopped reaching the file, once per run of failed writes.
    pub fn pin_save_failure(&self) -> Option<String> {
        self.pins.take_failure()
    }

    /// Runs `f` on the key manager and contacts, holding the key lock only for it. Pins it
    /// changes are written afterwards, beside the async workers, so media never waits on
    /// the file.
    fn with_keys<R>(&self, f: impl FnOnce(&mut KeyManager, &mut Contacts, Instant) -> R) -> R {
        let result = {
            let mut state = self.keys();
            let KeyState { keys, contacts } = &mut *state;
            f(keys, contacts, Instant::now())
        };
        self.save_pins();
        result
    }

    /// Writes the pins not yet saved, if any, beside the async workers.
    pub fn save_pins(&self) {
        if !self.pins.is_empty() {
            let pins = self.pins.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = pins.save() {
                    warn!("could not save a contact: {e:#}");
                }
            });
        }
    }

    fn send_requests(&self, requests: impl IntoIterator<Item = Request>) {
        for request in requests {
            self.requests.send(request).ok();
        }
    }

    /// Starts receiving `media_id`. Decrypted units arrive on the returned channel.
    pub fn subscribe(
        &self,
        media_id: MediaId,
        kind: Kind,
        capacity: usize,
    ) -> mpsc::Receiver<Received> {
        let (inbox, inbox_rx) = mpsc::channel(TRACK_INBOX);
        let (out, out_rx) = mpsc::channel(capacity);
        lock(&self.sinks).entry(media_id).or_default().inbox = Some(inbox);
        let track = Track::new(
            media_id,
            kind,
            self.keys.clone(),
            self.sinks.clone(),
            self.stats.clone(),
            self.requests.clone(),
            out,
        );
        tokio::spawn(track.run(inbox_rx));
        out_rx
    }

    /// Asks the sender of `media_id` for a fresh keyframe each time it is called.
    pub fn idr_requester(&self, media_id: MediaId) -> impl Fn() + Send + 'static {
        let requests = self.requests.clone();
        move || {
            let request = Request::RequestKeyframe {
                media_id,
                kind: Recovery::Idr,
            };
            requests.send(request).ok();
        }
    }

    pub fn unsubscribe(&self, media_id: MediaId) {
        if let Some(sink) = lock(&self.sinks).get_mut(&media_id) {
            sink.inbox = None;
        }
    }

    /// What has arrived on `media_id` so far.
    pub fn arrived(&self, media_id: MediaId) -> Arrived {
        lock(&self.sinks)
            .get(&media_id)
            .map_or(Arrived::default(), |s| Arrived {
                datagrams: s.window.received(),
                lost: s.window.lost(),
                bytes: s.bytes,
            })
    }

    /// Bytes sealed for sending on our track `media_id` so far.
    pub fn sent_bytes(&self, media_id: MediaId) -> u64 {
        lock(&self.stats.sent_bytes)
            .get(&media_id)
            .copied()
            .unwrap_or(0)
    }

    /// Queues a unit for sending. Never waits; a full queue drops the unit and counts it.
    pub fn send(&self, kind: Kind, unit: Outgoing) {
        let queue = match kind {
            Kind::Audio => &self.audio,
            Kind::Video => &self.video,
        };
        if queue.try_send(unit).is_err() {
            self.stats.send_queue_full.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn receive_loop(connection: quinn::Connection, sinks: Sinks, stats: Arc<Stats>) {
    while let Ok(datagram) = connection.read_datagram().await {
        let Ok(header) = Header::parse_strict(&datagram) else {
            stats.malformed.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let mut sinks = lock(&sinks);
        let Some(sink) = sinks.get_mut(&header.media_id) else {
            stats.unknown_track.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let Some(inbox) = &sink.inbox else {
            stats.unknown_track.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        match sink.window.peek(header.seq) {
            Seen::New => match inbox.try_send(datagram) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    stats.track_overflow.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    sink.inbox = None;
                    stats.unknown_track.fetch_add(1, Ordering::Relaxed);
                }
            },
            seen => stats.count(seen),
        }
    }
}

impl Stats {
    fn count(&self, seen: Seen) {
        match seen {
            Seen::New => {}
            Seen::Duplicate => {
                self.duplicates.fetch_add(1, Ordering::Relaxed);
            }
            Seen::TooOld => {
                self.too_old.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Decrypts, reassembles and reorders one remote track. A stalled consumer starves only it.
struct Track {
    media_id: MediaId,
    kind: Kind,
    keys: Arc<Mutex<KeyState>>,
    sinks: Sinks,
    stats: Arc<Stats>,
    requests: mpsc::UnboundedSender<Request>,
    out: mpsc::Sender<Received>,
    /// Datagrams no key we hold opens yet, oldest first, with when they arrived.
    pending: VecDeque<(Instant, Epoch, Bytes)>,
    next_tick: Option<Instant>,
    /// The key generation the pending datagrams were last tried against.
    tried_generation: u64,
    /// Where datagrams are decrypted; the reassembler copies out what it keeps.
    scratch: Vec<u8>,
    reassembler: Reassembler,
    reorder: Reorder,
    last_unit_overflowed: bool,
}

impl Track {
    fn new(
        media_id: MediaId,
        kind: Kind,
        keys: Arc<Mutex<KeyState>>,
        sinks: Sinks,
        stats: Arc<Stats>,
        requests: mpsc::UnboundedSender<Request>,
        out: mpsc::Sender<Received>,
    ) -> Self {
        Self {
            media_id,
            kind,
            keys,
            sinks,
            stats,
            requests,
            out,
            pending: VecDeque::new(),
            next_tick: None,
            tried_generation: 0,
            scratch: Vec::new(),
            last_unit_overflowed: false,
            reassembler: Reassembler::default(),
            reorder: Reorder::default(),
        }
    }

    async fn run(mut self, mut inbox: mpsc::Receiver<Bytes>) {
        loop {
            let wake = self.next_wake();
            let datagram = match wake {
                Some(at) => match tokio::time::timeout_at(at.into(), inbox.recv()).await {
                    Ok(next) => next,
                    Err(_) => {
                        self.tick(Instant::now());
                        continue;
                    }
                },
                None => inbox.recv().await,
            };
            let Some(datagram) = datagram else { return };
            if self.out.is_closed() {
                return;
            }
            self.receive(datagram, Instant::now());
        }
    }

    /// Handles a datagram that just arrived, after any pending ones a new key opens, so
    /// a unit that waited for its key isn't overtaken by the next one.
    fn receive(&mut self, datagram: Bytes, now: Instant) {
        self.retry_pending(now);
        if let Ok(header) = Header::parse_strict(&datagram) {
            self.handle(header.epoch, datagram, now, now);
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        self.next_tick
            .into_iter()
            .chain(self.reorder.deadline())
            .min()
    }

    /// Expires pending datagrams, and retries the rest only if new keys have arrived.
    fn tick(&mut self, now: Instant) {
        let expired = self
            .pending
            .iter()
            .take_while(|(arrived, ..)| now.duration_since(*arrived) >= PENDING_KEY_WAIT)
            .count();
        self.pending.drain(..expired);
        self.stats
            .decrypt_failures
            .fetch_add(expired as u64, Ordering::Relaxed);
        self.retry_pending(now);
        self.next_tick = (!self.pending.is_empty()).then(|| now + TICK);
        let outputs = self.reorder.poll(now);
        self.emit(outputs);
    }

    /// Tries the pending datagrams again, oldest first, if keys changed since the last try.
    fn retry_pending(&mut self, now: Instant) {
        if self.pending.is_empty() {
            return;
        }
        let generation = lock(&self.keys).keys.generation();
        if generation == self.tried_generation {
            return;
        }
        self.tried_generation = generation;
        for (arrived, epoch, datagram) in std::mem::take(&mut self.pending) {
            self.handle(epoch, datagram, arrived, now);
        }
    }

    fn handle(&mut self, epoch: Epoch, datagram: Bytes, arrived: Instant, now: Instant) {
        let key = lock(&self.keys).keys.recv_key(self.media_id, epoch);
        self.scratch.clear();
        self.scratch.extend_from_slice(&datagram);
        let (header, plaintext) = match key.map(|key| key.open(&mut self.scratch)) {
            Some(Ok(opened)) => opened,
            // A sender that replaces its secret within an epoch seals with the new one
            // about a round trip before the new one reaches us, so a datagram the key we
            // hold doesn't open waits like one we have no key for.
            None | Some(Err(MediaError::Auth)) => {
                self.hold(arrived, epoch, datagram, now);
                return;
            }
            Some(Err(_)) => {
                self.stats.decrypt_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let seen = lock(&self.sinks)
            .get_mut(&self.media_id)
            .map(|sink| sink.accept(header.seq, datagram.len()));
        match seen {
            Some(Seen::New) => {}
            Some(seen) => {
                self.stats.count(seen);
                return;
            }
            None => return,
        }
        let Some(unit) = self.reassembler.push(header.seq, header.flags, plaintext) else {
            return;
        };
        match self.kind {
            Kind::Audio => {
                let stragglers = unit.first_seq.saturating_sub(AUDIO_STRAGGLER_SEQS);
                self.reassembler.forget_before(stragglers);
                self.deliver(unit.into());
            }
            Kind::Video => {
                let outputs = self.reorder.push(unit, now);
                self.emit(outputs);
            }
        }
    }

    fn hold(&mut self, arrived: Instant, epoch: Epoch, datagram: Bytes, now: Instant) {
        if self.pending.len() < MAX_PENDING {
            self.pending.push_back((arrived, epoch, datagram));
            self.next_tick.get_or_insert(now + TICK);
        } else {
            self.stats.decrypt_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn emit(&mut self, outputs: Vec<Output>) {
        for output in outputs {
            match output {
                Output::Unit(unit) => {
                    self.reassembler.forget_before(unit.first_seq);
                    self.deliver(unit.into());
                }
                Output::Abandoned { .. } => {
                    self.stats.abandoned_units.fetch_add(1, Ordering::Relaxed);
                    self.ask_refresh();
                }
                Output::Late => {
                    self.stats.late_units.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn ask_refresh(&self) {
        let request = Request::RequestKeyframe {
            media_id: self.media_id,
            kind: Recovery::Refresh,
        };
        self.requests.send(request).ok();
    }

    /// Passes a unit to the consumer. The first video unit dropped in a run of full-channel
    /// drops requests a refresh from the sender.
    fn deliver(&mut self, received: Received) {
        match self.out.try_send(received) {
            Ok(()) => self.last_unit_overflowed = false,
            Err(e) => {
                self.stats.track_overflow.fetch_add(1, Ordering::Relaxed);
                let full = matches!(e, mpsc::error::TrySendError::Full(_));
                if full && self.kind == Kind::Video && !self.last_unit_overflowed {
                    self.ask_refresh();
                }
                self.last_unit_overflowed = full;
            }
        }
    }
}

/// Assigns each fragment the next `seq` of its track and seals it under the track's
/// current key. `seq` never resets for a `media_id`, so a nonce never repeats under a key.
#[derive(Default)]
pub struct Sealer {
    seqs: HashMap<MediaId, u32>,
}

impl Sealer {
    /// Seals one access unit into datagrams. Stops early if the track's `seq` is exhausted.
    pub fn seal(&mut self, unit: &Outgoing, epoch: Epoch, key: &TrackKey) -> Vec<Bytes> {
        let fragments = fragment(&unit.data, unit.timestamp, unit.keyframe);
        let mut datagrams = Vec::with_capacity(fragments.len());
        let seq = self.seqs.entry(unit.media_id).or_insert(0);
        for fragment in fragments {
            let Some(next) = seq.checked_add(1) else {
                warn!(
                    media_id = unit.media_id.0,
                    "sequence numbers exhausted; the track stops"
                );
                break;
            };
            let header = Header {
                media_id: unit.media_id,
                epoch,
                seq: *seq,
                flags: fragment.flags,
            };
            *seq = next;
            let parts = fragment.plaintext_parts();
            let len = parts.iter().map(|p| p.len()).sum::<usize>();
            let mut sealed = Vec::with_capacity(HEADER_BYTES + len + TAG_BYTES);
            if key.seal(&header, &parts, &mut sealed).is_ok() {
                datagrams.push(Bytes::from(sealed));
            }
        }
        datagrams
    }
}

/// Keyframes are sent this many times over, whole unit after whole unit. The receiver's
/// replay window keeps the first copy of each fragment, so a lost fragment is filled by a copy.
const KEYFRAME_COPIES: usize = 2;

async fn send_loop(
    connection: quinn::Connection,
    keys: Arc<Mutex<KeyState>>,
    mut audio: mpsc::Receiver<Outgoing>,
    mut video: mpsc::Receiver<Outgoing>,
    recovery: mpsc::Sender<MediaId>,
    stats: Arc<Stats>,
) {
    let mut sealer = Sealer::default();
    loop {
        let unit = tokio::select! {
            biased;
            Some(voice) = audio.recv() => {
                if !send_unit(&connection, &keys, &mut sealer, &voice, &stats).await {
                    return;
                }
                continue;
            }
            Some(unit) = video.recv() => unit,
            else => return,
        };
        if unit.enqueued.elapsed() > SEND_BUDGET {
            stats.send_dropped_units.fetch_add(1, Ordering::Relaxed);
            recovery.try_send(unit.media_id).ok();
            continue;
        }
        let Some(datagrams) = seal(&keys, &mut sealer, &unit, &stats) else {
            continue;
        };
        let copies = if unit.keyframe { KEYFRAME_COPIES } else { 1 };
        let originals = datagrams.len();
        for (sent, datagram) in datagrams
            .iter()
            .cycle()
            .take(originals * copies)
            .enumerate()
        {
            if sent >= originals && copy_would_delay(&unit, &video) {
                break;
            }
            while let Ok(voice) = audio.try_recv() {
                if !send_unit(&connection, &keys, &mut sealer, &voice, &stats).await {
                    return;
                }
            }
            if !send_datagram(&connection, datagram, &stats).await {
                return;
            }
        }
    }
}

/// A keyframe's copy stops once a newer frame is waiting and the copy would eat into its
/// send budget, so repairing one frame never makes the sender drop the next.
fn copy_would_delay(unit: &Outgoing, video: &mpsc::Receiver<Outgoing>) -> bool {
    !video.is_empty() && unit.enqueued.elapsed() > SEND_BUDGET / 2
}

fn seal(
    keys: &Mutex<KeyState>,
    sealer: &mut Sealer,
    unit: &Outgoing,
    stats: &Stats,
) -> Option<Vec<Bytes>> {
    let (epoch, key) = lock(keys).keys.send_key(unit.media_id)?;
    let datagrams = sealer.seal(unit, epoch, &key);
    let bytes: usize = datagrams.iter().map(Bytes::len).sum();
    *lock(&stats.sent_bytes).entry(unit.media_id).or_default() += bytes as u64;
    Some(datagrams)
}

/// Seals and sends one unit. False once the connection is gone.
async fn send_unit(
    connection: &quinn::Connection,
    keys: &Mutex<KeyState>,
    sealer: &mut Sealer,
    unit: &Outgoing,
    stats: &Stats,
) -> bool {
    for datagram in seal(keys, sealer, unit, stats).unwrap_or_default() {
        if !send_datagram(connection, &datagram, stats).await {
            return false;
        }
    }
    true
}

async fn send_datagram(connection: &quinn::Connection, datagram: &Bytes, stats: &Stats) -> bool {
    if connection
        .send_datagram_wait(datagram.clone())
        .await
        .is_err()
    {
        return false;
    }
    stats.sent_datagrams.fetch_add(1, Ordering::Relaxed);
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;
    use throcc_crypto::identity::random;

    use super::*;
    use crate::keys::tests::{Member, join_in_server_order, member, rekey, wrapped_for};
    use crate::media::fragment::WINDOW_BITS;

    /// A track task for `media_id` on `receiver`'s keys, and what it delivers.
    fn track(receiver: Member, media_id: MediaId, kind: Kind) -> (Track, mpsc::Receiver<Received>) {
        let keys = Arc::new(Mutex::new(KeyState {
            keys: receiver.keys,
            contacts: receiver.contacts,
        }));
        let sinks = Arc::new(Mutex::new(HashMap::from([(media_id, Sink::default())])));
        let (requests, _) = mpsc::unbounded_channel();
        let (out, out_rx) = mpsc::channel(16);
        let track = Track::new(media_id, kind, keys, sinks, Arc::default(), requests, out);
        (track, out_rx)
    }

    /// `sender`'s unit on `media_id`, sealed under its current key.
    fn sealed(
        sender: &Member,
        sealer: &mut Sealer,
        media_id: MediaId,
        len: usize,
        timestamp: u32,
        keyframe: bool,
    ) -> Vec<Bytes> {
        let (epoch, key) = sender.keys.send_key(media_id).unwrap();
        let unit = Outgoing::new(
            media_id,
            Bytes::from(vec![timestamp as u8; len]),
            timestamp,
            keyframe,
        );
        sealer.seal(&unit, epoch, &key)
    }

    fn timestamps(out: &mut mpsc::Receiver<Received>) -> Vec<u32> {
        std::iter::from_fn(|| out.try_recv().ok())
            .map(|r| r.timestamp)
            .collect()
    }

    /// With the server's event order, a member already in the room sends under its second
    /// secret of the epoch while the others still hold its first.
    #[test]
    fn media_sealed_under_a_secret_replaced_within_the_epoch_waits_for_it() {
        let now = Instant::now();
        let (mut a, mut b, mut n) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);
        let published = join_in_server_order(&mut [&mut a, &mut b], &mut n, 2, now);
        let mut from_a = published.iter().filter(|(from, _)| *from == a.id);
        let first = wrapped_for(&from_a.next().unwrap().1, b.id).unwrap();
        let second = wrapped_for(&from_a.next().unwrap().1, b.id).unwrap();
        b.keys.sender_key(a.id, first.0, &first.1, now).unwrap();

        let mic = a.tracks.mic;
        let mut sealer = Sealer::default();
        let early = sealed(&a, &mut sealer, mic, 100, 1, false);
        let later = sealed(&a, &mut sealer, mic, 100, 2, false);
        let (mut track, mut out) = track(b, mic, Kind::Audio);
        for datagram in early {
            track.receive(datagram, now);
        }
        assert!(timestamps(&mut out).is_empty());

        lock(&track.keys)
            .keys
            .sender_key(a.id, second.0, &second.1, now)
            .unwrap();
        for datagram in later {
            track.receive(datagram, now);
        }
        assert_eq!(timestamps(&mut out), [1, 2]);
        assert_eq!(track.stats.decrypt_failures.load(Ordering::Relaxed), 0);
    }

    /// A new subscription's first keyframe waits for the key; the unit after it must not
    /// overtake it once the key is there.
    #[test]
    fn units_waiting_for_their_key_go_out_before_newer_ones() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let mut n = member(3);
        let published = join_in_server_order(&mut [&mut a, &mut b], &mut n, 2, now);
        let (_, request) = published.iter().rfind(|(from, _)| *from == a.id).unwrap();
        let key = wrapped_for(request, n.id).unwrap();

        let video = a.tracks.shares[0].video;
        let mut sealer = Sealer::default();
        let keyframe = sealed(&a, &mut sealer, video, 3000, 1, true);
        let delta = sealed(&a, &mut sealer, video, 100, 2, false);
        let (mut track, mut out) = track(n, video, Kind::Video);
        for datagram in keyframe {
            track.receive(datagram, now);
        }
        lock(&track.keys)
            .keys
            .sender_key(a.id, key.0, &key.1, now)
            .unwrap();
        for datagram in delta {
            track.receive(datagram, now);
        }
        let received: Vec<(u32, bool)> = std::iter::from_fn(|| out.try_recv().ok())
            .map(|r| (r.timestamp, r.flags.contains(Flags::KEYFRAME)))
            .collect();
        assert_eq!(received, [(1, true), (2, false)]);
        assert_eq!(track.stats.late_units.load(Ordering::Relaxed), 0);
    }

    /// Only datagrams that authenticate move the replay window, so a forged one can't
    /// shut out the real datagram with its `seq`, or every later one.
    #[test]
    fn a_forged_datagram_does_not_move_the_replay_window() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let mic = a.tracks.mic;
        let mut sealer = Sealer::default();
        let first = sealed(&a, &mut sealer, mic, 100, 1, false).remove(0);
        let second = sealed(&a, &mut sealer, mic, 100, 2, false).remove(0);
        let (mut track, mut out) = track(b, mic, Kind::Audio);
        for seq in [1, WINDOW_BITS * 4] {
            let mut forged = first.to_vec();
            forged[8..12].copy_from_slice(&seq.to_be_bytes());
            track.receive(Bytes::from(forged), now);
        }
        track.tick(now + PENDING_KEY_WAIT);
        assert_eq!(track.stats.decrypt_failures.load(Ordering::Relaxed), 2);

        for datagram in [&second, &second, &first] {
            track.receive(datagram.clone(), now);
        }
        assert_eq!(timestamps(&mut out), [2, 1]);
        assert_eq!(track.stats.duplicates.load(Ordering::Relaxed), 1);
        let sinks = lock(&track.sinks);
        let window = &sinks[&mic].window;
        assert_eq!((window.received(), window.lost()), (2, 0));
    }

    proptest! {
        /// Across epoch changes and tracks, (media_id, epoch, seq) never repeats under one key.
        #[test]
        fn nonce_never_repeats_under_one_key(steps in proptest::collection::vec((0u32..3, 0usize..4000, any::<bool>()), 1..200)) {
            let mut sealer = Sealer::default();
            let mut epoch = Epoch(1);
            let mut keys: HashMap<(MediaId, Epoch), (usize, TrackKey)> = HashMap::new();
            let mut seen = HashSet::new();
            for (track, len, rekey) in steps {
                if rekey {
                    epoch = Epoch(epoch.0 + 1);
                }
                let media_id = MediaId(track);
                let next_id = keys.len();
                let (key_id, key) = keys
                    .entry((media_id, epoch))
                    .or_insert_with(|| (next_id, TrackKey::derive(&random(), media_id)));
                let unit = Outgoing {
                    media_id,
                    data: Bytes::from(vec![0u8; len]),
                    timestamp: 0,
                    keyframe: false,
                    enqueued: Instant::now(),
                };
                for datagram in sealer.seal(&unit, epoch, key) {
                    let header = Header::parse_strict(&datagram).unwrap();
                    prop_assert_eq!((header.media_id, header.epoch), (media_id, epoch));
                    prop_assert!(seen.insert((*key_id, header.nonce())), "nonce reused");
                }
            }
        }
    }
}
