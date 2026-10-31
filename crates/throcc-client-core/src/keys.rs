use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use throcc_crypto::hpke::{self, EncKeyPair, WrapContext};
use throcc_crypto::identity::{self, random};
use throcc_crypto::media::TrackKey;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, MediaId, RoomId, UserId};
use throcc_proto::messages::{PublicKey, Request, RoomPeer, Tracks};
use tracing::warn;
use zeroize::Zeroizing;

use crate::contacts::{ContactStatus, Contacts};

/// How long a sender's previous epoch key stays usable after it moves on.
pub const OLD_KEY_GRACE: Duration = Duration::from_secs(2);
const MAX_EARLY: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyNotice {
    /// A user's identity key differs from the pinned one. They are left out until accepted.
    KeyChanged {
        user: UserId,
        pinned: PublicKey,
        now: PublicKey,
    },
    /// A user's encryption key isn't validly signed for this server, or has expired.
    /// They are left out.
    BadEncKey { user: UserId },
    /// A user's identity key was pinned on first sight.
    NewContact { user: UserId },
    /// The room state gave two tracks the same media id. Nobody gets our key until a
    /// valid one arrives.
    ClashingTracks,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("sender key for an epoch whose grace period is over")]
    Stale,
    #[error(transparent)]
    Unwrap(#[from] hpke::HpkeError),
}

struct Peer {
    enc_key: PublicKey,
    tracks: Tracks,
}

struct Received {
    secret: Zeroizing<[u8; 32]>,
    retire_at: Option<Instant>,
}

/// Per-epoch sender secrets for one room: ours, wrapped to every trusted peer, and theirs.
/// Holds keys only in memory.
pub struct KeyManager {
    me: UserId,
    enc: EncKeyPair,
    /// Our encryption key before the last renewal.
    retired_enc: Option<EncKeyPair>,
    server: Fingerprint,
    /// The room each user's presence shows them in.
    present: HashMap<UserId, RoomId>,
    /// The peers of the room state last applied, as the server sent them.
    room_peers: Vec<RoomPeer>,
    room: Option<RoomId>,
    epoch: Epoch,
    epoch_since: Instant,
    my_tracks: Option<Tracks>,
    peers: BTreeMap<UserId, Peer>,
    owners: HashMap<MediaId, UserId>,
    my_secret: Option<Zeroizing<[u8; 32]>>,
    /// Who our current secret was wrapped to. Any change to the trusted set means a new secret.
    wrapped_to: BTreeSet<UserId>,
    send_keys: HashMap<MediaId, Arc<TrackKey>>,
    received: HashMap<(UserId, Epoch), Received>,
    recv_keys: HashMap<(MediaId, Epoch), (UserId, Arc<TrackKey>)>,
    /// Sender keys that arrived before the room state they belong to.
    early: HashMap<(UserId, Epoch), (Instant, Vec<u8>)>,
    /// The newest room state for a room we haven't been placed in yet.
    stashed: Option<(RoomId, Epoch, Vec<RoomPeer>)>,
    generation: u64,
}

impl KeyManager {
    pub fn new(me: UserId, enc: EncKeyPair, server: Fingerprint) -> Self {
        Self {
            me,
            enc,
            retired_enc: None,
            server,
            present: HashMap::new(),
            room_peers: Vec::new(),
            room: None,
            epoch: Epoch(0),
            epoch_since: Instant::now(),
            my_tracks: None,
            peers: BTreeMap::new(),
            owners: HashMap::new(),
            my_secret: None,
            wrapped_to: BTreeSet::new(),
            send_keys: HashMap::new(),
            received: HashMap::new(),
            recv_keys: HashMap::new(),
            early: HashMap::new(),
            stashed: None,
            generation: 0,
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn room(&self) -> Option<RoomId> {
        self.room
    }

    /// Records the room `user`'s presence shows them in. True when that changes who is
    /// shown in our room, so the current room state should be applied again.
    pub fn set_presence(&mut self, user: UserId, room: Option<RoomId>) -> bool {
        let before = match room {
            Some(room) => self.present.insert(user, room),
            None => self.present.remove(&user),
        };
        user != self.me && self.room.is_some() && (before == self.room) != (room == self.room)
    }

    /// The room state last applied, to apply again after a pin or presence changes.
    pub fn current_room_state(&self) -> Option<(RoomId, Epoch, Vec<RoomPeer>)> {
        Some((self.room?, self.epoch, self.room_peers.clone()))
    }

    /// The peers our key is wrapped to, as the server described them.
    pub fn trusted_room_peers(&self) -> Vec<RoomPeer> {
        self.room_peers
            .iter()
            .filter(|p| self.peers.contains_key(&p.user))
            .cloned()
            .collect()
    }

    /// Switches to a renewed encryption key, keeping the old one to unwrap secrets that
    /// peers sealed to it before they learn the new one.
    pub fn replace_enc(&mut self, enc: EncKeyPair) {
        self.retired_enc = Some(std::mem::replace(&mut self.enc, enc));
    }

    /// Changes whenever a receive key may have become available.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Applies our placement after `SetRoom`. Returns the `PublishKey` requests to send.
    pub fn placed(
        &mut self,
        room: Option<RoomId>,
        epoch: Epoch,
        tracks: Option<Tracks>,
        peers: &[RoomPeer],
        contacts: &mut Contacts,
        now: Instant,
    ) -> (Vec<Request>, Vec<KeyNotice>) {
        if room.is_some() && room == self.room && epoch < self.epoch {
            return (Vec::new(), Vec::new());
        }
        if room != self.room {
            self.received.clear();
            self.recv_keys.clear();
            self.peers.clear();
            self.owners.clear();
            self.room_peers.clear();
            self.epoch = Epoch(0);
        }
        self.room = room;
        self.my_tracks = tracks;
        self.my_secret = None;
        self.send_keys.clear();
        self.wrapped_to.clear();
        let Some(room) = room else {
            self.stashed = None;
            return (Vec::new(), Vec::new());
        };
        let (first, mut notices) = self.room_state(room, epoch, peers, contacts, now);
        let mut requests: Vec<Request> = first.into_iter().collect();
        if let Some((stashed_room, stashed_epoch, stashed_peers)) = self.stashed.take()
            && stashed_room == room
            && stashed_epoch > epoch
        {
            let (later, more) = self.room_state(room, stashed_epoch, &stashed_peers, contacts, now);
            requests.extend(later);
            notices.extend(more);
        }
        (requests, notices)
    }

    /// Applies a `RoomState`. Our secret is replaced whenever the epoch moves or the set of
    /// trusted peers differs from the one it was wrapped to, so a peer added or removed
    /// without an epoch bump never holds a secret from before or after its membership.
    /// A state that reuses a media id trusts nobody.
    pub fn room_state(
        &mut self,
        room: RoomId,
        epoch: Epoch,
        peers: &[RoomPeer],
        contacts: &mut Contacts,
        now: Instant,
    ) -> (Option<Request>, Vec<KeyNotice>) {
        if Some(room) != self.room {
            if self
                .stashed
                .as_ref()
                .is_none_or(|(r, e, _)| *r != room || *e < epoch)
            {
                self.stashed = Some((room, epoch, peers.to_vec()));
            }
            return (None, Vec::new());
        }
        if epoch < self.epoch {
            return (None, Vec::new());
        }
        let (trusted, notices) = if self.clashing_tracks(peers) {
            warn!(
                room = room.0,
                "room state reuses a media id; trusting nobody"
            );
            let repeated = self.room_peers == peers;
            let notices = if repeated {
                Vec::new()
            } else {
                vec![KeyNotice::ClashingTracks]
            };
            (BTreeMap::new(), notices)
        } else {
            self.trusted_peers(room, peers, contacts)
        };
        self.room_peers = peers.to_vec();
        for (user, _) in self
            .peers
            .iter()
            .filter(|(user, _)| !trusted.contains_key(*user))
        {
            for ((from, _), received) in self.received.iter_mut() {
                if from == user && received.retire_at.is_none() {
                    received.retire_at = Some(now + OLD_KEY_GRACE);
                }
            }
        }
        self.peers = trusted;
        self.owners = self
            .peers
            .iter()
            .flat_map(|(user, peer)| track_ids(&peer.tracks).into_iter().map(|id| (id, *user)))
            .collect();

        let peer_set: BTreeSet<UserId> = self.peers.keys().copied().collect();
        if epoch != self.epoch || self.my_secret.is_none() || peer_set != self.wrapped_to {
            self.start_epoch(epoch, now);
        }
        let request = self.wrap_for_new_peers(room);
        for ((from, early_epoch), (arrived, wrapped)) in std::mem::take(&mut self.early) {
            if now.duration_since(arrived) < OLD_KEY_GRACE {
                self.sender_key_at(from, early_epoch, wrapped, arrived, now)
                    .ok();
            }
        }
        self.generation += 1;
        (request, notices)
    }

    /// True when a user or a media id appears twice, counting our own tracks.
    fn clashing_tracks(&self, peers: &[RoomPeer]) -> bool {
        let mut users = HashSet::new();
        let mut ids = HashSet::new();
        let others = peers.iter().filter(|p| p.user != self.me);
        let theirs = others.clone().flat_map(|p| track_ids(&p.tracks));
        let mine = self.my_tracks.iter().flat_map(track_ids);
        !others.clone().all(|p| users.insert(p.user))
            || !mine.chain(theirs).all(|id| ids.insert(id))
    }

    /// Peers shown in `room` by their presence, whose encryption key is validly signed and
    /// current, and whose identity key matches its pin. A user seen for the first time is
    /// pinned only once their key checks out.
    fn trusted_peers(
        &self,
        room: RoomId,
        peers: &[RoomPeer],
        contacts: &mut Contacts,
    ) -> (BTreeMap<UserId, Peer>, Vec<KeyNotice>) {
        let mut notices = Vec::new();
        let mut trusted = BTreeMap::new();
        let unix_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let shown = |p: &&RoomPeer| p.user != self.me && self.present.get(&p.user) == Some(&room);
        for peer in peers.iter().filter(shown) {
            let signed = &peer.enc_key;
            let message =
                identity::enc_key_message(&self.server.0, peer.user, signed.not_after, &signed.key);
            if signed.not_after <= unix_now
                || identity::verify(&peer.identity_key, &message, &signed.sig).is_err()
            {
                notices.push(KeyNotice::BadEncKey { user: peer.user });
                continue;
            }
            match contacts.status(peer.user, &peer.identity_key) {
                ContactStatus::Changed { pinned } => {
                    notices.push(KeyNotice::KeyChanged {
                        user: peer.user,
                        pinned,
                        now: peer.identity_key,
                    });
                    continue;
                }
                ContactStatus::New => {
                    if let Err(e) = contacts.pin(peer.user, peer.identity_key) {
                        warn!("could not save a contact: {e:#}");
                    }
                    notices.push(KeyNotice::NewContact { user: peer.user });
                }
                ContactStatus::Known { .. } => {}
            }
            trusted.insert(
                peer.user,
                Peer {
                    enc_key: signed.key,
                    tracks: peer.tracks.clone(),
                },
            );
        }
        (trusted, notices)
    }

    fn start_epoch(&mut self, epoch: Epoch, now: Instant) {
        if epoch != self.epoch {
            for ((_, old), received) in self.received.iter_mut() {
                if *old < epoch && received.retire_at.is_none() {
                    received.retire_at = Some(now + OLD_KEY_GRACE);
                }
            }
            self.epoch = epoch;
            self.epoch_since = now;
        }
        let secret = Zeroizing::new(random::<32>());
        self.send_keys = self
            .my_tracks
            .iter()
            .flat_map(track_ids)
            .map(|id| (id, Arc::new(TrackKey::derive(&secret, id))))
            .collect();
        self.my_secret = Some(secret);
        self.wrapped_to.clear();
    }

    fn context(&self, room: RoomId, epoch: Epoch, from: UserId, to: UserId) -> WrapContext {
        WrapContext {
            server: self.server,
            room,
            epoch,
            from,
            to,
        }
    }

    fn wrap_for_new_peers(&mut self, room: RoomId) -> Option<Request> {
        let secret = self.my_secret.as_ref()?;
        let mut wrapped = Vec::new();
        for (user, peer) in &self.peers {
            if self.wrapped_to.contains(user) {
                continue;
            }
            let context = self.context(room, self.epoch, self.me, *user);
            match hpke::wrap(&self.enc, &peer.enc_key, &context, secret) {
                Ok(blob) => wrapped.push((*user, blob)),
                Err(e) => warn!(user = user.0, "could not wrap a sender key: {e}"),
            }
        }
        self.wrapped_to
            .extend(wrapped.iter().map(|(user, _)| *user));
        (!wrapped.is_empty()).then_some(Request::PublishKey {
            epoch: self.epoch,
            wrapped,
        })
    }

    /// Stores a peer's sender secret. One that arrives before its room state is held for
    /// up to [`OLD_KEY_GRACE`]. One for an earlier epoch is accepted only within the grace
    /// period after our epoch moved on, and expires with it.
    pub fn sender_key(
        &mut self,
        from: UserId,
        epoch: Epoch,
        wrapped: &[u8],
        now: Instant,
    ) -> Result<(), KeyError> {
        self.sender_key_at(from, epoch, wrapped.to_vec(), now, now)
    }

    fn sender_key_at(
        &mut self,
        from: UserId,
        epoch: Epoch,
        wrapped: Vec<u8>,
        arrived: Instant,
        now: Instant,
    ) -> Result<(), KeyError> {
        let room = match self.room {
            Some(room) if self.peers.contains_key(&from) && epoch <= self.epoch => room,
            _ => {
                if self.early.len() < MAX_EARLY || self.early.contains_key(&(from, epoch)) {
                    self.early.insert((from, epoch), (arrived, wrapped));
                }
                return Ok(());
            }
        };
        let retire_at = if epoch < self.epoch {
            let ends = self.epoch_since + OLD_KEY_GRACE;
            if now >= ends {
                return Err(KeyError::Stale);
            }
            Some(ends)
        } else {
            None
        };
        let enc_key = self
            .peers
            .get(&from)
            .map(|p| p.enc_key)
            .ok_or(KeyError::Stale)?;
        let context = self.context(room, epoch, from, self.me);
        let secret =
            hpke::unwrap(&self.enc, &enc_key, &context, &wrapped).or_else(|e| {
                match &self.retired_enc {
                    Some(retired) => hpke::unwrap(retired, &enc_key, &context, &wrapped),
                    None => Err(e),
                }
            })?;
        for ((user, old), received) in self.received.iter_mut() {
            if *user == from && *old < epoch && received.retire_at.is_none() {
                received.retire_at = Some(now + OLD_KEY_GRACE);
            }
        }
        self.received
            .insert((from, epoch), Received { secret, retire_at });
        self.recv_keys
            .retain(|(_, e), (owner, _)| !(*owner == from && *e == epoch));
        self.generation += 1;
        Ok(())
    }

    /// The key and epoch to send on one of our tracks.
    pub fn send_key(&self, media_id: MediaId) -> Option<(Epoch, Arc<TrackKey>)> {
        Some((self.epoch, self.send_keys.get(&media_id)?.clone()))
    }

    /// Who sends on `media_id`, among trusted peers.
    pub fn owner(&self, media_id: MediaId) -> Option<UserId> {
        self.owners.get(&media_id).copied()
    }

    pub fn peer_tracks(&self, user: UserId) -> Option<&Tracks> {
        self.peers.get(&user).map(|p| &p.tracks)
    }

    pub fn recv_key(&mut self, media_id: MediaId, epoch: Epoch) -> Option<Arc<TrackKey>> {
        if let Some((_, key)) = self.recv_keys.get(&(media_id, epoch)) {
            return Some(key.clone());
        }
        let owner = self.owner(media_id)?;
        let received = self.received.get(&(owner, epoch))?;
        let key = Arc::new(TrackKey::derive(&received.secret, media_id));
        self.recv_keys
            .insert((media_id, epoch), (owner, key.clone()));
        Some(key)
    }

    /// Zeroizes keys whose grace period has ended, and drops stale early keys.
    pub fn tick(&mut self, now: Instant) {
        self.received
            .retain(|_, r| r.retire_at.is_none_or(|at| at > now));
        let received = &self.received;
        self.recv_keys
            .retain(|(_, epoch), (owner, _)| received.contains_key(&(*owner, *epoch)));
        self.early
            .retain(|_, (arrived, _)| now.duration_since(*arrived) < OLD_KEY_GRACE);
    }
}

pub fn track_ids(tracks: &Tracks) -> Vec<MediaId> {
    std::iter::once(tracks.mic)
        .chain(tracks.shares.iter().flat_map(|s| [s.video, s.audio]))
        .collect()
}

#[cfg(test)]
mod tests {
    use throcc_crypto::identity::IdentityKey;
    use throcc_proto::messages::{Share, SignedEncKey};

    use super::*;

    const SERVER: Fingerprint = Fingerprint([5; 32]);
    const ROOM: RoomId = RoomId(1);

    struct Member {
        id: UserId,
        identity: IdentityKey,
        keys: KeyManager,
        contacts: Contacts,
        enc_public: PublicKey,
        tracks: Tracks,
        _dir: tempfile::TempDir,
    }

    fn member(id: u64) -> Member {
        let dir = tempfile::tempdir().unwrap();
        let enc = EncKeyPair::generate();
        let first = id as u32 * 10;
        Member {
            id: UserId(id),
            identity: IdentityKey::generate(),
            enc_public: enc.public(),
            keys: KeyManager::new(UserId(id), enc, SERVER),
            contacts: Contacts::load(dir.path(), &SERVER).unwrap(),
            tracks: Tracks {
                mic: MediaId(first),
                shares: vec![Share {
                    video: MediaId(first + 1),
                    audio: MediaId(first + 2),
                }],
            },
            _dir: dir,
        }
    }

    fn unix_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn signed(m: &Member, not_after: u64) -> SignedEncKey {
        let message = identity::enc_key_message(&SERVER.0, m.id, not_after, &m.enc_public);
        SignedEncKey {
            key: m.enc_public,
            not_after,
            sig: m.identity.sign(&message),
        }
    }

    fn peer(m: &Member) -> RoomPeer {
        RoomPeer {
            user: m.id,
            tracks: m.tracks.clone(),
            share: None,
            identity_key: m.identity.public(),
            enc_key: signed(m, unix_now() + 3600),
        }
    }

    /// Shows every one of `peers` in the room, as their presence would.
    fn show(keys: &mut KeyManager, peers: &[RoomPeer]) {
        for p in peers {
            keys.set_presence(p.user, Some(ROOM));
        }
    }

    /// Delivers a room state to every member and all resulting sender keys.
    fn deliver(members: &mut [&mut Member], epoch: u32, peers: &[RoomPeer], now: Instant) {
        let mut published = Vec::new();
        for m in members.iter_mut() {
            show(&mut m.keys, peers);
            let tracks = Some(m.tracks.clone());
            let requests = if m.keys.room() == Some(ROOM) {
                m.keys
                    .room_state(ROOM, Epoch(epoch), peers, &mut m.contacts, now)
                    .0
                    .into_iter()
                    .collect()
            } else {
                m.keys
                    .placed(
                        Some(ROOM),
                        Epoch(epoch),
                        tracks,
                        peers,
                        &mut m.contacts,
                        now,
                    )
                    .0
            };
            for request in requests {
                if let Request::PublishKey { epoch, wrapped } = request {
                    published.push((m.id, epoch, wrapped));
                }
            }
        }
        for (from, epoch, wrapped) in published {
            for (to, blob) in wrapped {
                if let Some(target) = members.iter_mut().find(|m| m.id == to) {
                    target.keys.sender_key(from, epoch, &blob, now).unwrap();
                }
            }
        }
    }

    fn rekey(members: &mut [&mut Member], epoch: u32, now: Instant) {
        let peers: Vec<RoomPeer> = members.iter().map(|m| peer(m)).collect();
        deliver(members, epoch, &peers, now);
    }

    fn can_read(receiver: &mut Member, sender: &Member, epoch: u32) -> bool {
        receiver
            .keys
            .recv_key(sender.tracks.mic, Epoch(epoch))
            .is_some()
    }

    #[test]
    fn joiner_cannot_read_the_previous_epoch_and_leaver_cannot_read_the_next() {
        let now = Instant::now();
        let (mut a, mut b, mut c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);
        assert!(can_read(&mut b, &a, 1));

        rekey(&mut [&mut a, &mut b, &mut c], 2, now);
        assert!(can_read(&mut c, &a, 2));
        assert!(!can_read(&mut c, &a, 1), "a joiner read the previous epoch");

        rekey(&mut [&mut a, &mut c], 3, now);
        assert!(can_read(&mut c, &a, 3));
        assert!(!can_read(&mut b, &a, 3), "a leaver read the next epoch");
    }

    #[test]
    fn a_peer_added_without_an_epoch_bump_gets_a_fresh_secret() {
        let now = Instant::now();
        let (mut a, mut b, mut d) = (member(1), member(2), member(4));
        rekey(&mut [&mut a, &mut b], 1, now);
        let before = a.keys.send_key(a.tracks.mic).unwrap().1;
        rekey(&mut [&mut a, &mut b, &mut d], 1, now);
        let after = a.keys.send_key(a.tracks.mic).unwrap().1;
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the secret from before the join was reused"
        );
        assert!(can_read(&mut d, &a, 1));
        assert!(can_read(&mut b, &a, 1));
    }

    #[test]
    fn old_epoch_keys_expire_after_the_grace_period() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        rekey(&mut [&mut a, &mut b], 2, now);
        assert!(can_read(&mut b, &a, 1));
        b.keys.tick(now + OLD_KEY_GRACE);
        assert!(!can_read(&mut b, &a, 1));
        assert!(can_read(&mut b, &a, 2));
    }

    #[test]
    fn a_departed_peers_cached_keys_are_dropped_after_the_grace_period() {
        let now = Instant::now();
        let (mut a, mut b, mut c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b, &mut c], 1, now);
        assert!(can_read(&mut b, &c, 1));
        rekey(&mut [&mut a, &mut b], 2, now);
        b.keys.tick(now + OLD_KEY_GRACE * 5);
        assert!(
            b.keys
                .recv_keys
                .keys()
                .all(|(media, _)| *media != c.tracks.mic)
        );
        assert!(!can_read(&mut b, &c, 1));
    }

    #[test]
    fn a_replayed_old_sender_key_is_refused_after_the_grace_period() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, _) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        let Some(Request::PublishKey { wrapped, .. }) = requests.into_iter().next() else {
            panic!()
        };
        rekey(&mut [&mut a, &mut b], 2, now);
        let later = now + OLD_KEY_GRACE * 2;
        assert_eq!(
            b.keys.sender_key(a.id, Epoch(1), &wrapped[0].1, later),
            Err(KeyError::Stale)
        );
    }

    #[test]
    fn substituted_or_expired_enc_keys_are_rejected() {
        let now = Instant::now();
        let (mut a, b, c) = (member(1), member(2), member(3));
        let mut forged = peer(&b);
        forged.enc_key.key = EncKeyPair::generate().public();
        let mut expired = peer(&c);
        expired.enc_key = signed(&c, unix_now() - 1);
        let peers = vec![peer(&a), forged, expired];
        show(&mut a.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, notices) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        assert!(notices.contains(&KeyNotice::BadEncKey { user: b.id }));
        assert!(notices.contains(&KeyNotice::BadEncKey { user: c.id }));
        assert!(requests.is_empty());
        assert_eq!(
            a.contacts.status(b.id, &b.identity.public()),
            ContactStatus::New
        );
    }

    #[test]
    fn changed_identity_key_is_left_out_until_accepted() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let old = b.identity.public();
        b.identity = IdentityKey::generate();
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        let (request, notices) = a
            .keys
            .room_state(ROOM, Epoch(2), &peers, &mut a.contacts, now);
        assert_eq!(request, None);
        assert!(notices.contains(&KeyNotice::KeyChanged {
            user: b.id,
            pinned: old,
            now: b.identity.public()
        }));

        a.contacts.pin(b.id, b.identity.public()).unwrap();
        let (request, _) = a
            .keys
            .room_state(ROOM, Epoch(2), &peers, &mut a.contacts, now);
        assert!(
            matches!(request, Some(Request::PublishKey { ref wrapped, .. }) if wrapped.len() == 1)
        );
    }

    #[test]
    fn sender_key_before_room_state_is_applied_later() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        show(&mut b.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, _) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        let Some(Request::PublishKey { epoch, wrapped }) = requests.into_iter().next() else {
            panic!()
        };
        b.keys.sender_key(a.id, epoch, &wrapped[0].1, now).unwrap();
        assert!(!can_read(&mut b, &a, 1));
        let tracks = Some(b.tracks.clone());
        b.keys
            .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut b.contacts, now);
        assert!(can_read(&mut b, &a, 1));
    }

    #[test]
    fn room_state_before_placed_is_kept_and_a_stale_placed_is_ignored() {
        let now = Instant::now();
        let (mut a, b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        a.keys
            .room_state(ROOM, Epoch(3), &peers, &mut a.contacts, now);
        let tracks = Some(a.tracks.clone());
        a.keys.placed(
            Some(ROOM),
            Epoch(2),
            tracks.clone(),
            &peers,
            &mut a.contacts,
            now,
        );
        assert_eq!(a.keys.epoch(), Epoch(3));
        let (requests, _) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        assert!(requests.is_empty());
        assert_eq!(a.keys.epoch(), Epoch(3));
        assert!(a.keys.send_key(a.tracks.mic).is_some());
    }

    fn wrapped_to(request: Option<&Request>) -> Vec<UserId> {
        match request {
            Some(Request::PublishKey { wrapped, .. }) => wrapped.iter().map(|(u, _)| *u).collect(),
            _ => Vec::new(),
        }
    }

    fn reapply(m: &mut Member, now: Instant) -> (Option<Request>, Vec<KeyNotice>) {
        let (room, epoch, peers) = m.keys.current_room_state().unwrap();
        m.keys.room_state(room, epoch, &peers, &mut m.contacts, now)
    }

    #[test]
    fn a_peer_without_presence_in_the_room_never_gets_our_key() {
        let now = Instant::now();
        let (mut a, b, hidden) = (member(1), member(2), member(3));
        let peers = vec![peer(&a), peer(&b), peer(&hidden)];
        show(&mut a.keys, &peers[..2]);
        let tracks = Some(a.tracks.clone());
        let (requests, notices) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        assert_eq!(wrapped_to(requests.first()), [b.id]);
        assert!(notices.contains(&KeyNotice::NewContact { user: b.id }));
        assert!(!notices.contains(&KeyNotice::NewContact { user: hidden.id }));
        assert_eq!(
            a.contacts.status(hidden.id, &hidden.identity.public()),
            ContactStatus::New
        );
        assert_eq!(a.keys.owner(hidden.tracks.mic), None);

        assert!(a.keys.set_presence(hidden.id, Some(ROOM)));
        let (request, _) = reapply(&mut a, now);
        assert_eq!(wrapped_to(request.as_ref()), [b.id, hidden.id]);

        assert!(a.keys.set_presence(hidden.id, Some(RoomId(9))));
        let (request, _) = reapply(&mut a, now);
        assert_eq!(wrapped_to(request.as_ref()), [b.id]);
        assert!(!a.keys.set_presence(hidden.id, None));
    }

    #[test]
    fn a_room_state_reusing_a_media_id_trusts_nobody() {
        let now = Instant::now();
        let (mut a, mut b, c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);
        assert_eq!(a.keys.owner(b.tracks.mic), Some(b.id));

        for stolen in [b.tracks.mic, a.tracks.mic] {
            let mut forged = peer(&c);
            forged.tracks.shares[0].video = stolen;
            let peers = vec![peer(&a), peer(&b), forged];
            show(&mut a.keys, &peers);
            let (request, notices) =
                a.keys
                    .room_state(ROOM, Epoch(2), &peers, &mut a.contacts, now);
            assert_eq!(request, None);
            assert_eq!(notices, [KeyNotice::ClashingTracks]);
            assert_eq!(a.keys.owner(b.tracks.mic), None);
            assert_eq!(a.keys.owner(stolen), None);
            assert!(a.keys.trusted_room_peers().is_empty());
        }
    }

    #[test]
    fn a_secret_wrapped_to_the_retired_enc_key_still_unwraps() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        show(&mut b.keys, &peers);
        let tracks = Some(a.tracks.clone());
        a.keys
            .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        let tracks = Some(b.tracks.clone());
        let (requests, _) =
            b.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut b.contacts, now);
        let Some(Request::PublishKey { epoch, wrapped }) = requests.into_iter().next() else {
            panic!()
        };
        a.keys.replace_enc(EncKeyPair::generate());
        a.keys.sender_key(b.id, epoch, &wrapped[0].1, now).unwrap();
        assert!(can_read(&mut a, &b, 1));
    }
}
