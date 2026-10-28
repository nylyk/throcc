use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use throcc_crypto::hpke::{self, EncKeyPair, WrapContext};
use throcc_crypto::identity::{self, random};
use throcc_crypto::media::TrackKey;
use throcc_proto::ids::{Epoch, MediaId, RoomId, UserId};
use throcc_proto::messages::{PublicKey, Request, RoomPeer, Tracks};
use tracing::warn;
use zeroize::Zeroizing;

use crate::contacts::{ContactStatus, Contacts};

/// How long a sender's previous epoch key stays usable after it moves on.
pub const OLD_KEY_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyNotice {
    /// A user's identity key differs from the pinned one. They are left out until accepted.
    KeyChanged {
        user: UserId,
        pinned: PublicKey,
        now: PublicKey,
    },
    /// A user's encryption key isn't signed by their identity key. They are left out.
    BadEncKey {
        user: UserId,
    },
    NewContact {
        user: UserId,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
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
    room: Option<RoomId>,
    epoch: Epoch,
    my_tracks: Option<Tracks>,
    peers: BTreeMap<UserId, Peer>,
    my_secret: Option<Zeroizing<[u8; 32]>>,
    wrapped_to: BTreeSet<UserId>,
    send_keys: HashMap<MediaId, Arc<TrackKey>>,
    received: HashMap<(UserId, Epoch), Received>,
    recv_keys: HashMap<(MediaId, Epoch), Arc<TrackKey>>,
    /// Sender keys that arrived before the room state they belong to.
    early: Vec<(Instant, UserId, Epoch, Vec<u8>)>,
}

impl KeyManager {
    pub fn new(me: UserId, enc: EncKeyPair) -> Self {
        Self {
            me,
            enc,
            room: None,
            epoch: Epoch(0),
            my_tracks: None,
            peers: BTreeMap::new(),
            my_secret: None,
            wrapped_to: BTreeSet::new(),
            send_keys: HashMap::new(),
            received: HashMap::new(),
            recv_keys: HashMap::new(),
            early: Vec::new(),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn room(&self) -> Option<RoomId> {
        self.room
    }

    /// Applies a new placement: our room, epoch and tracks plus the peer list.
    pub fn placed(
        &mut self,
        room: Option<RoomId>,
        epoch: Epoch,
        tracks: Option<Tracks>,
        peers: &[RoomPeer],
        contacts: &mut Contacts,
        now: Instant,
    ) -> (Option<Request>, Vec<KeyNotice>) {
        if room != self.room {
            self.received.clear();
            self.recv_keys.clear();
            self.epoch = Epoch(0);
        }
        self.room = room;
        self.my_tracks = tracks;
        self.my_secret = None;
        self.send_keys.clear();
        match room {
            Some(room) => self.room_state(room, epoch, peers, contacts, now),
            None => {
                self.peers.clear();
                (None, Vec::new())
            }
        }
    }

    /// Applies a `RoomState`. A new epoch gets a fresh sender secret, wrapped to every peer
    /// whose keys check out. Returns the `PublishKey` request to send, if any.
    pub fn room_state(
        &mut self,
        room: RoomId,
        epoch: Epoch,
        peers: &[RoomPeer],
        contacts: &mut Contacts,
        now: Instant,
    ) -> (Option<Request>, Vec<KeyNotice>) {
        if Some(room) != self.room || epoch < self.epoch {
            return (None, Vec::new());
        }
        let mut notices = Vec::new();
        self.peers.clear();
        for peer in peers.iter().filter(|p| p.user != self.me) {
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
            let message = identity::enc_key_message(&peer.enc_key);
            if identity::verify(&peer.identity_key, &message, &peer.enc_key_sig).is_err() {
                notices.push(KeyNotice::BadEncKey { user: peer.user });
                continue;
            }
            self.peers.insert(
                peer.user,
                Peer {
                    enc_key: peer.enc_key,
                    tracks: peer.tracks.clone(),
                },
            );
        }

        if epoch != self.epoch || self.my_secret.is_none() {
            self.start_epoch(epoch, now);
        }
        let request = self.wrap_for_new_peers(room);
        for (arrived, from, epoch, wrapped) in std::mem::take(&mut self.early) {
            if now.duration_since(arrived) < OLD_KEY_GRACE {
                self.sender_key_at(from, epoch, wrapped, arrived, now).ok();
            }
        }
        (request, notices)
    }

    fn start_epoch(&mut self, epoch: Epoch, now: Instant) {
        for ((_, old), received) in self.received.iter_mut() {
            if *old < epoch && received.retire_at.is_none() {
                received.retire_at = Some(now + OLD_KEY_GRACE);
            }
        }
        self.epoch = epoch;
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

    fn wrap_for_new_peers(&mut self, room: RoomId) -> Option<Request> {
        let secret = self.my_secret.as_ref()?;
        let mut wrapped = Vec::new();
        for (user, peer) in &self.peers {
            if self.wrapped_to.contains(user) {
                continue;
            }
            let context = WrapContext {
                room,
                epoch: self.epoch,
                from: self.me,
                to: *user,
            };
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
    /// up to [`OLD_KEY_GRACE`] and applied with that room state.
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
        let (Some(room), Some(peer)) = (self.room, self.peers.get(&from)) else {
            self.early.push((arrived, from, epoch, wrapped));
            return Ok(());
        };
        if epoch > self.epoch {
            self.early.push((arrived, from, epoch, wrapped));
            return Ok(());
        }
        let context = WrapContext {
            room,
            epoch,
            from,
            to: self.me,
        };
        let secret = hpke::unwrap(&self.enc, &peer.enc_key, &context, &wrapped)?;
        let retire_at = (epoch < self.epoch).then_some(now + OLD_KEY_GRACE);
        for ((user, old), received) in self.received.iter_mut() {
            if *user == from && *old < epoch && received.retire_at.is_none() {
                received.retire_at = Some(now + OLD_KEY_GRACE);
            }
        }
        self.received
            .insert((from, epoch), Received { secret, retire_at });
        let tracks = track_ids(&peer_tracks(&self.peers, from));
        self.recv_keys
            .retain(|(media, e), _| *e != epoch || !tracks.contains(media));
        Ok(())
    }

    /// The key and epoch to send on one of our tracks.
    pub fn send_key(&self, media_id: MediaId) -> Option<(Epoch, Arc<TrackKey>)> {
        Some((self.epoch, self.send_keys.get(&media_id)?.clone()))
    }

    /// Who sends on `media_id`, among trusted peers.
    pub fn owner(&self, media_id: MediaId) -> Option<UserId> {
        self.peers
            .iter()
            .find(|(_, peer)| track_ids(&peer.tracks).contains(&media_id))
            .map(|(user, _)| *user)
    }

    pub fn peer_tracks(&self, user: UserId) -> Option<&Tracks> {
        self.peers.get(&user).map(|p| &p.tracks)
    }

    pub fn recv_key(&mut self, media_id: MediaId, epoch: Epoch) -> Option<Arc<TrackKey>> {
        if let Some(key) = self.recv_keys.get(&(media_id, epoch)) {
            return Some(key.clone());
        }
        let owner = self.owner(media_id)?;
        let received = self.received.get(&(owner, epoch))?;
        let key = Arc::new(TrackKey::derive(&received.secret, media_id));
        self.recv_keys.insert((media_id, epoch), key.clone());
        Some(key)
    }

    /// Zeroizes keys whose grace period has ended.
    pub fn tick(&mut self, now: Instant) {
        self.early
            .retain(|(arrived, ..)| now.duration_since(*arrived) < OLD_KEY_GRACE);
        let retired: Vec<(UserId, Epoch)> = self
            .received
            .iter()
            .filter(|(_, r)| r.retire_at.is_some_and(|at| at <= now))
            .map(|(k, _)| *k)
            .collect();
        for (user, epoch) in retired {
            self.received.remove(&(user, epoch));
            let tracks = track_ids(&peer_tracks(&self.peers, user));
            self.recv_keys
                .retain(|(media, e), _| *e != epoch || !tracks.contains(media));
        }
    }
}

fn peer_tracks(peers: &BTreeMap<UserId, Peer>, user: UserId) -> Tracks {
    peers.get(&user).map_or(
        Tracks {
            mic: MediaId(0),
            shares: Vec::new(),
        },
        |p| p.tracks.clone(),
    )
}

pub fn track_ids(tracks: &Tracks) -> Vec<MediaId> {
    std::iter::once(tracks.mic)
        .chain(tracks.shares.iter().flat_map(|s| [s.video, s.audio]))
        .collect()
}

#[cfg(test)]
mod tests {
    use throcc_crypto::identity::IdentityKey;
    use throcc_proto::Fingerprint;
    use throcc_proto::messages::Share;

    use super::*;

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
            keys: KeyManager::new(UserId(id), enc),
            contacts: Contacts::load(dir.path(), &Fingerprint([0; 32])).unwrap(),
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

    fn peer(m: &Member) -> RoomPeer {
        RoomPeer {
            user: m.id,
            tracks: m.tracks.clone(),
            share: None,
            identity_key: m.identity.public(),
            enc_key: m.enc_public,
            enc_key_sig: m.identity.sign(&identity::enc_key_message(&m.enc_public)),
        }
    }

    const ROOM: RoomId = RoomId(1);

    /// Moves every member to `epoch` with each other as peers and delivers all sender keys.
    fn rekey(members: &mut [&mut Member], epoch: u32, now: Instant) {
        let peers: Vec<RoomPeer> = members.iter().map(|m| peer(m)).collect();
        let mut published = Vec::new();
        for m in members.iter_mut() {
            let tracks = Some(m.tracks.clone());
            let (request, _) = if m.keys.room() == Some(ROOM) {
                m.keys
                    .room_state(ROOM, Epoch(epoch), &peers, &mut m.contacts, now)
            } else {
                m.keys.placed(
                    Some(ROOM),
                    Epoch(epoch),
                    tracks,
                    &peers,
                    &mut m.contacts,
                    now,
                )
            };
            if let Some(Request::PublishKey { epoch, wrapped }) = request {
                published.push((m.id, epoch, wrapped));
            }
        }
        for (from, epoch, wrapped) in published {
            for (to, blob) in wrapped {
                let target = members.iter_mut().find(|m| m.id == to).unwrap();
                target.keys.sender_key(from, epoch, &blob, now).unwrap();
            }
        }
    }

    fn can_read(receiver: &mut Member, sender: &Member, epoch: u32) -> bool {
        assert!(sender.keys.send_key(sender.tracks.mic).is_some());
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
    fn old_epoch_keys_expire_after_the_grace_period() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        rekey(&mut [&mut a, &mut b], 2, now);
        b.keys.recv_keys.clear();
        assert!(can_read(&mut b, &a, 1));
        b.keys.tick(now + OLD_KEY_GRACE);
        assert!(!can_read(&mut b, &a, 1));
        assert!(can_read(&mut b, &a, 2));
    }

    #[test]
    fn substituted_enc_key_with_bad_signature_is_rejected() {
        let now = Instant::now();
        let (mut a, b) = (member(1), member(2));
        let mut forged = peer(&b);
        forged.enc_key = EncKeyPair::generate().public();
        let peers = vec![peer(&a), forged];
        let tracks = Some(a.tracks.clone());
        let (request, notices) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        assert!(notices.contains(&KeyNotice::BadEncKey { user: b.id }));
        assert_eq!(request, None);
    }

    #[test]
    fn changed_identity_key_is_left_out_until_accepted() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let old = b.identity.public();
        b.identity = IdentityKey::generate();
        let peers = vec![peer(&a), peer(&b)];
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
        let tracks = Some(a.tracks.clone());
        let (request, _) =
            a.keys
                .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut a.contacts, now);
        let Some(Request::PublishKey { epoch, wrapped }) = request else {
            panic!()
        };
        b.keys.sender_key(a.id, epoch, &wrapped[0].1, now).unwrap();
        assert!(!can_read(&mut b, &a, 1));
        let tracks = Some(b.tracks.clone());
        b.keys
            .placed(Some(ROOM), Epoch(1), tracks, &peers, &mut b.contacts, now);
        assert!(can_read(&mut b, &a, 1));
    }
}
