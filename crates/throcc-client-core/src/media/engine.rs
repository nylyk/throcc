use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_crypto::hpke::EncKeyPair;
use throcc_crypto::media::TrackKey;
use throcc_proto::Fingerprint;
use throcc_proto::frame::{Flags, Header};
use throcc_proto::ids::{Epoch, MediaId, UserId};
use throcc_proto::messages::{Event, Placed, Recovery, Request};
use tokio::sync::mpsc;
use tracing::warn;

use super::fragment::{Reassembler, ReplayWindow, Seen, Unit, fragment};
use super::reorder::{Output, Reorder};
use crate::contacts::Contacts;
use crate::keys::{KeyManager, KeyNotice};
use crate::lock;

/// How long ciphertext waits for its sender key before it counts as a decrypt failure.
pub const PENDING_KEY_WAIT: Duration = Duration::from_millis(500);
/// Video older than this when its turn comes is dropped whole, before it gets a `seq`.
pub const SEND_BUDGET: Duration = Duration::from_millis(100);
const TRACK_INBOX: usize = 1024;
const SEND_QUEUE: usize = 64;
const MAX_PENDING: usize = 512;
const TICK: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
}

/// A decrypted access unit from a remote track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub seq: u32,
    pub timestamp: u32,
    pub flags: Flags,
    pub data: Vec<u8>,
}

/// An access unit to send on one of our tracks.
pub struct Outgoing {
    pub media_id: MediaId,
    pub data: Bytes,
    pub timestamp: u32,
    pub keyframe: bool,
    pub discardable: bool,
    pub enqueued: Instant,
}

impl Outgoing {
    /// A unit that must not be discarded, enqueued now.
    pub fn new(media_id: MediaId, data: Bytes, timestamp: u32, keyframe: bool) -> Self {
        Self {
            media_id,
            data,
            timestamp,
            keyframe,
            discardable: false,
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

pub struct KeyState {
    pub keys: KeyManager,
    pub contacts: Contacts,
}

/// One remote track's replay window, which outlives subscriptions until the room changes,
/// and its track task's inbox while subscribed.
#[derive(Default)]
struct Sink {
    window: ReplayWindow,
    inbox: Option<mpsc::Sender<Bytes>>,
    bytes: u64,
}

/// Moves media between this client's tracks and the connection, end-to-end encrypted.
pub struct MediaEngine {
    keys: Arc<Mutex<KeyState>>,
    sinks: Arc<Mutex<HashMap<MediaId, Sink>>>,
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
    ) -> (
        Self,
        mpsc::UnboundedReceiver<Request>,
        mpsc::Receiver<MediaId>,
    ) {
        let keys = Arc::new(Mutex::new(KeyState {
            keys: KeyManager::new(me, enc, server),
            contacts,
        }));
        let sinks: Arc<Mutex<HashMap<MediaId, Sink>>> = Arc::default();
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
        let mut state = self.keys();
        let KeyState { keys, contacts } = &mut *state;
        let room_changed = keys.room() != placed.room;
        let (requests, notices) = keys.placed(
            placed.room,
            placed.epoch,
            placed.tracks.clone(),
            &placed.peers,
            contacts,
            Instant::now(),
        );
        drop(state);
        if room_changed {
            lock(&self.sinks).clear();
        }
        for request in requests {
            self.send_request(Some(request));
        }
        notices
    }

    /// Applies a control event that concerns media. Returns key warnings for the user.
    pub fn event(&self, event: &Event) -> Vec<KeyNotice> {
        match event {
            Event::RoomState { room, epoch, peers } => {
                let mut state = self.keys();
                let KeyState { keys, contacts } = &mut *state;
                let (request, notices) =
                    keys.room_state(*room, *epoch, peers, contacts, Instant::now());
                drop(state);
                self.send_request(request);
                notices
            }
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
            _ => Vec::new(),
        }
    }

    fn send_request(&self, request: Option<Request>) {
        if let Some(request) = request {
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
        let track = Track {
            media_id,
            kind,
            keys: self.keys.clone(),
            stats: self.stats.clone(),
            requests: self.requests.clone(),
            out,
            pending: VecDeque::new(),
            tried_generation: 0,
            reassembler: Reassembler::default(),
            reorder: Reorder::default(),
        };
        tokio::spawn(track.run(inbox_rx));
        out_rx
    }

    pub fn unsubscribe(&self, media_id: MediaId) {
        if let Some(sink) = lock(&self.sinks).get_mut(&media_id) {
            sink.inbox = None;
        }
    }

    /// Datagrams that never arrived on `media_id`, from its replay window.
    pub fn lost(&self, media_id: MediaId) -> u64 {
        lock(&self.sinks)
            .get(&media_id)
            .map_or(0, |s| s.window.lost())
    }

    /// Datagrams received and lost on `media_id` so far.
    pub fn datagrams(&self, media_id: MediaId) -> (u64, u64) {
        lock(&self.sinks)
            .get(&media_id)
            .map_or((0, 0), |s| (s.window.received(), s.window.lost()))
    }

    /// Bytes received on `media_id` so far, counting each datagram once.
    pub fn received_bytes(&self, media_id: MediaId) -> u64 {
        lock(&self.sinks).get(&media_id).map_or(0, |s| s.bytes)
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

async fn receive_loop(
    connection: quinn::Connection,
    sinks: Arc<Mutex<HashMap<MediaId, Sink>>>,
    stats: Arc<Stats>,
) {
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
        match sink.window.check(header.seq) {
            Seen::New => {
                sink.bytes += datagram.len() as u64;
                if inbox.try_send(datagram).is_err() {
                    stats.track_overflow.fetch_add(1, Ordering::Relaxed);
                }
            }
            Seen::Duplicate => {
                stats.duplicates.fetch_add(1, Ordering::Relaxed);
            }
            Seen::TooOld => {
                stats.too_old.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Decrypts, reassembles and reorders one remote track. A stalled consumer starves only it.
struct Track {
    media_id: MediaId,
    kind: Kind,
    keys: Arc<Mutex<KeyState>>,
    stats: Arc<Stats>,
    requests: mpsc::UnboundedSender<Request>,
    out: mpsc::Sender<Received>,
    pending: VecDeque<(Instant, Epoch, Bytes)>,
    /// The key generation the pending datagrams were last tried against.
    tried_generation: u64,
    reassembler: Reassembler,
    reorder: Reorder,
}

impl Track {
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
            let now = Instant::now();
            if let Ok(header) = Header::parse_strict(&datagram) {
                self.handle(header.epoch, datagram, now, now);
            }
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let pending = self.pending.front().map(|_| Instant::now() + TICK);
        pending.into_iter().chain(self.reorder.deadline()).min()
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
        let generation = lock(&self.keys).keys.generation();
        if generation != self.tried_generation && !self.pending.is_empty() {
            self.tried_generation = generation;
            for (arrived, epoch, datagram) in std::mem::take(&mut self.pending) {
                self.handle(epoch, datagram, arrived, now);
            }
        }
        let outputs = self.reorder.poll(now);
        self.emit(outputs);
    }

    fn handle(&mut self, epoch: Epoch, datagram: Bytes, arrived: Instant, now: Instant) {
        let key = lock(&self.keys).keys.recv_key(self.media_id, epoch);
        let Some(key) = key else {
            if self.pending.len() < MAX_PENDING {
                self.pending.push_back((arrived, epoch, datagram));
            } else {
                self.stats.decrypt_failures.fetch_add(1, Ordering::Relaxed);
            }
            return;
        };
        let mut buf = datagram.to_vec();
        let Ok((header, plaintext)) = key.open(&mut buf) else {
            self.stats.decrypt_failures.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match self.kind {
            Kind::Audio => {
                let Some((ts, data)) = plaintext.split_first_chunk::<4>() else {
                    return;
                };
                let received = Received {
                    seq: header.seq,
                    timestamp: u32::from_be_bytes(*ts),
                    flags: header.flags,
                    data: data.to_vec(),
                };
                self.deliver(received);
            }
            Kind::Video => {
                if let Some(unit) = self.reassembler.push(header.seq, header.flags, plaintext) {
                    let outputs = self.reorder.push(unit, now);
                    self.emit(outputs);
                }
            }
        }
    }

    fn emit(&mut self, outputs: Vec<Output>) {
        for output in outputs {
            match output {
                Output::Unit(Unit {
                    first_seq,
                    timestamp,
                    flags,
                    data,
                    ..
                }) => {
                    self.reassembler.forget_before(first_seq);
                    self.deliver(Received {
                        seq: first_seq,
                        timestamp,
                        flags,
                        data,
                    });
                }
                Output::Abandoned { .. } => {
                    self.stats.abandoned_units.fetch_add(1, Ordering::Relaxed);
                    let request = Request::RequestKeyframe {
                        media_id: self.media_id,
                        kind: Recovery::Refresh,
                    };
                    self.requests.send(request).ok();
                }
            }
        }
        self.stats
            .late_units
            .store(self.reorder.late, Ordering::Relaxed);
    }

    fn deliver(&self, received: Received) {
        if self.out.try_send(received).is_err() {
            self.stats.track_overflow.fetch_add(1, Ordering::Relaxed);
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
        let mut datagrams = Vec::new();
        let mut buf = Vec::new();
        for fragment in fragment(&unit.data, unit.timestamp, unit.keyframe, unit.discardable) {
            let seq = self.seqs.entry(unit.media_id).or_insert(0);
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
            if key
                .seal(&header, &fragment.plaintext_parts(), &mut buf)
                .is_ok()
            {
                datagrams.push(Bytes::copy_from_slice(&buf));
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
        let (kind, unit) = tokio::select! {
            biased;
            Some(unit) = audio.recv() => (Kind::Audio, unit),
            Some(unit) = video.recv() => (Kind::Video, unit),
            else => return,
        };
        if kind == Kind::Audio {
            if !send_unit(&connection, &keys, &mut sealer, &unit, &stats).await {
                return;
            }
            continue;
        }
        if unit.enqueued.elapsed() > SEND_BUDGET {
            stats.send_dropped_units.fetch_add(1, Ordering::Relaxed);
            if !unit.discardable {
                recovery.try_send(unit.media_id).ok();
            }
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
                    discardable: false,
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
