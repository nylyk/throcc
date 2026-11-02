use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use throcc_crypto::identity::{self, random};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, Hash, MediaId, Role, RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    ErrorCode, Event, MAX_AVATAR_BYTES, MAX_NAME_CHARS, MAX_PASSWORD_CHARS, MediaState, Placed,
    Presence, Recovery, Request, Response, Room, RoomPeer, ServerMessage, ServerProfile, Share,
    SignedEncKey, Tracks, Welcome, valid_name, valid_password,
};
use throcc_proto::permissions;
use throcc_proto::transport::close;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tracing::{info, warn};

use crate::auth::InviteRuns;
use crate::database::{Database, InviteRow, UserRow};
use crate::egress::{EgressHandle, TrackKind};
use crate::limits::Buckets;
use crate::room_password;
use crate::sfu::{ForwardingTable, KeyframeAggregator, Route, Sfu};
use crate::unix_now;

const INVITE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const PROFILE_INTERVAL: Duration = Duration::from_secs(10);
const TRACKS_PER_MEMBERSHIP: u32 = 3;
const MINT_ATTEMPTS: usize = 64;
const MAX_WATCHING: usize = 16;
/// Clients rotate encryption keys every 30 days; a day of slack covers clock skew.
const MAX_ENC_KEY_LIFETIME: Duration = Duration::from_secs(31 * 24 * 60 * 60);
const MAX_WRAPPED_BYTES: usize = 256;
/// Welcome lists every room, and must stay under the frame cap.
pub const MAX_ROOMS: usize = 500;
const REQUEST_BURST: f64 = 100.0;
const REQUESTS_PER_SECOND: f64 = 20.0;
/// Keyframe requests and picture fetches come in bursts, so they get their own larger
/// allowance, which still keeps one client from monopolising the hub.
const CHEAP_REQUEST_BURST: f64 = 1000.0;
const CHEAP_REQUESTS_PER_SECOND: f64 = 500.0;
const UPLOAD_BURST_BYTES: f64 = 1024.0 * 1024.0;
/// One full-size avatar every 10 seconds.
const UPLOAD_BYTES_PER_SECOND: f64 = MAX_AVATAR_BYTES as f64 / 10.0;
/// Bytes charged for an upload smaller than this.
const MIN_UPLOAD_COST: usize = 4096;
/// How long an uploaded picture that nothing refers to is kept.
const UNUSED_BLOB_GRACE: Duration = Duration::from_secs(10 * 60);
const PASSWORD_BURST: f64 = 5.0;
/// One password guess every 5 seconds, which also bounds the Argon2 work a user can cause.
const PASSWORDS_PER_SECOND: f64 = 0.2;

pub struct Refusal(pub ErrorCode, pub String);

impl Refusal {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(code, message.into())
    }

    fn internal(e: anyhow::Error) -> Self {
        warn!("internal error: {e:#}");
        Self(ErrorCode::Internal, "internal server error".into())
    }
}

impl From<Refusal> for Response {
    fn from(Refusal(code, message): Refusal) -> Self {
        Response::Err { code, message }
    }
}

type Outcome = Result<Response, Refusal>;

/// What [`Hub::handle`] makes of a request.
pub enum Handled {
    Done(Response),
    /// The request waits on Argon2, which must run without the hub lock: run the work,
    /// then pass its result to [`Hub::resume`].
    Pending(PasswordWork),
}

/// Argon2 work a request waits on. Takes tens of milliseconds.
pub enum PasswordWork {
    Join {
        room: RoomId,
        password: String,
        hash: String,
    },
    CreateRoom {
        name: String,
        password: String,
    },
    SetRoomPassword {
        room: RoomId,
        password: String,
    },
}

impl PasswordWork {
    pub fn run(self) -> PasswordDone {
        match self {
            Self::Join {
                room,
                password,
                hash,
            } => PasswordDone::Join {
                matched: room_password::matches(&password, &hash),
                room,
                hash,
            },
            Self::CreateRoom { name, password } => PasswordDone::CreateRoom {
                name,
                hash: room_password::hash(&password),
            },
            Self::SetRoomPassword { room, password } => PasswordDone::SetRoomPassword {
                room,
                hash: room_password::hash(&password),
            },
        }
    }
}

/// The result of [`PasswordWork`], for [`Hub::resume`].
pub enum PasswordDone {
    Join {
        room: RoomId,
        /// The hash the password was checked against.
        hash: String,
        matched: bool,
    },
    CreateRoom {
        name: String,
        hash: anyhow::Result<String>,
    },
    SetRoomPassword {
        room: RoomId,
        hash: anyhow::Result<String>,
    },
}

/// What the session writer sends next.
pub enum Outgoing {
    Message(ServerMessage),
    /// A message that keeps its share of the connection's queue budget until it is written.
    Budgeted(ServerMessage, OwnedSemaphorePermit),
    /// Sends `Kicked`, finishes the stream once the client has read it, then closes.
    Kick {
        reason: String,
        code: u32,
    },
}

/// What the hub needs to reach one live connection.
pub struct Link {
    pub outgoing: mpsc::Sender<Outgoing>,
    pub connection: quinn::Connection,
    pub egress: EgressHandle,
}

impl Link {
    fn kick(&self, reason: &str, code: u32) {
        let kick = Outgoing::Kick {
            reason: reason.to_owned(),
            code,
        };
        if self.outgoing.try_send(kick).is_err() {
            self.connection.close(code.into(), reason.as_bytes());
            return;
        }
        let connection = self.connection.clone();
        let reason = reason.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(crate::session::LINGER).await;
            connection.close(code.into(), reason.as_bytes());
        });
    }
}

struct Session {
    conn_id: u64,
    link: Link,
    row: UserRow,
    room: Option<RoomId>,
    tracks: Option<Tracks>,
    media: MediaState,
    watching: Vec<MediaId>,
    last_profile: Option<Instant>,
}

struct RoomEntry {
    name: String,
    epoch: Epoch,
    password_hash: Option<String>,
    members: BTreeSet<UserId>,
}

/// Rooms, presence and every connected session. One lock guards all of it.
pub struct Hub {
    pub db: Database,
    fingerprint: Fingerprint,
    sfu: Arc<Sfu>,
    keyframes: KeyframeAggregator,
    sessions: HashMap<UserId, Session>,
    rooms: BTreeMap<RoomId, RoomEntry>,
    next_conn: u64,
    requests: Buckets<UserId>,
    cheap_requests: Buckets<UserId>,
    uploads: Buckets<UserId>,
    password_tries: Buckets<UserId>,
    /// The room each user last moved into themselves, which a reconnect may return them to
    /// without its password.
    last_joined: HashMap<UserId, RoomId>,
    pub invite_runs: InviteRuns,
}

impl Hub {
    pub fn new(db: Database, sfu: Arc<Sfu>, fingerprint: Fingerprint) -> anyhow::Result<Self> {
        let rooms = db
            .rooms()?
            .into_iter()
            .map(|room| {
                (
                    room.id,
                    RoomEntry {
                        name: room.name,
                        epoch: room.epoch,
                        password_hash: room.password_hash,
                        members: BTreeSet::new(),
                    },
                )
            })
            .collect();
        delete_unused_blobs(&db, unix_now())?;
        Ok(Self {
            db,
            fingerprint,
            sfu,
            keyframes: KeyframeAggregator::default(),
            sessions: HashMap::new(),
            rooms,
            next_conn: 0,
            requests: Buckets::new(REQUEST_BURST, REQUESTS_PER_SECOND),
            cheap_requests: Buckets::new(CHEAP_REQUEST_BURST, CHEAP_REQUESTS_PER_SECOND),
            uploads: Buckets::new(UPLOAD_BURST_BYTES, UPLOAD_BYTES_PER_SECOND),
            password_tries: Buckets::new(PASSWORD_BURST, PASSWORDS_PER_SECOND),
            last_joined: HashMap::new(),
            invite_runs: InviteRuns::default(),
        })
    }

    /// Registers a connection that has authenticated as `row`. A previous connection of the
    /// same user is kicked. Returns the connection id and the welcome snapshot, or `None`
    /// if the user was removed after authenticating.
    pub fn connect(
        &mut self,
        row: UserRow,
        link: Link,
        want_room: Option<RoomId>,
    ) -> anyhow::Result<Option<(u64, Welcome)>> {
        let user = row.id;
        let Some(row) = self
            .db
            .user(user)?
            .filter(|current| current.pubkey == row.pubkey)
        else {
            return Ok(None);
        };
        self.evict(user, "you signed in from somewhere else");
        self.next_conn += 1;
        let conn_id = self.next_conn;
        let role = row.role;
        self.sessions.insert(
            user,
            Session {
                conn_id,
                link,
                row,
                room: None,
                tracks: None,
                media: MediaState::default(),
                watching: Vec::new(),
                last_profile: None,
            },
        );
        let placed = want_room
            .filter(|room| self.may_rejoin(user, *room))
            .and_then(|room| self.place(user, Some(room)).ok())
            .unwrap_or_else(lobby);
        let server = self.db.server_profile().unwrap_or_else(|e| {
            warn!("reading the server profile: {e:#}");
            ServerProfile::default()
        });
        let welcome = Welcome {
            me: user,
            role,
            rooms: self.room_list(),
            presence: self.sessions.values().filter_map(presence).collect(),
            placed,
            server,
        };
        Ok(Some((conn_id, welcome)))
    }

    /// Leaving the room already rebuilds the routes, and a session outside a room has none.
    pub fn disconnect(&mut self, user: UserId, conn_id: u64) {
        if self.is_current(user, conn_id) {
            self.leave_room(user);
            self.sessions.remove(&user);
        }
    }

    /// Handles a request from connection `conn_id`. A connection that has been replaced
    /// by a newer one of the same user is refused.
    pub fn handle(&mut self, user: UserId, conn_id: u64, request: Request) -> Handled {
        self.dispatch(user, conn_id, request)
            .unwrap_or_else(|refusal| Handled::Done(refusal.into()))
    }

    /// Finishes a request after its [`PasswordWork`]. The hub may have changed while the
    /// work ran, so everything the request relies on is checked again.
    pub fn resume(&mut self, user: UserId, conn_id: u64, done: PasswordDone) -> Response {
        self.finish(user, conn_id, done)
            .unwrap_or_else(Response::from)
    }

    /// The id of `user`'s current connection.
    pub fn conn_id(&self, user: UserId) -> Option<u64> {
        self.sessions.get(&user).map(|s| s.conn_id)
    }

    fn is_current(&self, user: UserId, conn_id: u64) -> bool {
        self.sessions
            .get(&user)
            .is_some_and(|s| s.conn_id == conn_id)
    }

    fn require_current(&self, user: UserId, conn_id: u64) -> Result<(), Refusal> {
        if self.is_current(user, conn_id) {
            Ok(())
        } else {
            Err(Refusal::new(
                ErrorCode::Forbidden,
                "this connection was replaced",
            ))
        }
    }

    fn dispatch(
        &mut self,
        user: UserId,
        conn_id: u64,
        request: Request,
    ) -> Result<Handled, Refusal> {
        self.require_current(user, conn_id)?;
        let cheap = matches!(
            request,
            Request::RequestKeyframe { .. } | Request::GetAvatar(_)
        );
        let bucket = if cheap {
            &mut self.cheap_requests
        } else {
            &mut self.requests
        };
        if !bucket.take(user, 1.0, Instant::now()) {
            return Err(Refusal::new(
                ErrorCode::RateLimited,
                "too many requests; slow down",
            ));
        }
        let outcome = match request {
            Request::SetRoom { room, password } => return self.join(user, room, password),
            Request::CreateRoom { name, password } => {
                return self.create_room(user, name, password);
            }
            Request::SetRoomPassword { room, password } => {
                return self.set_room_password(user, room, password);
            }
            Request::SetProfile { name, avatar } => self.set_profile(user, name, avatar),
            Request::SetAvatar(bytes) => self.set_avatar(user, bytes),
            Request::GetAvatar(hash) => self.get_avatar(hash),
            Request::SetEncKey(enc_key) => self.set_enc_key(user, enc_key),
            Request::SetMedia(media) => self.set_media(user, media),
            Request::RenameRoom { room, name } => self.rename_room(user, room, name),
            Request::DeleteRoom(room) => self.delete_room(user, room),
            Request::CreateInvite => self.create_invite(user),
            Request::ListUsers => self.list_users(user),
            Request::SetRole { user: target, role } => self.set_role(user, target, role),
            Request::RemoveUser {
                user: target,
                cascade,
            } => self.remove_user(user, target, cascade),
            Request::SetWatching(ids) => self.set_watching(user, ids),
            Request::RequestKeyframe { media_id, kind } => {
                self.request_keyframe(user, media_id, kind)
            }
            Request::PublishKey { epoch, wrapped } => self.publish_key(user, epoch, wrapped),
            Request::SetServerName(name) => self.set_server_name(user, name),
            Request::SetServerAvatar(avatar) => self.set_server_avatar(user, avatar),
            Request::KickFromRoom { user: target, room } => self.kick_from_room(user, target, room),
        };
        outcome.map(Handled::Done)
    }

    fn finish(&mut self, user: UserId, conn_id: u64, done: PasswordDone) -> Outcome {
        self.require_current(user, conn_id)?;
        match done {
            PasswordDone::Join {
                room,
                hash,
                matched,
            } => self.finish_join(user, room, &hash, matched),
            PasswordDone::CreateRoom { name, hash } => {
                let hash = hash.map_err(Refusal::internal)?;
                let name = self.room_to_create(user, &name)?;
                self.add_room(name, Some(hash))
            }
            PasswordDone::SetRoomPassword { room, hash } => {
                let hash = hash.map_err(Refusal::internal)?;
                self.may_manage_room(user, room)?;
                self.store_room_password(room, Some(hash))
            }
        }
    }

    fn evict(&mut self, user: UserId, reason: &str) {
        self.leave_room(user);
        if let Some(session) = self.sessions.remove(&user) {
            session.link.kick(reason, close::REMOVED);
        }
    }

    pub fn emit_to(&self, user: UserId, event: Event) {
        if let Some(session) = self.sessions.get(&user) {
            send(session, event);
        }
    }

    /// Sends a change of the user list to everyone who may list users.
    pub fn emit_user_list_change(&self, event: Event) {
        let listers = self.sessions.values();
        for session in listers.filter(|s| permissions::can_list_users(s.row.role)) {
            send(session, event.clone());
        }
    }

    fn emit_all(&self, event: Event) {
        for session in self.sessions.values() {
            send(session, event.clone());
        }
    }

    fn emit_room(&self, room: RoomId, event: Event, except: Option<UserId>) {
        let Some(entry) = self.rooms.get(&room) else {
            return;
        };
        for member in entry.members.iter().filter(|m| Some(**m) != except) {
            self.emit_to(*member, event.clone());
        }
    }

    fn session(&self, user: UserId) -> Result<&Session, Refusal> {
        self.sessions
            .get(&user)
            .ok_or_else(|| Refusal::new(ErrorCode::Internal, "no session"))
    }

    fn session_mut(&mut self, user: UserId) -> Result<&mut Session, Refusal> {
        self.sessions
            .get_mut(&user)
            .ok_or_else(|| Refusal::new(ErrorCode::Internal, "no session"))
    }

    fn role(&self, user: UserId) -> Result<Role, Refusal> {
        Ok(self.session(user)?.row.role)
    }

    fn current_room(&self, user: UserId) -> Result<RoomId, Refusal> {
        self.session(user)?
            .room
            .ok_or_else(|| Refusal::new(ErrorCode::Invalid, "join a room first"))
    }

    fn room_entry(&self, room: RoomId) -> Result<&RoomEntry, Refusal> {
        self.rooms.get(&room).ok_or_else(no_such_room)
    }

    fn stored_user(&self, user: UserId) -> Result<UserRow, Refusal> {
        self.db
            .user(user)
            .map_err(Refusal::internal)?
            .ok_or_else(|| Refusal::new(ErrorCode::NotFound, "no such user"))
    }

    /// Refuses a picture that hasn't been uploaded.
    fn require_picture(&self, avatar: Option<Hash>) -> Result<(), Refusal> {
        match avatar {
            Some(hash) if !self.db.has_blob(&hash).map_err(Refusal::internal)? => {
                Err(Refusal::new(ErrorCode::NotFound, "no such picture"))
            }
            _ => Ok(()),
        }
    }

    fn announce_presence(&self, user: UserId) {
        if let Some(p) = self.sessions.get(&user).and_then(presence) {
            self.emit_all(Event::PresenceChanged(p));
        }
    }

    fn room_list(&self) -> Vec<Room> {
        self.rooms
            .iter()
            .map(|(id, entry)| Room {
                id: *id,
                name: entry.name.clone(),
                locked: entry.password_hash.is_some(),
            })
            .collect()
    }

    fn peers(&self, room: RoomId) -> Vec<RoomPeer> {
        let Some(entry) = self.rooms.get(&room) else {
            return Vec::new();
        };
        entry
            .members
            .iter()
            .filter_map(|m| self.sessions.get(m))
            .filter_map(peer)
            .collect()
    }

    /// Moves each of `rooms` to its next epoch, all or none.
    fn bump_epochs(&mut self, rooms: &[RoomId]) -> Result<(), Refusal> {
        let epochs = rooms
            .iter()
            .map(|room| {
                let next = self.room_entry(*room)?.epoch.0.checked_add(1);
                let next =
                    next.ok_or_else(|| Refusal::new(ErrorCode::Internal, "room epoch exhausted"))?;
                Ok((*room, Epoch(next)))
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        self.db.set_epochs(&epochs).map_err(Refusal::internal)?;
        for (room, epoch) in epochs {
            if let Some(entry) = self.rooms.get_mut(&room) {
                entry.epoch = epoch;
            }
        }
        Ok(())
    }

    fn broadcast_room_state(&self, room: RoomId, except: Option<UserId>) {
        let Some(entry) = self.rooms.get(&room) else {
            return;
        };
        let event = Event::RoomState {
            room,
            epoch: entry.epoch,
            peers: self.peers(room),
        };
        self.emit_room(room, event, except);
    }

    fn join(
        &mut self,
        user: UserId,
        target: Option<RoomId>,
        password: Option<String>,
    ) -> Result<Handled, Refusal> {
        let needed = match target {
            Some(room) => self.password_to_enter(user, room)?.map(|hash| (room, hash)),
            None => None,
        };
        let Some((room, hash)) = needed else {
            return self.enter(user, target).map(Handled::Done);
        };
        let Some(password) = password else {
            return Err(Refusal::new(
                ErrorCode::WrongPassword,
                "this room needs a password",
            ));
        };
        // Taken before the work is handed out, so guesses can't make Argon2 run faster.
        if !self.password_tries.take(user, 1.0, Instant::now()) {
            return Err(Refusal::new(
                ErrorCode::RateLimited,
                "too many password attempts; wait a few seconds",
            ));
        }
        Ok(Handled::Pending(PasswordWork::Join {
            room,
            password,
            hash,
        }))
    }

    /// Enters `room` once a password checked against `hash` matched, if the room still
    /// asks for that same password.
    fn finish_join(&mut self, user: UserId, room: RoomId, hash: &str, matched: bool) -> Outcome {
        if !matched {
            return Err(Refusal::new(ErrorCode::WrongPassword, "wrong password"));
        }
        if self
            .password_to_enter(user, room)?
            .is_some_and(|current| current != hash)
        {
            return Err(Refusal::new(
                ErrorCode::WrongPassword,
                "the room's password has just changed",
            ));
        }
        self.enter(user, Some(room))
    }

    fn enter(&mut self, user: UserId, target: Option<RoomId>) -> Outcome {
        let placed = self.place(user, target)?;
        match target {
            Some(room) => self.last_joined.insert(user, room),
            None => self.last_joined.remove(&user),
        };
        Ok(Response::Placed(placed))
    }

    /// The hash of the password `user` needs to enter `room`, if they need one. Members
    /// already in it and those who manage rooms enter without one.
    fn password_to_enter(&self, user: UserId, room: RoomId) -> Result<Option<String>, Refusal> {
        let Some(hash) = &self.room_entry(room)?.password_hash else {
            return Ok(None);
        };
        if self.session(user)?.room == Some(room) || permissions::can_manage_rooms(self.role(user)?)
        {
            return Ok(None);
        }
        Ok(Some(hash.clone()))
    }

    /// Whether a reconnect may put `user` back into `room` without its password.
    fn may_rejoin(&self, user: UserId, room: RoomId) -> bool {
        self.rooms
            .get(&room)
            .is_some_and(|entry| entry.password_hash.is_none())
            || self
                .sessions
                .get(&user)
                .is_some_and(|s| permissions::can_manage_rooms(s.row.role))
            || self.last_joined.get(&user) == Some(&room)
    }

    /// Moves `user` to `target` in one step. On failure the user stays where they were.
    fn place(&mut self, user: UserId, target: Option<RoomId>) -> Result<Placed, Refusal> {
        let session = self.session(user)?;
        let current = session.room;
        if current == target {
            return Ok(self.placed(user));
        }
        if let Some(room) = target {
            self.room_entry(room)?;
            let now = unix_now();
            if session.row.enc_key.is_none_or(|key| key.not_after <= now) {
                return Err(Refusal::new(
                    ErrorCode::Invalid,
                    "set an encryption key before joining a room",
                ));
            }
        }

        let tracks = target.map(|_| self.allocate_tracks()).transpose()?;
        let changed: Vec<RoomId> = current.into_iter().chain(target).collect();
        self.bump_epochs(&changed)?;
        if let Some(entry) = current.and_then(|old| self.rooms.get_mut(&old)) {
            entry.members.remove(&user);
        }
        if let Some(entry) = target.and_then(|new| self.rooms.get_mut(&new)) {
            entry.members.insert(user);
        }
        self.set_membership(user, target, tracks);

        if let Some(old) = current {
            self.broadcast_room_state(old, None);
        }
        if let Some(new) = target {
            self.broadcast_room_state(new, Some(user));
        }
        match self.sessions.get(&user).and_then(presence) {
            Some(p) => self.emit_all(Event::PresenceChanged(p)),
            None => self.emit_all(Event::PresenceEnded(user)),
        }
        self.rebuild_routes();
        Ok(self.placed(user))
    }

    /// Takes `user` out of their room even when the room's epoch can't be bumped.
    fn leave_room(&mut self, user: UserId) {
        if !self.sessions.contains_key(&user) {
            return;
        }
        let Err(Refusal(_, message)) = self.place(user, None) else {
            return;
        };
        warn!(user = user.0, "leaving the room cleanly failed: {message}");
        let Some(room) = self.sessions.get(&user).and_then(|s| s.room) else {
            return;
        };
        if let Some(entry) = self.rooms.get_mut(&room) {
            entry.members.remove(&user);
        }
        self.set_membership(user, None, None);
        self.broadcast_room_state(room, None);
        self.emit_all(Event::PresenceEnded(user));
        self.rebuild_routes();
    }

    /// Puts `user`'s session in `room` with `tracks`, ending what it sent and watched.
    fn set_membership(&mut self, user: UserId, room: Option<RoomId>, tracks: Option<Tracks>) {
        let Some(session) = self.sessions.get_mut(&user) else {
            return;
        };
        session.room = room;
        session.media.share = None;
        session.watching.clear();
        let old_tracks = std::mem::replace(&mut session.tracks, tracks);
        for share in old_tracks.iter().flat_map(|t| &t.shares) {
            self.keyframes.forget(share.video);
        }
    }

    fn placed(&self, user: UserId) -> Placed {
        let Some(session) = self.sessions.get(&user) else {
            return lobby();
        };
        let Some(room) = session.room else {
            return lobby();
        };
        Placed {
            room: Some(room),
            epoch: self.rooms.get(&room).map_or(Epoch(0), |r| r.epoch),
            tracks: session.tracks.clone(),
            peers: self.peers(room),
        }
    }

    fn allocate_tracks(&mut self) -> Result<Tracks, Refusal> {
        let first = self
            .db
            .allocate_media_ids(TRACKS_PER_MEMBERSHIP)
            .map_err(Refusal::internal)?
            .ok_or_else(|| {
                warn!("media ids are exhausted; nobody can join a room any more");
                Refusal::new(ErrorCode::Internal, "this server has run out of media ids")
            })?
            .0;
        Ok(Tracks {
            mic: MediaId(first),
            shares: vec![Share {
                video: MediaId(first + 1),
                audio: MediaId(first + 2),
            }],
        })
    }

    fn set_profile(&mut self, user: UserId, name: String, avatar: Option<Hash>) -> Outcome {
        let name = checked_name(&name)?;
        self.require_picture(avatar)?;
        let session = self.session(user)?;
        if session
            .last_profile
            .is_some_and(|t| t.elapsed() < PROFILE_INTERVAL)
        {
            return Err(Refusal::new(
                ErrorCode::RateLimited,
                format!(
                    "one profile change per {} seconds",
                    PROFILE_INTERVAL.as_secs()
                ),
            ));
        }
        self.db
            .set_profile(user, &name, avatar)
            .map_err(Refusal::internal)?;
        let session = self.session_mut(user)?;
        session.row.name = name;
        session.row.avatar = avatar;
        session.last_profile = Some(Instant::now());
        self.announce_presence(user);
        Ok(Response::Ok)
    }

    fn set_server_name(&mut self, user: UserId, name: Option<String>) -> Outcome {
        require(permissions::can_edit_server(self.role(user)?))?;
        let name = match name.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(name) => Some(checked_name(name)?),
        };
        self.db
            .set_server_name(name.as_deref())
            .map_err(Refusal::internal)?;
        self.announce_server_profile()
    }

    fn set_server_avatar(&mut self, user: UserId, avatar: Option<Hash>) -> Outcome {
        require(permissions::can_edit_server(self.role(user)?))?;
        self.require_picture(avatar)?;
        self.db
            .set_server_avatar(avatar)
            .map_err(Refusal::internal)?;
        self.announce_server_profile()
    }

    fn announce_server_profile(&mut self) -> Outcome {
        let profile = self.db.server_profile().map_err(Refusal::internal)?;
        self.emit_all(Event::ServerProfileChanged(profile));
        Ok(Response::Ok)
    }

    fn set_avatar(&mut self, user: UserId, bytes: Vec<u8>) -> Outcome {
        if bytes.len() > MAX_AVATAR_BYTES {
            return Err(Refusal::new(
                ErrorCode::TooLarge,
                format!("avatars are at most {} KiB", MAX_AVATAR_BYTES / 1024),
            ));
        }
        let cost = bytes.len().max(MIN_UPLOAD_COST) as f64;
        if !self.uploads.take(user, cost, Instant::now()) {
            return Err(Refusal::new(
                ErrorCode::RateLimited,
                "too many pictures; try again in a minute",
            ));
        }
        let now = unix_now();
        delete_unused_blobs(&self.db, now).map_err(Refusal::internal)?;
        let hash = Hash::of(&bytes);
        self.db
            .put_blob(&hash, &bytes, now)
            .map_err(Refusal::internal)?;
        Ok(Response::AvatarHash(hash))
    }

    fn get_avatar(&mut self, hash: Hash) -> Outcome {
        match self.db.blob(&hash).map_err(Refusal::internal)? {
            Some(bytes) => Ok(Response::Avatar(bytes)),
            None => Err(Refusal::new(ErrorCode::NotFound, "no such avatar")),
        }
    }

    fn set_enc_key(&mut self, user: UserId, enc_key: SignedEncKey) -> Outcome {
        let session = self.session(user)?;
        let now = unix_now();
        if enc_key.not_after <= now || enc_key.not_after > now + MAX_ENC_KEY_LIFETIME.as_secs() {
            return Err(Refusal::new(
                ErrorCode::Invalid,
                "the encryption key's expiry is out of range",
            ));
        }
        let message =
            identity::enc_key_message(&self.fingerprint.0, user, enc_key.not_after, &enc_key.key);
        identity::verify(&session.row.pubkey, &message, &enc_key.sig).map_err(|_| {
            Refusal::new(
                ErrorCode::Invalid,
                "the encryption key's signature is wrong",
            )
        })?;
        self.db
            .set_enc_key(user, &enc_key)
            .map_err(Refusal::internal)?;
        let session = self.session_mut(user)?;
        session.row.enc_key = Some(enc_key);
        if let Some(room) = session.room {
            self.bump_epochs(&[room])?;
            self.broadcast_room_state(room, None);
        }
        Ok(Response::Ok)
    }

    fn set_media(&mut self, user: UserId, media: MediaState) -> Outcome {
        let room = self.current_room(user)?;
        let session = self.session_mut(user)?;
        let share_changed = session.media.share != media.share;
        let deafened_changed = session.media.deafened != media.deafened;
        session.media = media;
        if share_changed {
            self.broadcast_room_state(room, None);
        }
        if deafened_changed {
            self.rebuild_routes();
        }
        self.announce_presence(user);
        Ok(Response::Ok)
    }

    fn create_room(
        &mut self,
        user: UserId,
        name: String,
        password: Option<String>,
    ) -> Result<Handled, Refusal> {
        let name = self.room_to_create(user, &name)?;
        match password {
            Some(password) => {
                check_password_length(&password)?;
                Ok(Handled::Pending(PasswordWork::CreateRoom {
                    name,
                    password,
                }))
            }
            None => self.add_room(name, None).map(Handled::Done),
        }
    }

    /// Checks that `user` may create a room called `name`, and returns the name to use.
    fn room_to_create(&self, user: UserId, name: &str) -> Result<String, Refusal> {
        require(permissions::can_manage_rooms(self.role(user)?))?;
        let name = checked_name(name)?;
        if self.rooms.len() >= MAX_ROOMS {
            return Err(Refusal::new(
                ErrorCode::TooLarge,
                format!("a server holds at most {MAX_ROOMS} rooms"),
            ));
        }
        Ok(name)
    }

    fn add_room(&mut self, name: String, password_hash: Option<String>) -> Outcome {
        let id = self
            .db
            .create_room(&name, password_hash.as_deref())
            .map_err(Refusal::internal)?;
        let locked = password_hash.is_some();
        self.rooms.insert(
            id,
            RoomEntry {
                name: name.clone(),
                epoch: Epoch(0),
                password_hash,
                members: BTreeSet::new(),
            },
        );
        self.emit_all(Event::RoomCreated(Room { id, name, locked }));
        Ok(Response::Ok)
    }

    fn rename_room(&mut self, user: UserId, room: RoomId, name: String) -> Outcome {
        require(permissions::can_manage_rooms(self.role(user)?))?;
        let name = checked_name(&name)?;
        let entry = self.rooms.get_mut(&room).ok_or_else(no_such_room)?;
        self.db
            .rename_room(room, &name)
            .map_err(Refusal::internal)?;
        entry.name = name.clone();
        self.emit_all(Event::RoomRenamed { room, name });
        Ok(Response::Ok)
    }

    fn set_room_password(
        &mut self,
        user: UserId,
        room: RoomId,
        password: Option<String>,
    ) -> Result<Handled, Refusal> {
        self.may_manage_room(user, room)?;
        match password {
            Some(password) => {
                check_password_length(&password)?;
                Ok(Handled::Pending(PasswordWork::SetRoomPassword {
                    room,
                    password,
                }))
            }
            None => self.store_room_password(room, None).map(Handled::Done),
        }
    }

    fn may_manage_room(&self, user: UserId, room: RoomId) -> Result<(), Refusal> {
        require(permissions::can_manage_rooms(self.role(user)?))?;
        self.room_entry(room).map(|_| ())
    }

    fn store_room_password(&mut self, room: RoomId, hash: Option<String>) -> Outcome {
        let entry = self.rooms.get_mut(&room).ok_or_else(no_such_room)?;
        self.db
            .set_room_password_hash(room, hash.as_deref())
            .map_err(Refusal::internal)?;
        let locked = hash.is_some();
        entry.password_hash = hash;
        let members = &entry.members;
        self.last_joined
            .retain(|user, joined| *joined != room || members.contains(user));
        self.emit_all(Event::RoomLocked { room, locked });
        Ok(Response::Ok)
    }

    fn delete_room(&mut self, user: UserId, room: RoomId) -> Outcome {
        require(permissions::can_manage_rooms(self.role(user)?))?;
        for member in self.room_entry(room)?.members.clone() {
            self.place(member, None)?;
        }
        self.db.delete_room(room).map_err(Refusal::internal)?;
        self.rooms.remove(&room);
        self.last_joined.retain(|_, joined| *joined != room);
        self.emit_all(Event::RoomDeleted(room));
        Ok(Response::Ok)
    }

    fn create_invite(&mut self, user: UserId) -> Outcome {
        let now = unix_now();
        if let Some(cap) = permissions::invite_cap(self.role(user)?) {
            let day = INVITE_LIFETIME.as_secs();
            let used = self
                .db
                .invites_used(user, now, now.saturating_sub(day))
                .map_err(Refusal::internal)?;
            if used >= cap {
                return Err(Refusal::new(
                    ErrorCode::RateLimited,
                    format!("{cap} invites per day"),
                ));
            }
        }
        let (code, expires) = self
            .mint_invite(Some(user), now)
            .map_err(Refusal::internal)?;
        Ok(Response::Invite {
            code: code.to_string(),
            expires,
        })
    }

    /// Mints and stores a fresh invite. `minted_by` is `None` only for the bootstrap invite.
    pub fn mint_invite(
        &mut self,
        minted_by: Option<UserId>,
        now: u64,
    ) -> anyhow::Result<(InviteCode, u64)> {
        self.db.purge_expired_invites(now)?;
        let expires_at = now + INVITE_LIFETIME.as_secs();
        for _ in 0..MINT_ATTEMPTS {
            let code = InviteCode::from_random(random());
            let row = InviteRow {
                code_id: code.id.clone(),
                secret: code.secret.clone(),
                minted_by,
                expires_at,
                failures: 0,
            };
            if self.db.insert_invite(&row)? {
                return Ok((code, expires_at));
            }
        }
        anyhow::bail!("no free invite id; too many live invites")
    }

    fn list_users(&mut self, user: UserId) -> Outcome {
        require(permissions::can_list_users(self.role(user)?))?;
        let users = self.db.users().map_err(Refusal::internal)?;
        Ok(Response::Users(
            users.iter().map(UserRow::to_user).collect(),
        ))
    }

    fn set_role(&mut self, user: UserId, target: UserId, role: Role) -> Outcome {
        let actor = self.role(user)?;
        let row = self.stored_user(target)?;
        require(permissions::can_set_role(actor, row.role, role))?;
        self.db.set_role(target, role).map_err(Refusal::internal)?;
        if let Some(session) = self.sessions.get_mut(&target) {
            session.row.role = role;
        }
        self.announce_presence(target);
        let event = Event::RoleChanged { user: target, role };
        self.emit_user_list_change(event.clone());
        if !permissions::can_list_users(role) {
            self.emit_to(target, event);
        }
        Ok(Response::Ok)
    }

    fn kick_from_room(&mut self, user: UserId, target: UserId, room: RoomId) -> Outcome {
        let actor = self.role(user)?;
        let session = self
            .sessions
            .get(&target)
            .filter(|s| s.room == Some(room))
            .ok_or_else(|| Refusal::new(ErrorCode::NotFound, "they aren't in that room"))?;
        require(permissions::can_kick_from_room(actor, session.row.role))?;
        self.last_joined.remove(&target);
        self.leave_room(target);
        self.emit_to(target, Event::KickedFromRoom { by: user, room });
        info!(user = user.0, target = target.0, "kicked from room");
        Ok(Response::Ok)
    }

    fn remove_user(&mut self, user: UserId, target: UserId, cascade: bool) -> Outcome {
        let actor = self.role(user)?;
        let leaving = target == user && !cascade;
        if leaving && actor == Role::Admin && self.db.admin_count().map_err(Refusal::internal)? == 1
        {
            return Err(Refusal::new(
                ErrorCode::Forbidden,
                "you are the server's only admin, so you can't leave it",
            ));
        }
        let removed = if cascade {
            self.db.invite_tree(target).map_err(Refusal::internal)?
        } else {
            vec![target]
        };
        for id in &removed {
            let row = self.stored_user(*id)?;
            require(leaving || permissions::can_remove(actor, row.role))?;
        }
        self.db.remove_users(&removed).map_err(Refusal::internal)?;
        for id in &removed {
            self.evict(*id, "you were removed from this server");
            self.emit_user_list_change(Event::UserRemoved(*id));
        }
        info!(?removed, "removed users");
        Ok(Response::Ok)
    }

    fn set_watching(&mut self, user: UserId, ids: Vec<MediaId>) -> Outcome {
        if ids.len() > MAX_WATCHING {
            return Err(Refusal::new(
                ErrorCode::TooLarge,
                "watching too many tracks",
            ));
        }
        let room = self.current_room(user)?;
        for id in &ids {
            match self.share_track(*id) {
                Some((owner, _))
                    if owner != user
                        && self.sessions.get(&owner).and_then(|s| s.room) == Some(room) => {}
                _ => return Err(Refusal::new(ErrorCode::Invalid, "not a share in your room")),
            }
        }
        let session = self.session_mut(user)?;
        let started: Vec<MediaId> = ids
            .iter()
            .filter(|id| !session.watching.contains(id))
            .copied()
            .collect();
        session.watching = ids;
        self.rebuild_routes();
        let now = Instant::now();
        for id in started {
            if let Some((owner, TrackKind::Video)) = self.share_track(id)
                && let Some(kind) = self.keyframes.request(id, Recovery::Idr, now)
            {
                self.emit_to(owner, Event::KeyframeWanted { media_id: id, kind });
            }
        }
        Ok(Response::Ok)
    }

    fn request_keyframe(&mut self, user: UserId, media_id: MediaId, kind: Recovery) -> Outcome {
        if !self.session(user)?.watching.contains(&media_id) {
            return Err(Refusal::new(
                ErrorCode::Invalid,
                "you aren't watching that track",
            ));
        }
        let Some((owner, TrackKind::Video)) = self.share_track(media_id) else {
            return Err(Refusal::new(ErrorCode::Invalid, "not a video track"));
        };
        if let Some(kind) = self.keyframes.request(media_id, kind, Instant::now()) {
            self.emit_to(owner, Event::KeyframeWanted { media_id, kind });
        }
        Ok(Response::Ok)
    }

    /// Forwards keyframe requests that waited out their interval.
    pub fn flush_keyframes(&mut self) {
        for (media_id, kind) in self.keyframes.due(Instant::now()) {
            match self.share_track(media_id) {
                Some((owner, _)) => self.emit_to(owner, Event::KeyframeWanted { media_id, kind }),
                None => self.keyframes.forget(media_id),
            }
        }
    }

    fn publish_key(
        &mut self,
        user: UserId,
        epoch: Epoch,
        wrapped: Vec<(UserId, Vec<u8>)>,
    ) -> Outcome {
        let entry = self.room_entry(self.current_room(user)?)?;
        if epoch != entry.epoch {
            return Err(Refusal::new(
                ErrorCode::StaleEpoch,
                "the room has moved to a newer epoch",
            ));
        }
        let mut recipients = BTreeSet::new();
        for (to, blob) in &wrapped {
            if *to == user || !entry.members.contains(to) || !recipients.insert(*to) {
                return Err(Refusal::new(
                    ErrorCode::Invalid,
                    "every recipient must be another member of the room, once",
                ));
            }
            if blob.len() > MAX_WRAPPED_BYTES {
                return Err(Refusal::new(ErrorCode::TooLarge, "wrapped key too large"));
            }
        }
        for (to, wrapped) in wrapped {
            self.emit_to(
                to,
                Event::SenderKey {
                    from: user,
                    epoch,
                    wrapped,
                },
            );
        }
        Ok(Response::Ok)
    }

    /// The owner and kind of a share track.
    fn share_track(&self, media_id: MediaId) -> Option<(UserId, TrackKind)> {
        self.sessions.values().find_map(|s| {
            let share = s
                .tracks
                .as_ref()?
                .shares
                .iter()
                .find(|sh| sh.video == media_id || sh.audio == media_id)?;
            let kind = if share.video == media_id {
                TrackKind::Video
            } else {
                TrackKind::Audio
            };
            Some((s.row.id, kind))
        })
    }

    /// Recomputes who receives what and publishes it to the ingress loops.
    fn rebuild_routes(&self) {
        let mut table = ForwardingTable::default();
        for entry in self.rooms.values() {
            let members: Vec<&Session> = entry
                .members
                .iter()
                .filter_map(|m| self.sessions.get(m))
                .collect();
            for sender in &members {
                let Some(tracks) = &sender.tracks else {
                    continue;
                };
                let owner = sender.conn_id;
                let others = || members.iter().filter(|m| m.row.id != sender.row.id);
                let hearing = others().filter(|m| !m.media.deafened);
                table
                    .routes
                    .insert(tracks.mic, route(owner, TrackKind::Audio, hearing));
                for share in &tracks.shares {
                    let watchers = |id: MediaId| others().filter(move |m| m.watching.contains(&id));
                    let video = watchers(share.video);
                    let audio = watchers(share.audio).filter(|m| !m.media.deafened);
                    table
                        .routes
                        .insert(share.video, route(owner, TrackKind::Video, video));
                    table
                        .routes
                        .insert(share.audio, route(owner, TrackKind::Audio, audio));
                }
            }
        }
        self.sfu.table.store(Arc::new(table));
    }
}

fn require(allowed: bool) -> Result<(), Refusal> {
    if allowed {
        Ok(())
    } else {
        Err(Refusal::new(ErrorCode::Forbidden, "you can't do that"))
    }
}

fn no_such_room() -> Refusal {
    Refusal::new(ErrorCode::NotFound, "no such room")
}

fn delete_unused_blobs(db: &Database, now: u64) -> anyhow::Result<usize> {
    db.delete_unused_blobs(now.saturating_sub(UNUSED_BLOB_GRACE.as_secs()))
}

fn send(session: &Session, event: Event) {
    let message = Outgoing::Message(ServerMessage::Event(event));
    if session.link.outgoing.try_send(message).is_err() {
        warn!(user = session.row.id.0, "control queue full; closing");
        session
            .link
            .connection
            .close(close::PROTOCOL_ERROR.into(), b"control queue full");
    }
}

fn route<'a>(
    owner: u64,
    kind: TrackKind,
    subscribers: impl Iterator<Item = &'a &'a Session>,
) -> Route {
    Route {
        owner,
        kind,
        subscribers: subscribers.map(|m| m.link.egress.clone()).collect(),
    }
}

fn presence(session: &Session) -> Option<Presence> {
    Some(Presence {
        user: session.row.id,
        name: session.row.name.clone(),
        avatar: session.row.avatar,
        room: Some(session.room?),
        mic: session.media.mic,
        deafened: session.media.deafened,
        sharing: session.media.share.is_some(),
        role: session.row.role,
    })
}

fn peer(session: &Session) -> Option<RoomPeer> {
    Some(RoomPeer {
        user: session.row.id,
        tracks: session.tracks.clone()?,
        share: session.media.share,
        identity_key: session.row.pubkey,
        enc_key: session.row.enc_key?,
    })
}

fn lobby() -> Placed {
    Placed {
        room: None,
        epoch: Epoch(0),
        tracks: None,
        peers: Vec::new(),
    }
}

fn checked_name(name: &str) -> Result<String, Refusal> {
    valid_name(name).map(str::to_owned).ok_or_else(|| {
        Refusal::new(
            ErrorCode::Invalid,
            format!("names are 1 to {MAX_NAME_CHARS} characters"),
        )
    })
}

fn check_password_length(password: &str) -> Result<(), Refusal> {
    if valid_password(password) {
        Ok(())
    } else {
        Err(Refusal::new(
            ErrorCode::Invalid,
            format!("passwords are 1 to {MAX_PASSWORD_CHARS} characters"),
        ))
    }
}
