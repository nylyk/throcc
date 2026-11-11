use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use throcc_crypto::safety;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Codec, PublicKey, ShareConfig, User};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::warn;

use crate::account::{self, SignInError};
use crate::avatars;
use crate::call::Call;
use crate::keystore::Keystore;
use crate::media::video::frame::VideoFrame;
use crate::media::video::{Frame, Picture};
use crate::media::voice::AudioEngine;
use crate::profile::Profile;
use crate::session::{AvatarSlot, Session, Shared};
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, MIC_KBPS, Person, Playback, RoomView,
    SHARE_AUDIO_DETAIL, SHARE_AUDIO_KBPS, SentTrack, ServerView, Sound, SoundLevel, State,
    TrackHealth, VOICE_DETAIL, share_detail,
};
use crate::verified::VerifiedStore;

/// What the UI asks the core to do. The core's own type, never `throcc-proto`'s.
#[derive(Clone, Debug, PartialEq)]
pub enum UiCommand {
    Join(Option<RoomId>),
    JoinWithPassword(RoomId, String),
    SetMic(bool),
    SetDeafened(bool),
    StartShare(ShareConfig),
    StopShare,
    Watch(UserId),
    Unwatch(UserId),
    SetSoundLevel(UserId, Sound, SoundLevel),
    /// Gives the server our changed profile, its picture at most 64 KiB.
    SetProfile(Profile),
    /// Names the server and sets its picture (at most 64 KiB, `Some(None)` clears it) in one
    /// save. A `None` field keeps what it has. Admins only.
    SetServerProfile {
        name: Option<Option<String>>,
        picture: Option<Option<Vec<u8>>>,
    },
    FetchAvatar(Hash),
    /// Creates a room with this name, locked behind the password when there is one.
    CreateRoom(String, Option<String>),
    RenameRoom(RoomId, String),
    /// Locks the room behind a password, or unlocks it with `None`.
    SetRoomPassword(RoomId, Option<String>),
    DeleteRoom(RoomId),
    CreateInvite,
    ListUsers,
    SetRole(UserId, Role),
    /// Moves someone from `room` to the lobby.
    KickFromRoom {
        user: UserId,
        room: RoomId,
    },
    RemoveUser {
        user: UserId,
        cascade: bool,
    },
    /// Verifies `key`, the identity key whose safety number the user compared with `user`,
    /// on every server, or takes it off the verified ones. Refused with a notice once `user`
    /// presents another key.
    SetVerified {
        user: UserId,
        key: Fingerprint,
        verified: bool,
    },
    /// Stops warning that this user's verified key changed, leaving the new key unverified.
    /// `shown` is the key the user looked at; refused with a notice once `user` presents
    /// another.
    DismissKeyChange {
        user: UserId,
        shown: Option<Fingerprint>,
    },
    /// Shows who is verified again, after a change to the [`VerifiedStore`] made outside
    /// the client.
    VerifiedChanged,
    /// Ends our membership of this server and forgets it, then disconnects.
    LeaveServer,
    /// Signs in again once the session waits in [`ConnectionStatus::AwaitingReconnect`].
    Reconnect,
    Disconnect,
}

/// What the core tells the UI.
#[derive(Clone, Debug)]
pub enum UiEvent {
    State(Arc<State>),
    Invite {
        code: String,
        expires: u64,
    },
    Users(Vec<User>),
    Notice(String),
    RemovedFromServer(String),
    /// A profile picture asked for with [`ClientApi::avatar`] has arrived.
    AvatarReady(Hash),
    /// The server has our profile as kept on this computer.
    ProfileSaved,
    /// The server refused our profile, for this reason.
    ProfileRefused(String),
    /// The server took the server's new name or picture.
    ServerProfileSaved,
    /// The server refused the server's new name or picture, for this reason.
    ServerProfileRefused(String),
    /// The server refused the password we joined a locked room with, for this reason.
    PasswordRefused(String),
    /// We left the server for good, and it is forgotten.
    LeftServer,
}

/// The UI's view of a client. Implemented by the real client and by [`MockClient`].
pub trait ClientApi: Send + Sync {
    fn command(&self, command: UiCommand);
    fn events(&self) -> broadcast::Receiver<UiEvent>;
    fn state(&self) -> Arc<State>;
    /// The 60-digit safety number with whoever holds the identity key `key`.
    fn safety_number(&self, key: &PublicKey) -> String;
    /// The newest picture of `user`'s share while watched, or of ours while sharing.
    fn picture(&self, user: UserId) -> Option<Picture>;
    /// Tells the core `user`'s picture is on screen. Our own share builds its preview only
    /// while this keeps being called.
    fn picture_drawn(&self, _user: UserId) {}
    /// The profile picture behind `hash`, fetched in the background on first ask.
    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>>;
}

const COMMANDS: usize = 64;
/// How long dropping a client waits for its session to say goodbye to the server.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// A live client: tokio runs on its own threads, and a session task keeps [`State`]
/// current, drives media and reconnects with the room it was in.
pub struct Client {
    commands: mpsc::Sender<UiCommand>,
    shared: Arc<Shared>,
    identity: PublicKey,
    runtime: Option<tokio::runtime::Runtime>,
    session: Option<tokio::task::JoinHandle<()>>,
}

impl Client {
    /// Signs in, or enrolls with `invite`, and blocks until the server has answered. The
    /// server gets the newest `profile` unless it has it already.
    pub fn connect(
        address: &str,
        mut keystore: Keystore,
        invite: Option<&InviteCode>,
        profile: watch::Receiver<Profile>,
        audio: Option<Arc<AudioEngine>>,
        verified: VerifiedStore,
    ) -> Result<Self, SignInError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("throcc-core")
            .enable_all()
            .build()
            .map_err(|e| SignInError::Store(e.into()))?;
        keystore.renew_encryption_key_if_due()?;
        let name = profile.borrow().name.clone();
        let link = runtime.block_on(async {
            match invite {
                Some(code) => account::enroll(&mut keystore, address, code, &name).await,
                None => account::sign_in(&keystore, address, None).await,
            }
        })?;
        let identity = keystore.identity().public();
        let cache = keystore.cache_dir().to_owned();
        runtime.spawn_blocking(move || {
            if let Err(e) = avatars::prune(&cache, SystemTime::now()) {
                warn!("removing unused cached pictures: {e:#}");
            }
        });
        let shared = Arc::new(Shared::new(State::default()));
        let (commands, commands_rx) = mpsc::channel(COMMANDS);
        let session = {
            let _guard = runtime.enter();
            let call =
                Call::start_shown(link, &keystore, verified.clone()).map_err(SignInError::Store)?;
            Session::new(
                shared.clone(),
                keystore,
                address.to_owned(),
                profile,
                audio,
                call,
                verified,
            )
        };
        let session = runtime.spawn(session.run(commands_rx));
        Ok(Self {
            commands,
            shared,
            identity,
            runtime: Some(runtime),
            session: Some(session),
        })
    }
}

impl Drop for Client {
    /// Closes the connection before stopping the runtime: shutting down cancels every task
    /// at once, so the server would otherwise keep us present until the idle timeout.
    fn drop(&mut self) {
        // Closing the channel makes the session disconnect once it has run what was queued.
        drop(std::mem::replace(&mut self.commands, mpsc::channel(1).0));
        if let Some(runtime) = self.runtime.take() {
            if let Some(session) = self.session.take() {
                runtime.block_on(async { tokio::time::timeout(CLOSE_WAIT, session).await.ok() });
            }
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl ClientApi for Client {
    fn command(&self, command: UiCommand) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.commands.try_send(command) {
            self.shared.notice("busy; try again");
        }
    }

    fn events(&self) -> broadcast::Receiver<UiEvent> {
        self.shared.events.subscribe()
    }

    fn state(&self) -> Arc<State> {
        self.shared.state()
    }

    fn safety_number(&self, key: &PublicKey) -> String {
        safety::safety_number(&self.identity, key)
    }

    fn picture(&self, user: UserId) -> Option<Picture> {
        self.shared.picture(user)
    }

    fn picture_drawn(&self, user: UserId) {
        self.shared.picture_drawn(user);
    }

    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>> {
        let (bytes, fetch) = self.shared.avatar(hash);
        if fetch
            && self
                .commands
                .try_send(UiCommand::FetchAvatar(hash))
                .is_err()
        {
            self.shared.forget_avatar(hash);
        }
        bytes
    }
}

/// A client with canned people and rooms that reacts to commands locally.
pub struct MockClient {
    shared: Shared,
    verified: VerifiedStore,
    /// The picture every share shows.
    picture: watch::Sender<Option<Arc<Frame>>>,
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new(VerifiedStore::in_memory())
    }
}

impl MockClient {
    /// Keeps `bytes` as a picture, as the server would, and returns its hash.
    fn keep_avatar(&self, bytes: Vec<u8>) -> Hash {
        let hash = Hash::of(&bytes);
        self.shared
            .set_avatar(hash, AvatarSlot::Ready(bytes.into()));
        hash
    }

    /// The sample client, with its sample verified people added to `verified`.
    pub fn new(verified: VerifiedStore) -> Self {
        let frame = VideoFrame::test_pattern(1280, 720, 1, 0);
        add_sample_verified(&verified);
        let client = Self {
            shared: Shared::new(sample_state()),
            verified,
            picture: watch::Sender::new(Some(Arc::new(Frame::Planes(frame)))),
        };
        for (_, picture) in SAMPLE_PICTURES {
            client.keep_avatar(picture.to_vec());
        }
        client.keep_avatar(SAMPLE_SERVER_PICTURE.to_vec());
        client
    }

    /// Makes every share show `frame`.
    pub fn show_in_shares(&self, frame: VideoFrame) {
        self.picture
            .send_replace(Some(Arc::new(Frame::Planes(frame))));
    }

    /// The sample client, as if connected at `address`.
    pub fn at(address: &str, verified: VerifiedStore) -> Self {
        let client = Self::new(verified);
        client.update(|s| {
            if let Some(server) = s.server.as_mut() {
                server.address = address.to_owned();
            }
        });
        client
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        self.shared.update(change);
    }

    fn me(&self) -> UserId {
        self.state().me.unwrap_or(UserId(0))
    }
}

impl ClientApi for MockClient {
    fn command(&self, command: UiCommand) {
        let me = self.me();
        match command {
            UiCommand::Join(room) => self.update(|s| {
                let (mic, deafened) = (s.mic, s.deafened);
                if let Some(person) = s.people.get_mut(&me) {
                    person.room = room;
                    person.mic = mic;
                    person.deafened = deafened;
                    person.sharing = None;
                }
                s.current = room.map(|room| CurrentRoom {
                    room,
                    sharing: None,
                    watching: vec![],
                });
            }),
            UiCommand::JoinWithPassword(room, _) => self.command(UiCommand::Join(Some(room))),
            UiCommand::SetMic(on) => self.update(|s| {
                s.mic = on;
                if let Some(p) = s.people.get_mut(&me) {
                    p.mic = on;
                }
            }),
            UiCommand::SetDeafened(on) => self.update(|s| {
                s.deafened = on;
                if let Some(p) = s.people.get_mut(&me) {
                    p.deafened = on;
                }
            }),
            UiCommand::StartShare(config) => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.sharing = Some(config);
                    p.sharing = Some(config);
                })
            }),
            UiCommand::StopShare => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.sharing = None;
                    p.sharing = None;
                })
            }),
            UiCommand::Watch(user) => self.update(|s| {
                if let Some(c) = s.current.as_mut()
                    && !c.watching.contains(&user)
                {
                    c.watching.push(user);
                    if let Some(p) = s.people.get_mut(&user) {
                        p.audience += 1;
                    }
                }
            }),
            UiCommand::Unwatch(user) => self.update(|s| {
                if let Some(c) = s.current.as_mut()
                    && c.watching.contains(&user)
                {
                    c.watching.retain(|u| *u != user);
                    if let Some(p) = s.people.get_mut(&user) {
                        p.audience = p.audience.saturating_sub(1);
                    }
                }
            }),
            UiCommand::SetSoundLevel(user, sound, level) => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user) {
                    *p.playback.get_mut(sound) = level;
                }
            }),
            UiCommand::SetProfile(profile) => {
                let hash = profile
                    .picture
                    .map(|bytes| self.keep_avatar(bytes.to_vec()));
                self.update(|s| {
                    if let Some(p) = s.people.get_mut(&me) {
                        p.name = profile.name;
                        p.avatar = hash;
                    }
                });
                self.shared.emit(UiEvent::ProfileSaved);
            }
            UiCommand::SetServerProfile { name, picture } => {
                let hash = picture.map(|p| p.map(|bytes| self.keep_avatar(bytes)));
                self.update(|s| {
                    if let Some(server) = s.server.as_mut() {
                        if let Some(name) = name {
                            server.name = name;
                        }
                        if let Some(hash) = hash {
                            server.avatar = hash;
                        }
                    }
                });
                self.shared.emit(UiEvent::ServerProfileSaved);
            }
            UiCommand::FetchAvatar(_) => {}
            UiCommand::CreateRoom(name, password) => self.update(|s| {
                let id = RoomId(s.rooms.iter().map(|r| r.id.0).max().unwrap_or(0) + 1);
                s.rooms.push(RoomView {
                    id,
                    name,
                    locked: password.is_some(),
                });
            }),
            UiCommand::RenameRoom(room, name) => self.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.name = name;
                }
            }),
            UiCommand::SetRoomPassword(room, password) => self.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.locked = password.is_some();
                }
            }),
            UiCommand::KickFromRoom { user, room } => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user).filter(|p| p.room == Some(room)) {
                    p.room = None;
                    p.sharing = None;
                }
            }),
            UiCommand::DeleteRoom(room) => self.update(|s| {
                s.rooms.retain(|r| r.id != room);
                for p in s.people.values_mut().filter(|p| p.room == Some(room)) {
                    p.room = None;
                }
                if s.current.as_ref().is_some_and(|c| c.room == room) {
                    s.current = None;
                }
            }),
            UiCommand::CreateInvite => self.shared.emit(UiEvent::Invite {
                code: "K7Q-M2XPD".into(),
                expires: 0,
            }),
            UiCommand::ListUsers | UiCommand::SetRole(..) | UiCommand::RemoveUser { .. } => {
                self.shared.emit(UiEvent::Users(sample_users()));
            }
            UiCommand::SetVerified {
                user,
                key,
                verified,
            } => self.update(|s| {
                let Some(p) = s.people.get_mut(&user) else {
                    return;
                };
                if p.identity_key.map(Fingerprint) != Some(key) {
                    return;
                }
                p.verified = verified;
                let name = p.name.clone();
                let mut keys = self.verified.lock();
                if verified {
                    if let Some(server) = s.server.as_ref().map(|s| s.fingerprint) {
                        let _ = keys.verify(&server, user, &key.0, &name);
                    }
                    s.key_warnings.retain(|w| w.user != user);
                } else {
                    let _ = keys.unverify(&key.0);
                    s.key_warnings.retain(|w| w.verified != key);
                }
            }),
            UiCommand::DismissKeyChange { user, shown } => self.update(|s| {
                if s.person(user).and_then(|p| p.identity_key).map(Fingerprint) == shown {
                    s.key_warnings.retain(|w| w.user != user);
                }
            }),
            UiCommand::VerifiedChanged => {
                let state = self.state();
                let keys = self.verified.lock();
                let verified: HashSet<PublicKey> = state
                    .key_warnings
                    .iter()
                    .map(|w| w.verified.0)
                    .chain(state.people.values().filter_map(|p| p.identity_key))
                    .filter(|key| keys.is_verified(key))
                    .collect();
                drop(keys);
                self.update(|s| {
                    s.key_warnings.retain(|w| verified.contains(&w.verified.0));
                    for p in s.people.values_mut() {
                        p.verified = p.identity_key.is_some_and(|k| verified.contains(&k));
                    }
                });
            }
            UiCommand::LeaveServer => {
                self.update(|s| s.connection = ConnectionStatus::Disconnected);
                self.shared.emit(UiEvent::LeftServer);
            }
            UiCommand::Reconnect => self.update(|s| s.connection = ConnectionStatus::Connected),
            UiCommand::Disconnect => self.update(|s| s.connection = ConnectionStatus::Disconnected),
        }
    }

    fn events(&self) -> broadcast::Receiver<UiEvent> {
        self.shared.events.subscribe()
    }

    fn state(&self) -> Arc<State> {
        self.shared.state()
    }

    fn safety_number(&self, _key: &PublicKey) -> String {
        "482139057716024733195580241196087436295031487266089415270031".into()
    }

    fn picture(&self, _user: UserId) -> Option<Picture> {
        Some(self.picture.subscribe())
    }

    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>> {
        self.shared.avatar(hash).0
    }
}

fn set_mine(state: &mut State, me: UserId, change: impl FnOnce(&mut CurrentRoom, &mut Person)) {
    if let (Some(current), Some(person)) = (state.current.as_mut(), state.people.get_mut(&me)) {
        change(current, person);
    }
}

fn person(name: &str, room: Option<u64>) -> Person {
    Person {
        name: name.into(),
        avatar: None,
        room: room.map(RoomId),
        mic: room.is_some(),
        deafened: false,
        sharing: None,
        audience: 0,
        speaking: false,
        playback: Playback::default(),
        verified: false,
        identity_key: None,
        role: Role::User,
    }
}

const SAMPLE_PICTURES: [(u64, &[u8]); 7] = [
    (1, include_bytes!("../assets/mock/nori.jpg")),
    (2, include_bytes!("../assets/mock/kiwi.jpg")),
    (3, include_bytes!("../assets/mock/mochi.jpg")),
    (4, include_bytes!("../assets/mock/pixelmoth.png")),
    (5, include_bytes!("../assets/mock/sleepyfox.jpg")),
    (6, include_bytes!("../assets/mock/juno.jpg")),
    (8, include_bytes!("../assets/mock/rook.jpg")),
];
pub const SAMPLE_SERVER_PICTURE: &[u8] = include_bytes!("../assets/mock/night-owls.jpg");

/// The profile picture of the sample person `user`, if they have one.
pub fn sample_picture(user: UserId) -> Option<&'static [u8]> {
    SAMPLE_PICTURES
        .iter()
        .find(|(id, _)| UserId(*id) == user)
        .map(|(_, picture)| *picture)
}

fn sample_state() -> State {
    let mut people = std::collections::BTreeMap::new();
    let pixelmoth_share = ShareConfig {
        audio: true,
        kbps: 10_000,
        codec: Codec::Av1,
        width: 2560,
        height: 1440,
        fps: 60,
    };
    for (id, name, room) in [
        (1, "nori", None),
        (2, "Kiwi", Some(1)),
        (3, "mochi", Some(1)),
        (4, "pixelmoth", Some(1)),
        (5, "sleepyfox", Some(2)),
        (6, "Juno", Some(2)),
        (7, "tobi_", Some(4)),
        (8, "Rook", Some(4)),
    ] {
        let mut person = person(name, room);
        person.avatar = sample_picture(UserId(id)).map(Hash::of);
        person.identity_key = Some(sample_key(id));
        person.verified = [2, 5, 6].contains(&id);
        people.insert(UserId(id), person);
    }
    if let Some(p) = people.get_mut(&UserId(1)) {
        p.role = Role::Admin;
    }
    if let Some(p) = people.get_mut(&UserId(2)) {
        p.speaking = true;
        p.role = Role::Manager;
    }
    if let Some(p) = people.get_mut(&UserId(3)) {
        p.mic = false;
    }
    if let Some(p) = people.get_mut(&UserId(4)) {
        p.sharing = Some(pixelmoth_share);
        p.audience = 2;
    }
    if let Some(p) = people.get_mut(&UserId(7)) {
        p.deafened = true;
    }
    State {
        server: Some(ServerView {
            address: "owls.thro.cc".into(),
            fingerprint: Fingerprint([0x2c; 32]),
            name: Some("Night Owls".into()),
            avatar: Some(Hash::of(SAMPLE_SERVER_PICTURE)),
        }),
        connection: ConnectionStatus::Connected,
        me: Some(UserId(1)),
        role: Some(Role::Admin),
        rooms: ["Minecraft", "Lounge", "Study", "AFK"]
            .iter()
            .enumerate()
            .map(|(i, name)| RoomView {
                id: RoomId(i as u64 + 1),
                name: (*name).into(),
                locked: *name == "Study",
            })
            .collect(),
        people,
        current: None,
        mic: true,
        deafened: false,
        key_warnings: Vec::new(),
        health: Health {
            rtt_ms: 18.0,
            download_kbps: 10_480,
            upload_kbps: 92,
            upload_loss: 0.0,
            download_loss: 0.3,
            dropped_for_you: 0,
            slow_shares: Default::default(),
            receiving: vec![
                TrackHealth {
                    label: "pixelmoth, screen".into(),
                    detail: share_detail(&pixelmoth_share, "VideoToolbox"),
                    set_kbps: Some(pixelmoth_share.kbps),
                    arriving_kbps: 6_100,
                    loss: Some(4.2),
                },
                TrackHealth {
                    label: "pixelmoth, screen audio".into(),
                    detail: SHARE_AUDIO_DETAIL.into(),
                    set_kbps: Some(SHARE_AUDIO_KBPS),
                    arriving_kbps: 128,
                    loss: Some(0.0),
                },
                TrackHealth {
                    label: "Kiwi, mic".into(),
                    detail: VOICE_DETAIL.into(),
                    set_kbps: Some(MIC_KBPS),
                    arriving_kbps: 61,
                    loss: Some(0.3),
                },
                TrackHealth {
                    label: "mochi, mic".into(),
                    detail: VOICE_DETAIL.into(),
                    set_kbps: Some(MIC_KBPS),
                    arriving_kbps: 0,
                    loss: None,
                },
            ],
            sending: vec![SentTrack {
                label: "mic",
                detail: VOICE_DETAIL.into(),
                set_kbps: MIC_KBPS,
                leaving_kbps: 61,
                dropped: None,
            }],
        },
    }
}

fn add_sample_verified(verified: &VerifiedStore) {
    let (here, elsewhere) = (Fingerprint([0x2c; 32]), Fingerprint([0x91; 32]));
    let mut keys = verified.lock();
    for (server, user, key, name) in [
        (here, 2, sample_key(2), "Kiwi"),
        (elsewhere, 2, sample_key(2), "Kiwi"),
        (here, 3, [0x7c; 32], "mochi"),
        (here, 5, sample_key(5), "sleepyfox"),
        (here, 6, sample_key(6), "Juno"),
    ] {
        let _ = keys.verify(&server, UserId(user), &key, name);
    }
}

/// A sample person's identity key, as in [`sample_users`].
fn sample_key(id: u64) -> PublicKey {
    [(id as u8).wrapping_mul(37); 32]
}

fn sample_users() -> Vec<User> {
    [
        (1, "nori", Role::Admin, None),
        (3, "mochi", Role::Manager, Some(1)),
        (2, "Kiwi", Role::User, Some(3)),
        (4, "pixelmoth", Role::User, Some(3)),
        (5, "sleepyfox", Role::User, Some(2)),
        (6, "Juno", Role::User, Some(5)),
    ]
    .into_iter()
    .map(|(id, name, role, by)| User {
        id: UserId(id),
        name: name.into(),
        role,
        invited_by: by.map(UserId),
        identity_key: sample_key(id),
        created_at: 0,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_reacts_to_commands() {
        let client = MockClient::default();
        let mut events = client.events();
        client.command(UiCommand::Join(Some(RoomId(1))));
        let Ok(UiEvent::State(state)) = events.try_recv() else {
            panic!("no state")
        };
        assert_eq!(state.current.as_ref().map(|c| c.room), Some(RoomId(1)));
        client.command(UiCommand::SetMic(false));
        assert!(!client.state().mic);
        client.command(UiCommand::Watch(UserId(4)));
        assert_eq!(
            client.state().current.as_ref().map(|c| c.watching.clone()),
            Some(vec![UserId(4)])
        );
    }
}
