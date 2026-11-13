use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use throcc_crypto::hpke::{self, EncryptionKeyPair, WrapContext};
use throcc_crypto::identity::{self, random};
use throcc_crypto::media::TrackKey;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, MediaId, RoomId, UserId};
use throcc_proto::limits::{MAX_ROOM_MEMBERS, MAX_ROOM_SHARES, MAX_SIGNED_IN};
use throcc_proto::messages::{Peer, PublicKey, Request, SignedEncryptionKey, Tracks};
use tracing::warn;
use zeroize::Zeroizing;

use crate::floors::EpochFloors;

/// How long a sender's previous epoch key stays usable after it moves on.
const OLD_KEY_GRACE: Duration = Duration::from_secs(2);
/// How long our encryption key still unwraps secrets after a renewal replaced it.
const RETIRED_ENCRYPTION_KEPT: Duration = OLD_KEY_GRACE.saturating_mul(5);
/// Sender keys kept from senders the room state hasn't named yet: two epochs of a full room.
const MAX_EARLY: usize = 2 * MAX_ROOM_MEMBERS;
/// A wrapped sender secret: the 32-byte encapsulated key, then the 32-byte secret sealed
/// with a 16-byte tag.
const WRAPPED_BYTES: usize = 32 + 32 + 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyNotice {
    /// A user's encryption key isn't validly signed for this server, or has expired.
    /// They are left out.
    BadEncryptionKey { user: UserId },
    /// The room state gave two tracks the same media id. Nobody gets our key until a
    /// valid one arrives.
    ClashingTracks,
    /// The room state listed more than [`MAX_ROOM_MEMBERS`] people. Nobody gets our key until
    /// a valid one arrives.
    TooManyPeers,
    /// The server gave someone more than [`MAX_ROOM_SHARES`] shares. Nobody gets our key
    /// until a valid room state arrives, or, when it was us, until we are placed again.
    TooManyShares,
    /// We were placed in a room at an epoch no newer than one we took part in there
    /// before, which only a server restored from a backup or replaying an old call does.
    /// Keys and media are refused until the room's epoch passes it.
    OldEpoch,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("sender key for an epoch whose grace period is over")]
    Stale,
    #[error("sender key of the wrong length")]
    Length,
    #[error(transparent)]
    Unwrap(#[from] hpke::HpkeError),
}

struct KeyPeer {
    identity_key: PublicKey,
    encryption_key: PublicKey,
    tracks: Tracks,
}

struct Received {
    secret: Zeroizing<[u8; 32]>,
    retire_at: Option<Instant>,
}

/// Per-epoch sender secrets for one room: ours, wrapped to every trusted peer, and theirs.
/// A peer is trusted once its encryption key is validly signed by the identity key it
/// presents, for this server and its user; who that identity key belongs to is the user's
/// to verify. Holds keys only in memory.
pub struct KeyManager {
    me: UserId,
    encryption: EncryptionKeyPair,
    /// Our encryption key before the last renewal, until the time it is dropped.
    retired_encryption: Option<(EncryptionKeyPair, Instant)>,
    server: Fingerprint,
    /// The people we were told are signed in, with the room each is in, at most
    /// [`MAX_SIGNED_IN`]. A client that shows people tells us of exactly those it shows, so
    /// nobody it leaves out is trusted.
    present: HashMap<UserId, Option<RoomId>>,
    /// The peers of the room state last applied, as the server sent them.
    room_peers: Vec<Peer>,
    room: Option<RoomId>,
    epoch: Epoch,
    epoch_since: Instant,
    floors: EpochFloors,
    /// The newest epoch we took part in for this room before we were placed in it. Nothing
    /// at or before it is accepted: no epoch from before a room change can be one we belong
    /// to.
    floor: Option<Epoch>,
    my_tracks: Option<Tracks>,
    peers: BTreeMap<UserId, KeyPeer>,
    owners: HashMap<MediaId, UserId>,
    my_secret: Option<Zeroizing<[u8; 32]>>,
    /// Who our current secret was wrapped to, by the encryption key it was wrapped to. Any
    /// change to the trusted set or their keys means a new secret.
    wrapped_to: BTreeSet<(UserId, PublicKey)>,
    /// Peers whose encryption key HPKE refused to wrap to, with that key. They stay out
    /// until they present another, so the trusted set settles instead of re-keying.
    unwrappable: HashMap<UserId, PublicKey>,
    /// Whether each peer of the room state last applied signed its encryption key, for the
    /// keys it presented, so a presence change doesn't verify everyone again.
    checked_signatures: HashMap<UserId, (PublicKey, SignedEncryptionKey, bool)>,
    #[cfg(test)]
    signature_checks: usize,
    send_keys: HashMap<MediaId, Arc<TrackKey>>,
    received: HashMap<(UserId, Epoch), Received>,
    recv_keys: HashMap<(MediaId, Epoch), (UserId, Arc<TrackKey>)>,
    /// Sender keys that arrived before the room state they belong to.
    early: HashMap<(UserId, Epoch), (Instant, Vec<u8>)>,
    /// The newest room state for a room we haven't been placed in yet.
    stashed: Option<(RoomId, Epoch, Vec<Peer>)>,
    generation: u64,
    /// Every identity key that became trusted since they were last taken, in order. A key
    /// trusted only between two looks at the room is still in here.
    newly_trusted: Vec<(UserId, PublicKey)>,
}

impl KeyManager {
    pub fn new(
        me: UserId,
        encryption: EncryptionKeyPair,
        server: Fingerprint,
        floors: EpochFloors,
    ) -> Self {
        Self {
            me,
            encryption,
            retired_encryption: None,
            server,
            present: HashMap::new(),
            room_peers: Vec::new(),
            room: None,
            epoch: Epoch(0),
            epoch_since: Instant::now(),
            floors,
            floor: None,
            my_tracks: None,
            peers: BTreeMap::new(),
            owners: HashMap::new(),
            my_secret: None,
            wrapped_to: BTreeSet::new(),
            unwrappable: HashMap::new(),
            checked_signatures: HashMap::new(),
            #[cfg(test)]
            signature_checks: 0,
            send_keys: HashMap::new(),
            received: HashMap::new(),
            recv_keys: HashMap::new(),
            early: HashMap::new(),
            stashed: None,
            generation: 0,
            newly_trusted: Vec::new(),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn room(&self) -> Option<RoomId> {
        self.room
    }

    /// Records `user`'s presence and the room it shows them in, unless that would hold more
    /// than [`MAX_SIGNED_IN`] people. True when that changes who is shown in our room, so the
    /// current room state should be applied again.
    pub fn set_presence(&mut self, user: UserId, room: Option<RoomId>) -> bool {
        if self.present.len() >= MAX_SIGNED_IN && !self.present.contains_key(&user) {
            return false;
        }
        let before = self.present.insert(user, room).flatten();
        self.changes_our_room(user, before, room)
    }

    /// Forgets `user`, whose presence ended. True as for [`Self::set_presence`].
    pub fn end_presence(&mut self, user: UserId) -> bool {
        let before = self.present.remove(&user).flatten();
        self.changes_our_room(user, before, None)
    }

    fn changes_our_room(&self, user: UserId, before: Option<RoomId>, now: Option<RoomId>) -> bool {
        user != self.me && self.room.is_some() && (before == self.room) != (now == self.room)
    }

    /// The identity keys that became trusted since this was last called, oldest first.
    pub fn take_newly_trusted(&mut self) -> Vec<(UserId, PublicKey)> {
        std::mem::take(&mut self.newly_trusted)
    }

    /// Applies the room state last applied again, after a presence changed.
    pub fn reapply_peers(&mut self, now: Instant) -> (Option<Request>, Vec<KeyNotice>) {
        let Some(room) = self.room else {
            return (None, Vec::new());
        };
        let peers = self.room_peers.clone();
        self.peers_changed(room, self.epoch, &peers, now)
    }

    /// The peers our key is wrapped to, as the server described them.
    pub fn trusted_room_peers(&self) -> Vec<Peer> {
        self.room_peers
            .iter()
            .filter(|p| self.peers.contains_key(&p.user))
            .cloned()
            .collect()
    }

    /// Switches to a renewed encryption key, keeping the old one for a short while to
    /// unwrap secrets that peers sealed to it before they learn the new one.
    pub fn replace_encryption(&mut self, encryption: EncryptionKeyPair, now: Instant) {
        let retired = std::mem::replace(&mut self.encryption, encryption);
        self.retired_encryption = Some((retired, now + RETIRED_ENCRYPTION_KEPT));
    }

    /// Changes whenever a receive key may have become available.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Applies our placement after `JoinRoom` or `LeaveRoom`. Returns the `PublishSenderKey`
    /// requests to send.
    pub fn apply_placement(
        &mut self,
        room: Option<RoomId>,
        epoch: Epoch,
        tracks: Option<Tracks>,
        peers: &[Peer],
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
            self.unwrappable.clear();
            self.epoch = Epoch(0);
            self.floor = room.and_then(|room| self.floors.get(self.me, room));
        }
        let mut notices = Vec::new();
        if room.is_some() && self.below_floor(epoch) {
            warn!(
                room = room.map(|r| r.0),
                "placed in an epoch no newer than one we took part in; its keys are refused"
            );
            notices.push(KeyNotice::OldEpoch);
        }
        if tracks.as_ref().is_some_and(too_many_shares) {
            warn!("placed with more shares than a room holds; sending nothing");
            notices.push(KeyNotice::TooManyShares);
        }
        self.room = room;
        self.my_tracks = tracks.filter(|tracks| !too_many_shares(tracks));
        self.my_secret = None;
        self.send_keys.clear();
        self.wrapped_to.clear();
        let Some(room) = room else {
            self.stashed = None;
            return (Vec::new(), notices);
        };
        let (first, more) = self.peers_changed(room, epoch, peers, now);
        notices.extend(more);
        let mut requests: Vec<Request> = first.into_iter().collect();
        if let Some((stashed_room, stashed_epoch, stashed_peers)) = self.stashed.take()
            && stashed_room == room
            && stashed_epoch > epoch
        {
            let (later, more) = self.peers_changed(room, stashed_epoch, &stashed_peers, now);
            requests.extend(later);
            notices.extend(more);
        }
        let mut unique = Vec::new();
        for notice in notices {
            if !unique.contains(&notice) {
                unique.push(notice);
            }
        }
        (requests, unique)
    }

    /// Applies a `PeersChanged`. Our secret is replaced whenever the epoch moves or the set of
    /// trusted peers and their keys differs from the one it was wrapped to, so a peer added,
    /// removed or presenting other keys without an epoch bump never holds a secret from
    /// before or after its membership. A peer whose identity key changed counts as departed,
    /// so the secrets its old key holder sent expire. A state that reuses a media id trusts
    /// nobody.
    pub fn peers_changed(
        &mut self,
        room: RoomId,
        epoch: Epoch,
        peers: &[Peer],
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
        let invalid = if peers.len() > MAX_ROOM_MEMBERS {
            Some(KeyNotice::TooManyPeers)
        } else if peers.iter().any(|p| too_many_shares(&p.tracks)) {
            Some(KeyNotice::TooManyShares)
        } else if self.clashing_tracks(peers) {
            Some(KeyNotice::ClashingTracks)
        } else {
            None
        };
        let (trusted, mut notices) = match invalid {
            Some(notice) => {
                warn!(
                    room = room.0,
                    ?notice,
                    "invalid room state; trusting nobody"
                );
                let repeated = self.room_peers == peers;
                (
                    BTreeMap::new(),
                    if repeated { vec![] } else { vec![notice] },
                )
            }
            None => self.trusted_peers(room, peers),
        };
        self.room_peers = peers.to_vec();
        let departed: Vec<UserId> = self
            .peers
            .iter()
            .filter(|(user, old)| {
                trusted
                    .get(*user)
                    .is_none_or(|new| new.identity_key != old.identity_key)
            })
            .map(|(user, _)| *user)
            .collect();
        self.retire_received(now, |from, _| departed.contains(&from));
        for (user, peer) in &trusted {
            let known = self.peers.get(user).map(|old| old.identity_key);
            if known != Some(peer.identity_key) {
                self.newly_trusted.push((*user, peer.identity_key));
            }
        }
        self.peers = trusted;
        self.owners = self
            .peers
            .iter()
            .flat_map(|(user, peer)| track_ids(&peer.tracks).map(|id| (id, *user)))
            .collect();

        let peer_set: BTreeSet<(UserId, PublicKey)> = self
            .peers
            .iter()
            .map(|(u, p)| (*u, p.encryption_key))
            .collect();
        if epoch != self.epoch || self.my_secret.is_none() || peer_set != self.wrapped_to {
            self.start_epoch(epoch, now);
        }
        let (request, unwrappable) = self.wrap_for_new_peers(room);
        notices.extend(
            unwrappable
                .into_iter()
                .map(|user| KeyNotice::BadEncryptionKey { user }),
        );
        for ((from, early_epoch), (arrived, wrapped)) in std::mem::take(&mut self.early) {
            if keeps_early(
                &self.room_peers,
                self.epoch,
                from,
                early_epoch,
                arrived,
                now,
            ) {
                self.sender_key_at(from, early_epoch, wrapped, arrived, now)
                    .ok();
            }
        }
        self.generation += 1;
        (request, notices)
    }

    /// True when a user or a media id appears twice, counting our own tracks.
    fn clashing_tracks(&self, peers: &[Peer]) -> bool {
        let mut users = HashSet::new();
        let mut ids = HashSet::new();
        let others = peers.iter().filter(|p| p.user != self.me);
        let theirs = others.clone().flat_map(|p| track_ids(&p.tracks));
        let mine = self.my_tracks.iter().flat_map(track_ids);
        !others.clone().all(|p| users.insert(p.user))
            || !mine.chain(theirs).all(|id| ids.insert(id))
    }

    /// Peers shown in `room` by their presence whose encryption key is current and validly
    /// signed by the identity key they present, leaving out those HPKE refused before.
    fn trusted_peers(
        &mut self,
        room: RoomId,
        peers: &[Peer],
    ) -> (BTreeMap<UserId, KeyPeer>, Vec<KeyNotice>) {
        let listed: HashSet<UserId> = peers.iter().map(|p| p.user).collect();
        self.checked_signatures
            .retain(|user, _| listed.contains(user));
        self.unwrappable.retain(|user, _| listed.contains(user));
        let mut notices = Vec::new();
        let mut trusted = BTreeMap::new();
        let unix_now = crate::unix_now().unwrap_or(0);
        let shown = |p: &&Peer| p.user != self.me && self.present.get(&p.user) == Some(&Some(room));
        let shown: Vec<&Peer> = peers.iter().filter(shown).collect();
        for peer in shown {
            let signed = &peer.encryption_key;
            if signed.not_after <= unix_now || !self.signed_by_identity(peer) {
                notices.push(KeyNotice::BadEncryptionKey { user: peer.user });
                continue;
            }
            if self.unwrappable.get(&peer.user) == Some(&signed.key) {
                continue;
            }
            trusted.insert(
                peer.user,
                KeyPeer {
                    identity_key: peer.identity_key,
                    encryption_key: signed.key,
                    tracks: peer.tracks.clone(),
                },
            );
        }
        (trusted, notices)
    }

    /// Whether `peer`'s encryption key is signed by its identity key, for this server and user.
    fn signed_by_identity(&mut self, peer: &Peer) -> bool {
        let presented = (peer.identity_key, peer.encryption_key);
        if let Some((identity_key, encryption_key, holds)) = self.checked_signatures.get(&peer.user)
            && (*identity_key, *encryption_key) == presented
        {
            return *holds;
        }
        #[cfg(test)]
        {
            self.signature_checks += 1;
        }
        let signed = &peer.encryption_key;
        let message = identity::encryption_key_message(
            &self.server.0,
            peer.user,
            signed.not_after,
            &signed.key,
        );
        let holds = identity::verify(&peer.identity_key, &message, &signed.sig).is_ok();
        self.checked_signatures
            .insert(peer.user, (peer.identity_key, peer.encryption_key, holds));
        holds
    }

    fn start_epoch(&mut self, epoch: Epoch, now: Instant) {
        if epoch != self.epoch {
            self.retire_received(now, |_, old| old < epoch);
            self.epoch = epoch;
            self.epoch_since = now;
        }
        // The floor covers the epoch a stay starts at. A room change resets our epoch to 0,
        // and a stay at epoch 0 needs a floor too, or it could be replayed.
        if let Some(room) = self.room {
            self.floors.raise(self.me, room, epoch);
        }
        self.wrapped_to.clear();
        let Some(tracks) = &self.my_tracks else {
            self.my_secret = None;
            self.send_keys.clear();
            return;
        };
        let secret = Zeroizing::new(random::<32>());
        self.send_keys = track_ids(tracks)
            .map(|id| (id, Arc::new(TrackKey::derive(&secret, id))))
            .collect();
        self.my_secret = Some(secret);
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

    /// True for an epoch whose sender keys and media can only be a replay: one at or before
    /// the floor, or before the one preceding ours.
    fn below_floor(&self, epoch: Epoch) -> bool {
        self.floor.is_some_and(|floor| epoch <= floor) || epoch.0.saturating_add(1) < self.epoch.0
    }

    /// Wraps our secret to the peers that lack it. Returns the request, and the peers whose
    /// key HPKE refused, who are left out from now on.
    fn wrap_for_new_peers(&mut self, room: RoomId) -> (Option<Request>, Vec<UserId>) {
        let Some(secret) = self.my_secret.as_ref() else {
            return (None, Vec::new());
        };
        let mut wrapped = Vec::new();
        let mut refused = Vec::new();
        for (user, peer) in &self.peers {
            if self.wrapped_to.contains(&(*user, peer.encryption_key)) {
                continue;
            }
            let context = self.context(room, self.epoch, self.me, *user);
            match hpke::wrap(&self.encryption, &peer.encryption_key, &context, secret) {
                Ok(blob) => wrapped.push((*user, blob)),
                Err(e) => {
                    warn!(
                        user = user.0,
                        "could not wrap a sender key; leaving them out: {e}"
                    );
                    refused.push((*user, peer.encryption_key));
                }
            }
        }
        for (user, encryption_key) in &refused {
            self.unwrappable.insert(*user, *encryption_key);
            self.peers.remove(user);
            self.owners.retain(|_, owner| owner != user);
        }
        let peers = &self.peers;
        self.wrapped_to.extend(
            wrapped
                .iter()
                .filter_map(|(user, _)| Some((*user, peers.get(user)?.encryption_key))),
        );
        let request = (!wrapped.is_empty()).then_some(Request::PublishSenderKey {
            epoch: self.epoch,
            wrapped,
        });
        (request, refused.into_iter().map(|(user, _)| user).collect())
    }

    /// Stores a peer's sender secret, holding an early one as [`keeps_early`] says. One for an
    /// earlier epoch is kept only within the grace period after our epoch moved on.
    pub fn sender_key(
        &mut self,
        from: UserId,
        epoch: Epoch,
        wrapped: &[u8],
        now: Instant,
    ) -> Result<(), KeyError> {
        if wrapped.len() != WRAPPED_BYTES {
            return Err(KeyError::Length);
        }
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
        if self.below_floor(epoch) {
            return Err(KeyError::Stale);
        }
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
        let encryption_key = self
            .peers
            .get(&from)
            .map(|p| p.encryption_key)
            .ok_or(KeyError::Stale)?;
        let context = self.context(room, epoch, from, self.me);
        let secret =
            hpke::unwrap(&self.encryption, &encryption_key, &context, &wrapped).or_else(|e| {
                match &self.retired_encryption {
                    Some((retired, _)) => {
                        hpke::unwrap(retired, &encryption_key, &context, &wrapped)
                    }
                    None => Err(e),
                }
            })?;
        self.retire_received(now, |user, old| user == from && old < epoch);
        self.received
            .insert((from, epoch), Received { secret, retire_at });
        self.recv_keys
            .retain(|(_, e), (owner, _)| !(*owner == from && *e == epoch));
        self.generation += 1;
        Ok(())
    }

    /// Starts the grace period of the received secrets `which` picks, unless one has started.
    fn retire_received(&mut self, now: Instant, which: impl Fn(UserId, Epoch) -> bool) {
        for (&(from, epoch), received) in &mut self.received {
            if which(from, epoch) && received.retire_at.is_none() {
                received.retire_at = Some(now + OLD_KEY_GRACE);
            }
        }
    }

    /// The key and epoch to send on one of our tracks.
    pub fn send_key(&self, media_id: MediaId) -> Option<(Epoch, Arc<TrackKey>)> {
        Some((self.epoch, self.send_keys.get(&media_id)?.clone()))
    }

    /// Who sends on `media_id`, among trusted peers.
    fn owner(&self, media_id: MediaId) -> Option<UserId> {
        self.owners.get(&media_id).copied()
    }

    pub fn peer_tracks(&self, user: UserId) -> Option<&Tracks> {
        self.peers.get(&user).map(|p| &p.tracks)
    }

    /// The key for `media_id` at `epoch`, from the secret of whoever the room state last
    /// applied says sends on it. A cached key counts only while that is still who it was
    /// derived for, so a server can't move one peer's media id to another.
    pub fn recv_key(&mut self, media_id: MediaId, epoch: Epoch) -> Option<Arc<TrackKey>> {
        if self.below_floor(epoch) {
            return None;
        }
        let owner = self.owner(media_id)?;
        if let Some((cached_for, key)) = self.recv_keys.get(&(media_id, epoch))
            && *cached_for == owner
        {
            return Some(key.clone());
        }
        let received = self.received.get(&(owner, epoch))?;
        let key = Arc::new(TrackKey::derive(&received.secret, media_id));
        self.recv_keys
            .insert((media_id, epoch), (owner, key.clone()));
        Some(key)
    }

    /// Zeroizes keys whose grace period has ended, and drops stale early keys.
    pub fn tick(&mut self, now: Instant) {
        if self
            .retired_encryption
            .as_ref()
            .is_some_and(|(_, until)| *until <= now)
        {
            self.retired_encryption = None;
        }
        self.received
            .retain(|_, r| r.retire_at.is_none_or(|at| at > now));
        let received = &self.received;
        self.recv_keys
            .retain(|(_, epoch), (owner, _)| received.contains_key(&(*owner, *epoch)));
        let (room_peers, epoch) = (&self.room_peers, self.epoch);
        self.early.retain(|(from, early_epoch), (arrived, _)| {
            keeps_early(room_peers, epoch, *from, *early_epoch, *arrived, now)
        });
    }
}

/// Whether a sender key that came early is still kept: for [`OLD_KEY_GRACE`], and past it
/// while the room state names its sender at our epoch.
fn keeps_early(
    room_peers: &[Peer],
    epoch: Epoch,
    from: UserId,
    early_epoch: Epoch,
    arrived: Instant,
    now: Instant,
) -> bool {
    // A sender the room names may only be waiting for a client that shows people to show
    // them, and sends its key just once.
    now.duration_since(arrived) < OLD_KEY_GRACE
        || (early_epoch == epoch && room_peers.iter().any(|p| p.user == from))
}

fn too_many_shares(tracks: &Tracks) -> bool {
    tracks.shares.len() > MAX_ROOM_SHARES
}

fn track_ids(tracks: &Tracks) -> impl Iterator<Item = MediaId> + '_ {
    std::iter::once(tracks.mic).chain(tracks.shares.iter().flat_map(|s| [s.video, s.audio]))
}

#[cfg(test)]
pub(crate) mod tests {
    use throcc_crypto::identity::IdentityKey;
    use throcc_proto::frame::{Flags, Header};
    use throcc_proto::messages::ShareTracks;

    use super::*;

    const SERVER: Fingerprint = Fingerprint([5; 32]);
    const ROOM: RoomId = RoomId(1);

    pub(crate) struct Member {
        pub(crate) id: UserId,
        identity: IdentityKey,
        pub(crate) keys: KeyManager,
        encryption_public: PublicKey,
        pub(crate) tracks: Tracks,
    }

    pub(crate) fn member(id: u64) -> Member {
        let encryption = EncryptionKeyPair::generate();
        let first = id as u32 * 10;
        Member {
            id: UserId(id),
            identity: IdentityKey::generate(),
            encryption_public: encryption.public(),
            keys: KeyManager::new(UserId(id), encryption, SERVER, EpochFloors::default()),
            tracks: Tracks {
                mic: MediaId(first),
                shares: vec![ShareTracks {
                    video: MediaId(first + 1),
                    audio: MediaId(first + 2),
                }],
            },
        }
    }

    fn unix_now() -> u64 {
        crate::unix_now().unwrap()
    }

    fn signed(m: &Member, not_after: u64) -> SignedEncryptionKey {
        let message =
            identity::encryption_key_message(&SERVER.0, m.id, not_after, &m.encryption_public);
        SignedEncryptionKey {
            key: m.encryption_public,
            not_after,
            sig: m.identity.sign(&message),
        }
    }

    fn peer(m: &Member) -> Peer {
        Peer {
            user: m.id,
            tracks: m.tracks.clone(),
            share: None,
            identity_key: m.identity.public(),
            encryption_key: signed(m, unix_now() + 3600),
        }
    }

    /// Shows every one of `peers` in the room, as their presence would.
    fn show(keys: &mut KeyManager, peers: &[Peer]) {
        for p in peers {
            keys.set_presence(p.user, Some(ROOM));
        }
    }

    type Published = Vec<(UserId, Epoch, Vec<(UserId, Vec<u8>)>)>;

    /// Delivers a room state to every member and all resulting sender keys. Returns the
    /// keys sent, by sender.
    fn deliver(members: &mut [&mut Member], epoch: u32, peers: &[Peer], now: Instant) -> Published {
        let mut published = Vec::new();
        for m in members.iter_mut() {
            show(&mut m.keys, peers);
            let tracks = Some(m.tracks.clone());
            let requests = if m.keys.room() == Some(ROOM) {
                m.keys
                    .peers_changed(ROOM, Epoch(epoch), peers, now)
                    .0
                    .into_iter()
                    .collect()
            } else {
                m.keys
                    .apply_placement(Some(ROOM), Epoch(epoch), tracks, peers, now)
                    .0
            };
            for request in requests {
                if let Request::PublishSenderKey { epoch, wrapped } = request {
                    published.push((m.id, epoch, wrapped));
                }
            }
        }
        for (from, epoch, wrapped) in &published {
            for (to, blob) in wrapped {
                if let Some(target) = members.iter_mut().find(|m| m.id == *to) {
                    target.keys.sender_key(*from, *epoch, blob, now).unwrap();
                }
            }
        }
        published
    }

    pub(crate) fn rekey(members: &mut [&mut Member], epoch: u32, now: Instant) -> Published {
        let peers: Vec<Peer> = members.iter().map(|m| peer(m)).collect();
        deliver(members, epoch, &peers, now)
    }

    /// The secret `from` wrapped to `to` among `published`.
    fn blob(published: &Published, from: UserId, to: UserId) -> (Epoch, Vec<u8>) {
        published
            .iter()
            .filter(|(sender, ..)| *sender == from)
            .find_map(|(_, epoch, wrapped)| {
                let (_, blob) = wrapped.iter().find(|(user, _)| *user == to)?;
                Some((*epoch, blob.clone()))
            })
            .unwrap()
    }

    /// `sender`'s mic datagram, sealed under its current key.
    fn mic_datagram(sender: &Member) -> (Epoch, Vec<u8>) {
        let (epoch, key) = sender.keys.send_key(sender.tracks.mic).unwrap();
        let header = Header {
            media_id: sender.tracks.mic,
            epoch,
            seq: 0,
            flags: Flags::FIRST | Flags::LAST,
        };
        let mut sealed = Vec::new();
        key.seal(&header, &[b"hello"], &mut sealed).unwrap();
        (epoch, sealed)
    }

    fn opens(receiver: &mut Member, media_id: MediaId, (epoch, sealed): &(Epoch, Vec<u8>)) -> bool {
        let mut sealed = sealed.clone();
        receiver
            .keys
            .recv_key(media_id, *epoch)
            .is_some_and(|key| key.open(&mut sealed).is_ok())
    }

    /// Places `m` in `room` at `epoch` with `peers`, as a `JoinRoom` reply would.
    fn place(
        m: &mut Member,
        room: Option<RoomId>,
        epoch: u32,
        peers: &[Peer],
        now: Instant,
    ) -> Vec<KeyNotice> {
        show(&mut m.keys, peers);
        let tracks = room.map(|_| m.tracks.clone());
        m.keys
            .apply_placement(room, Epoch(epoch), tracks, peers, now)
            .1
    }

    #[test]
    fn a_cached_receive_key_does_not_follow_its_media_id_to_another_peer() {
        let now = Instant::now();
        let (mut a, mut b, mut c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b, &mut c], 1, now);
        let from_a = mic_datagram(&a);
        assert!(opens(&mut b, a.tracks.mic, &from_a));

        let mut moved_a = peer(&a);
        moved_a.tracks.mic = MediaId(99);
        let mut moved_c = peer(&c);
        moved_c.tracks.mic = a.tracks.mic;
        let peers = [moved_a, peer(&b), moved_c];
        let (request, _) = b.keys.peers_changed(ROOM, Epoch(1), &peers, now);
        assert_eq!(request, None, "the trusted set didn't change");
        assert_eq!(b.keys.owner(a.tracks.mic), Some(c.id));
        assert!(
            !opens(&mut b, a.tracks.mic, &from_a),
            "a's media played as c's"
        );
    }

    #[test]
    fn sender_keys_and_media_of_an_epoch_left_behind_are_refused_after_the_lobby() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let recorded = blob(&rekey(&mut [&mut a, &mut b], 5, now), a.id, b.id);
        assert_eq!(recorded.1.len(), WRAPPED_BYTES);
        let datagram = mic_datagram(&a);
        rekey(&mut [&mut a, &mut b], 9, now);

        place(&mut b, None, 0, &[], now);
        let peers = [peer(&a), peer(&b)];
        place(&mut b, Some(ROOM), 5, &peers, now);
        assert_eq!(
            b.keys.sender_key(a.id, recorded.0, &recorded.1, now),
            Err(KeyError::Stale)
        );
        assert!(!opens(&mut b, a.tracks.mic, &datagram));
    }

    #[test]
    fn sender_keys_of_an_epoch_left_behind_are_refused_after_a_reconnect() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let recorded = blob(&rekey(&mut [&mut a, &mut b], 5, now), a.id, b.id);
        rekey(&mut [&mut a, &mut b], 9, now);
        let peers = [peer(&a), peer(&b)];
        let reconnect = |b: &mut Member, floors: EpochFloors| {
            let encryption = EncryptionKeyPair::from_secret(&b.keys.encryption.secret()).unwrap();
            b.keys = KeyManager::new(b.id, encryption, SERVER, floors);
            place(b, Some(ROOM), 5, &peers, now);
            b.keys.sender_key(a.id, recorded.0, &recorded.1, now)
        };

        let floors = b.keys.floors.clone();
        assert_eq!(reconnect(&mut b, floors), Err(KeyError::Stale));
        assert_eq!(
            reconnect(&mut b, EpochFloors::default()),
            Ok(()),
            "only the floor refuses it"
        );
    }

    #[test]
    fn the_last_epoch_before_the_lobby_is_refused_when_placed_just_past_it() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 5, now);
        let recorded = blob(&rekey(&mut [&mut a, &mut b], 9, now), a.id, b.id);
        let datagram = mic_datagram(&a);

        place(&mut b, None, 0, &[], now);
        let peers = [peer(&a), peer(&b)];
        assert!(!place(&mut b, Some(ROOM), 10, &peers, now).contains(&KeyNotice::OldEpoch));
        assert_eq!(
            b.keys.sender_key(a.id, recorded.0, &recorded.1, now),
            Err(KeyError::Stale)
        );
        assert!(!opens(&mut b, a.tracks.mic, &datagram));
    }

    #[test]
    fn the_last_epoch_before_a_reconnect_is_refused_when_placed_just_past_it() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let recorded = blob(&rekey(&mut [&mut a, &mut b], 9, now), a.id, b.id);
        let datagram = mic_datagram(&a);

        let encryption = EncryptionKeyPair::from_secret(&b.keys.encryption.secret()).unwrap();
        b.keys = KeyManager::new(b.id, encryption, SERVER, b.keys.floors.clone());
        let peers = [peer(&a), peer(&b)];
        place(&mut b, Some(ROOM), 10, &peers, now);
        assert_eq!(
            b.keys.sender_key(a.id, recorded.0, &recorded.1, now),
            Err(KeyError::Stale)
        );
        assert!(!opens(&mut b, a.tracks.mic, &datagram));
    }

    #[test]
    fn a_placement_at_or_before_the_floor_is_told() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 9, now);
        let peers = [peer(&a), peer(&b)];
        for epoch in [9, 3] {
            place(&mut b, None, 0, &[], now);
            assert!(place(&mut b, Some(ROOM), epoch, &peers, now).contains(&KeyNotice::OldEpoch));
        }
        place(&mut b, None, 0, &[], now);
        assert!(!place(&mut b, Some(ROOM), 11, &peers, now).contains(&KeyNotice::OldEpoch));
    }

    #[test]
    fn only_the_epoch_just_before_ours_is_accepted_late() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let first = blob(&rekey(&mut [&mut a, &mut b], 1, now), a.id, b.id);
        let second = blob(&rekey(&mut [&mut a, &mut b], 2, now), a.id, b.id);
        rekey(&mut [&mut a, &mut b], 3, now);
        assert_eq!(b.keys.sender_key(a.id, second.0, &second.1, now), Ok(()));
        assert_eq!(
            b.keys.sender_key(a.id, first.0, &first.1, now),
            Err(KeyError::Stale)
        );
        assert!(!can_read(&mut b, &a, 1));
    }

    #[test]
    fn a_sender_key_of_the_wrong_length_is_refused_before_it_is_kept() {
        let now = Instant::now();
        let mut b = member(2);
        for len in [0, WRAPPED_BYTES - 1, WRAPPED_BYTES + 1, 1 << 20] {
            assert_eq!(
                b.keys.sender_key(UserId(1), Epoch(1), &vec![0; len], now),
                Err(KeyError::Length)
            );
        }
        assert!(b.keys.early.is_empty());
        assert_eq!(
            b.keys
                .sender_key(UserId(1), Epoch(1), &[0; WRAPPED_BYTES], now),
            Ok(())
        );
        assert_eq!(b.keys.early.len(), 1);
    }

    #[test]
    fn a_peer_whose_key_cannot_be_wrapped_to_is_left_out_without_re_keying() {
        let now = Instant::now();
        let (mut a, mut b, mut m) = (member(1), member(2), member(3));
        m.encryption_public = [0; 32];
        let peers = [peer(&a), peer(&b), peer(&m)];
        deliver(&mut [&mut a, &mut b], 1, &peers, now);
        let first = a.keys.send_key(a.tracks.mic).unwrap().1;
        assert!(a.keys.trusted_room_peers().iter().all(|p| p.user != m.id));

        for _ in 0..3 {
            show(&mut a.keys, &peers);
            let (request, notices) = a.keys.peers_changed(ROOM, Epoch(1), &peers, now);
            assert_eq!(request, None);
            assert!(!notices.contains(&KeyNotice::BadEncryptionKey { user: m.id }));
        }
        let still = a.keys.send_key(a.tracks.mic).unwrap().1;
        assert!(Arc::ptr_eq(&first, &still), "the secret was replaced");
        assert!(can_read(&mut b, &a, 1));

        rekey(&mut [&mut a, &mut b], 2, now);
        assert!(a.keys.unwrappable.is_empty(), "kept for someone not listed");
    }

    /// `newcomer` joins `present` at `epoch`, with events in the server's order: the
    /// members already there get the room state, then everyone gets the newcomer's
    /// presence. Returns the `PublishSenderKey`s sent, in order, by sender. None are delivered.
    pub(crate) fn join_in_server_order(
        present: &mut [&mut Member],
        newcomer: &mut Member,
        epoch: u32,
        now: Instant,
    ) -> Vec<(UserId, Request)> {
        let mut peers: Vec<Peer> = present.iter().map(|m| peer(m)).collect();
        peers.push(peer(newcomer));
        let mut published = Vec::new();
        for m in present.iter_mut() {
            let (request, _) = m.keys.peers_changed(ROOM, Epoch(epoch), &peers, now);
            published.extend(request.map(|r| (m.id, r)));
            assert!(m.keys.set_presence(newcomer.id, Some(ROOM)));
            let (request, _) = m.keys.reapply_peers(now);
            published.extend(request.map(|r| (m.id, r)));
        }
        show(&mut newcomer.keys, &peers);
        let tracks = Some(newcomer.tracks.clone());
        let (requests, _) =
            newcomer
                .keys
                .apply_placement(Some(ROOM), Epoch(epoch), tracks, &peers, now);
        published.extend(requests.into_iter().map(|r| (newcomer.id, r)));
        published
    }

    /// The secret `from` wrapped for `to` in a `PublishSenderKey`.
    pub(crate) fn wrapped_for(request: &Request, to: UserId) -> Option<(Epoch, Vec<u8>)> {
        match request {
            Request::PublishSenderKey { epoch, wrapped } => wrapped
                .iter()
                .find(|(user, _)| *user == to)
                .map(|(_, blob)| (*epoch, blob.clone())),
            _ => None,
        }
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

    /// The newcomer is in the room state before its presence shows it in the room, so each
    /// member already there replaces its secret a second time within the epoch.
    #[test]
    fn a_join_in_the_servers_order_replaces_each_secret_twice_in_the_epoch() {
        let now = Instant::now();
        let (mut a, mut b, mut n) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);
        let published = join_in_server_order(&mut [&mut a, &mut b], &mut n, 2, now);
        let from_a: Vec<Vec<UserId>> = published
            .iter()
            .filter(|(from, _)| *from == a.id)
            .map(|(_, request)| wrapped_to(Some(request)))
            .collect();
        assert_eq!(from_a, [vec![b.id], vec![b.id, n.id]]);

        let (epoch, first) = wrapped_for(&published[0].1, b.id).unwrap();
        b.keys.sender_key(a.id, epoch, &first, now).unwrap();
        let held = b.keys.recv_key(a.tracks.mic, epoch).unwrap();
        let (_, sending) = a.keys.send_key(a.tracks.mic).unwrap();
        let header = Header {
            media_id: a.tracks.mic,
            epoch,
            seq: 0,
            flags: Flags::FIRST | Flags::LAST,
        };
        let mut sealed = Vec::new();
        sending.seal(&header, &[b"hello"], &mut sealed).unwrap();
        assert!(
            held.open(&mut sealed).is_err(),
            "b already holds the secret a sends with"
        );
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
        let (requests, _) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        let Some(Request::PublishSenderKey { wrapped, .. }) = requests.into_iter().next() else {
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
    fn substituted_or_expired_encryption_keys_are_rejected() {
        let now = Instant::now();
        let (mut a, b, c) = (member(1), member(2), member(3));
        let mut forged = peer(&b);
        forged.encryption_key.key = EncryptionKeyPair::generate().public();
        let mut expired = peer(&c);
        expired.encryption_key = signed(&c, unix_now() - 1);
        let peers = vec![peer(&a), forged, expired];
        show(&mut a.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, notices) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert!(notices.contains(&KeyNotice::BadEncryptionKey { user: b.id }));
        assert!(notices.contains(&KeyNotice::BadEncryptionKey { user: c.id }));
        assert!(requests.is_empty());
        assert!(a.keys.trusted_room_peers().is_empty());
    }

    #[test]
    fn a_new_identity_key_that_signed_its_encryption_key_gets_a_fresh_secret() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let before = a.keys.send_key(a.tracks.mic).unwrap().1;
        let from_old_b = mic_datagram(&b);
        assert!(opens(&mut a, b.tracks.mic, &from_old_b));

        let mut newcomer = member(2);
        newcomer.tracks = b.tracks.clone();
        let peers = vec![peer(&a), peer(&newcomer)];
        let (request, notices) = a.keys.peers_changed(ROOM, Epoch(1), &peers, now);
        assert_eq!(notices, []);
        assert_eq!(wrapped_to(request.as_ref()), [b.id]);
        let after = a.keys.send_key(a.tracks.mic).unwrap().1;
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the old key holder still has our secret"
        );
        a.keys.tick(now + OLD_KEY_GRACE);
        assert!(
            !opens(&mut a, b.tracks.mic, &from_old_b),
            "the old key holder's media still plays"
        );
    }

    #[test]
    fn an_identity_key_that_did_not_sign_the_encryption_key_is_left_out() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        rekey(&mut [&mut a, &mut b], 1, now);
        let mut swapped = peer(&b);
        swapped.identity_key = IdentityKey::generate().public();
        let (request, notices) = a
            .keys
            .peers_changed(ROOM, Epoch(2), &[peer(&a), swapped], now);
        assert_eq!(request, None);
        assert_eq!(notices, [KeyNotice::BadEncryptionKey { user: b.id }]);
        assert!(a.keys.trusted_room_peers().is_empty());
    }

    #[test]
    fn sender_key_before_its_peers_is_applied_later() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        show(&mut b.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, _) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        let Some(Request::PublishSenderKey { epoch, wrapped }) = requests.into_iter().next() else {
            panic!()
        };
        b.keys.sender_key(a.id, epoch, &wrapped[0].1, now).unwrap();
        assert!(!can_read(&mut b, &a, 1));
        let tracks = Some(b.tracks.clone());
        b.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert!(can_read(&mut b, &a, 1));
    }

    #[test]
    fn peers_before_placement_are_kept_and_a_stale_placement_is_ignored() {
        let now = Instant::now();
        let (mut a, b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        a.keys.peers_changed(ROOM, Epoch(3), &peers, now);
        let tracks = Some(a.tracks.clone());
        a.keys
            .apply_placement(Some(ROOM), Epoch(2), tracks.clone(), &peers, now);
        assert_eq!(a.keys.epoch(), Epoch(3));
        let (requests, _) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert!(requests.is_empty());
        assert_eq!(a.keys.epoch(), Epoch(3));
        assert!(a.keys.send_key(a.tracks.mic).is_some());
    }

    fn wrapped_to(request: Option<&Request>) -> Vec<UserId> {
        match request {
            Some(Request::PublishSenderKey { wrapped, .. }) => {
                wrapped.iter().map(|(u, _)| *u).collect()
            }
            _ => Vec::new(),
        }
    }

    fn reapply(m: &mut Member, now: Instant) -> (Option<Request>, Vec<KeyNotice>) {
        m.keys.reapply_peers(now)
    }

    #[test]
    fn a_peer_without_presence_in_the_room_never_gets_our_key() {
        let now = Instant::now();
        let (mut a, b, hidden) = (member(1), member(2), member(3));
        let peers = vec![peer(&a), peer(&b), peer(&hidden)];
        show(&mut a.keys, &peers[..2]);
        let tracks = Some(a.tracks.clone());
        let (requests, notices) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert_eq!(wrapped_to(requests.first()), [b.id]);
        assert_eq!(notices, []);
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
    fn peers_reusing_a_media_id_trust_nobody() {
        let now = Instant::now();
        let (mut a, mut b, c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);
        assert_eq!(a.keys.owner(b.tracks.mic), Some(b.id));

        for stolen in [b.tracks.mic, a.tracks.mic] {
            let mut forged = peer(&c);
            forged.tracks.shares[0].video = stolen;
            let peers = vec![peer(&a), peer(&b), forged];
            show(&mut a.keys, &peers);
            let (request, notices) = a.keys.peers_changed(ROOM, Epoch(2), &peers, now);
            assert_eq!(request, None);
            assert_eq!(notices, [KeyNotice::ClashingTracks]);
            assert_eq!(a.keys.owner(b.tracks.mic), None);
            assert_eq!(a.keys.owner(stolen), None);
            assert!(a.keys.trusted_room_peers().is_empty());
        }
    }

    fn shares_past_the_limit(first: u32) -> Vec<ShareTracks> {
        (first..)
            .step_by(2)
            .take(MAX_ROOM_SHARES + 1)
            .map(|id| ShareTracks {
                video: MediaId(id),
                audio: MediaId(id + 1),
            })
            .collect()
    }

    #[test]
    fn a_peer_with_too_many_shares_trusts_nobody() {
        let now = Instant::now();
        let (mut a, mut b, c) = (member(1), member(2), member(3));
        rekey(&mut [&mut a, &mut b], 1, now);

        let mut crowded = peer(&c);
        crowded.tracks.shares = shares_past_the_limit(1_000);
        let peers = vec![peer(&a), peer(&b), crowded];
        show(&mut a.keys, &peers);
        let (request, notices) = a.keys.peers_changed(ROOM, Epoch(2), &peers, now);
        assert_eq!(request, None);
        assert_eq!(notices, [KeyNotice::TooManyShares]);
        assert_eq!(a.keys.owner(b.tracks.mic), None);
        assert!(a.keys.trusted_room_peers().is_empty());
    }

    #[test]
    fn a_placement_with_too_many_shares_sends_nothing() {
        let now = Instant::now();
        let (mut a, b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        let mut tracks = a.tracks.clone();
        tracks.shares = shares_past_the_limit(1_000);
        let (requests, notices) =
            a.keys
                .apply_placement(Some(ROOM), Epoch(1), Some(tracks), &peers, now);
        assert_eq!(requests, []);
        assert_eq!(notices, [KeyNotice::TooManyShares]);
        assert!(a.keys.send_key(a.tracks.mic).is_none());
        assert!(a.keys.send_key(MediaId(1_000)).is_none());
    }

    #[test]
    fn too_many_shares_in_our_placement_and_our_peer_entry_is_told_once() {
        let now = Instant::now();
        let (mut a, b) = (member(1), member(2));
        let mut crowded = a.tracks.clone();
        crowded.shares = shares_past_the_limit(1_000);
        let mut us = peer(&a);
        us.tracks = crowded.clone();
        let peers = vec![us, peer(&b)];
        show(&mut a.keys, &peers);
        let (requests, notices) =
            a.keys
                .apply_placement(Some(ROOM), Epoch(1), Some(crowded), &peers, now);
        assert_eq!(requests, []);
        assert_eq!(notices, [KeyNotice::TooManyShares]);
    }

    fn invented_peer(id: u64) -> Peer {
        peer(&member(id))
    }

    #[test]
    fn peers_over_the_limit_trust_nobody() {
        let now = Instant::now();
        let mut a = member(1);
        let mut peers = vec![peer(&a)];
        peers.extend((100..).take(MAX_ROOM_MEMBERS - 1).map(invented_peer));
        show(&mut a.keys, &peers);
        let tracks = Some(a.tracks.clone());
        let (requests, _) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert_eq!(wrapped_to(requests.first()).len(), MAX_ROOM_MEMBERS - 1);

        let crowd = invented_peer(99);
        peers.push(crowd.clone());
        show(&mut a.keys, &peers);
        let (request, notices) = a.keys.peers_changed(ROOM, Epoch(2), &peers, now);
        assert_eq!(request, None);
        assert_eq!(notices, [KeyNotice::TooManyPeers]);
        assert!(a.keys.trusted_room_peers().is_empty());
        a.keys.set_presence(UserId(100), None);
        assert_eq!(reapply(&mut a, now), (None, vec![]));
    }

    #[test]
    fn presence_changes_verify_only_keys_not_checked_before() {
        let now = Instant::now();
        let (mut a, b, c) = (member(1), member(2), member(3));
        let peers = vec![peer(&a), peer(&b), peer(&c)];
        show(&mut a.keys, &peers);
        let tracks = Some(a.tracks.clone());
        a.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert_eq!(a.keys.signature_checks, 2);
        for room in [None, Some(ROOM), None, Some(ROOM)] {
            assert!(a.keys.set_presence(c.id, room));
            reapply(&mut a, now);
        }
        assert_eq!(a.keys.signature_checks, 2);
        assert_eq!(a.keys.trusted_room_peers().len(), 2);

        let mut forged = peer(&b);
        forged.encryption_key.key = EncryptionKeyPair::generate().public();
        // The keys signed before: signing them again in the next second would change their
        // expiry, and so give them a new signature to check.
        let peers = vec![peers[0].clone(), forged, peers[2].clone()];
        let (request, notices) = a.keys.peers_changed(ROOM, Epoch(1), &peers, now);
        assert_eq!(wrapped_to(request.as_ref()), [c.id]);
        assert!(notices.contains(&KeyNotice::BadEncryptionKey { user: b.id }));
        assert_eq!(a.keys.signature_checks, 3);
    }

    #[test]
    fn a_secret_wrapped_to_the_retired_encryption_key_still_unwraps() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = vec![peer(&a), peer(&b)];
        show(&mut a.keys, &peers);
        show(&mut b.keys, &peers);
        let tracks = Some(a.tracks.clone());
        a.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        let tracks = Some(b.tracks.clone());
        let (requests, _) = b
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        let Some(Request::PublishSenderKey { epoch, wrapped }) = requests.into_iter().next() else {
            panic!()
        };
        a.keys
            .replace_encryption(EncryptionKeyPair::generate(), now);
        a.keys.sender_key(b.id, epoch, &wrapped[0].1, now).unwrap();
        assert!(can_read(&mut a, &b, 1));
    }

    #[test]
    fn the_retired_encryption_key_is_dropped_after_a_while() {
        let now = Instant::now();
        let mut a = member(1);
        a.keys
            .replace_encryption(EncryptionKeyPair::generate(), now);
        a.keys.tick(now + OLD_KEY_GRACE);
        assert!(a.keys.retired_encryption.is_some());
        a.keys.tick(now + RETIRED_ENCRYPTION_KEPT);
        assert!(a.keys.retired_encryption.is_none());
    }

    #[test]
    fn nobody_past_the_people_shown_gets_our_key() {
        let now = Instant::now();
        let (mut a, ghost) = (member(1), member(2));
        for id in 0..MAX_SIGNED_IN as u64 {
            a.keys.set_presence(UserId(1000 + id), None);
        }
        a.keys.set_presence(ghost.id, Some(ROOM));
        let peers = [peer(&a), peer(&ghost)];
        let tracks = Some(a.tracks.clone());
        let (requests, _) = a
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        assert!(requests.is_empty());
        assert!(a.keys.trusted_room_peers().is_empty());
        assert!(a.keys.take_newly_trusted().is_empty());

        a.keys.end_presence(UserId(1000));
        assert!(a.keys.set_presence(ghost.id, Some(ROOM)));
        let (request, _) = reapply(&mut a, now);
        assert_eq!(wrapped_to(request.as_ref()), [ghost.id]);
    }

    #[test]
    fn a_key_trusted_and_then_replaced_is_still_reported() {
        let now = Instant::now();
        let (mut a, b, impostor) = (member(1), member(2), member(2));
        let real = [peer(&a), peer(&b)];
        let swapped = [peer(&a), peer(&impostor)];
        show(&mut a.keys, &real);
        let tracks = Some(a.tracks.clone());
        a.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &real, now);
        assert_eq!(a.keys.take_newly_trusted(), [(b.id, b.identity.public())]);

        let (request, _) = a.keys.peers_changed(ROOM, Epoch(1), &swapped, now);
        assert_eq!(wrapped_to(request.as_ref()), [b.id]);
        a.keys.peers_changed(ROOM, Epoch(1), &real, now);
        assert_eq!(
            a.keys.take_newly_trusted(),
            [
                (b.id, impostor.identity.public()),
                (b.id, b.identity.public())
            ]
        );
        a.keys.peers_changed(ROOM, Epoch(1), &real, now);
        assert!(a.keys.take_newly_trusted().is_empty());
    }

    #[test]
    fn a_stay_at_epoch_zero_leaves_a_floor() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let key = blob(&rekey(&mut [&mut a, &mut b], 0, now), a.id, b.id);
        let peers = [peer(&a), peer(&b)];
        place(&mut b, None, 0, &[], now);
        assert!(place(&mut b, Some(ROOM), 0, &peers, now).contains(&KeyNotice::OldEpoch));
        assert_eq!(
            b.keys.sender_key(a.id, key.0, &key.1, now),
            Err(KeyError::Stale)
        );
        assert!(!can_read(&mut b, &a, 0));
    }

    #[test]
    fn a_key_from_someone_the_room_names_waits_until_they_are_shown() {
        let now = Instant::now();
        let (mut a, mut b) = (member(1), member(2));
        let peers = [peer(&a), peer(&b)];
        show(&mut b.keys, &peers);
        let tracks = Some(b.tracks.clone());
        let (requests, _) = b
            .keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        let Some(Request::PublishSenderKey { epoch, wrapped }) = requests.into_iter().next() else {
            panic!()
        };

        // Alice has the room state and Bob's key, but nothing has shown her Bob yet.
        let tracks = Some(a.tracks.clone());
        a.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        a.keys.sender_key(b.id, epoch, &wrapped[0].1, now).unwrap();
        let later = now + 10 * OLD_KEY_GRACE;
        a.keys.tick(later);
        assert!(!can_read(&mut a, &b, 1));

        assert!(a.keys.set_presence(b.id, Some(ROOM)));
        a.keys.reapply_peers(later);
        assert!(can_read(&mut a, &b, 1));
    }

    #[test]
    fn an_early_key_from_nobody_the_room_names_is_dropped_after_a_while() {
        let now = Instant::now();
        let mut a = member(1);
        let tracks = Some(a.tracks.clone());
        let peers = [peer(&a)];
        a.keys
            .apply_placement(Some(ROOM), Epoch(1), tracks, &peers, now);
        for epoch in [1, 2] {
            a.keys
                .sender_key(UserId(9), Epoch(epoch), &[0; WRAPPED_BYTES], now)
                .unwrap();
        }
        assert_eq!(a.keys.early.len(), 2);
        a.keys.tick(now + OLD_KEY_GRACE);
        assert!(a.keys.early.is_empty());
    }
}
