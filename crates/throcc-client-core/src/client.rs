use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
use crate::lock;
use crate::media::audio::VoiceSettings;
use crate::media::cues::Cue;
use crate::media::video::Picture;
use crate::media::video::frame::VideoFrame;
use crate::session::{AvatarSlot, Session, Shared};
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, MIC_KBPS, Person, PersonPlayback, Playback,
    RoomView, SHARE_AUDIO_DETAIL, SHARE_AUDIO_KBPS, SentTrack, ServerView, Sound, State,
    TrackHealth, VOICE_DETAIL, VerifiedIdentity, share_detail,
};

#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    pub voice: VoiceSettings,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Opens audio devices. Off for headless tests.
    pub audio: bool,
}

/// What the UI asks the core to do. The core's own type, never `throcc-proto`'s.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Join(Option<RoomId>),
    JoinWithPassword(RoomId, String),
    SetMic(bool),
    SetDeafened(bool),
    StartShare(ShareConfig),
    StopShare,
    Watch(UserId),
    Unwatch(UserId),
    SetPlayback(UserId, Sound, Playback),
    /// Changes our name and picture (at most 64 KiB, `Some(None)` clears it) in one save.
    /// A `None` field keeps what we have.
    SetProfile {
        name: Option<String>,
        picture: Option<Option<Vec<u8>>>,
    },
    /// Names the server and sets its picture like [`Command::SetProfile`]. Admins only.
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
    /// Takes a verified identity key off the verified ones, on every server.
    Unverify(Fingerprint),
    Voice(VoiceSettings),
    Devices {
        input: Option<String>,
        output: Option<String>,
    },
    /// Plays your processed mic back to you.
    MicTest(bool),
    PushToTalk(bool),
    /// Plays a cue at the cue volume, to hear the setting.
    PlayCue(Cue),
    /// Ends our membership of this server and forgets it, then disconnects.
    LeaveServer,
    Disconnect,
}

/// What the core tells the UI.
#[derive(Clone, Debug)]
pub enum Event {
    State(Arc<State>),
    Invite {
        code: String,
        expires: u64,
    },
    Users(Vec<User>),
    Notice(String),
    Kicked(String),
    /// A profile picture asked for with [`ClientApi::avatar`] has arrived.
    AvatarReady(Hash),
    /// The server took our new name or picture.
    ProfileSaved,
    /// The server refused our new name or picture, for this reason.
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
    fn command(&self, command: Command);
    fn events(&self) -> broadcast::Receiver<Event>;
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
    /// The processed mic level in dBFS, refreshed every 100 ms while audio runs.
    fn mic_level(&self) -> Option<f32>;
}

const COMMANDS: usize = 64;
/// How long dropping a client waits for its session to say goodbye to the server.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// A live client: tokio runs on its own threads, and a session task keeps [`State`]
/// current, drives media and reconnects with the room it was in.
pub struct Client {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    identity: PublicKey,
    runtime: Option<tokio::runtime::Runtime>,
    session: Option<tokio::task::JoinHandle<()>>,
}

impl Client {
    /// Signs in, or enrolls with `invite`, and blocks until the server has answered.
    pub fn connect(
        address: &str,
        mut keystore: Keystore,
        invite: Option<&InviteCode>,
        options: ClientOptions,
    ) -> Result<Self, SignInError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("throcc-core")
            .enable_all()
            .build()
            .map_err(|e| SignInError::Store(e.into()))?;
        keystore.renew_enc_key_if_due()?;
        let link = runtime.block_on(async {
            match invite {
                Some(code) => account::enroll(&mut keystore, address, code).await,
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
            let call = Call::start(link, &keystore).map_err(SignInError::Store)?;
            Session::new(shared.clone(), keystore, address.to_owned(), options, call)
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
    fn command(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            self.shared.notice("busy; try again");
        }
    }

    fn events(&self) -> broadcast::Receiver<Event> {
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
        if fetch && self.commands.try_send(Command::FetchAvatar(hash)).is_err() {
            self.shared.forget_avatar(hash);
        }
        bytes
    }

    fn mic_level(&self) -> Option<f32> {
        *lock(&self.shared.mic_level)
    }
}

/// A client with canned people and rooms that reacts to commands locally.
pub struct MockClient {
    shared: Shared,
    /// A test pattern every share shows.
    picture: watch::Sender<Option<Arc<VideoFrame>>>,
    started: Instant,
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new()
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

    pub fn new() -> Self {
        let frame = VideoFrame::test_pattern(1280, 720, 1, 0);
        Self {
            shared: Shared::new(sample_state()),
            picture: watch::Sender::new(Some(Arc::new(frame))),
            started: Instant::now(),
        }
    }

    /// The sample client, as if connected at `address`.
    pub fn at(address: &str) -> Self {
        let client = Self::new();
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
    fn command(&self, command: Command) {
        let me = self.me();
        match command {
            Command::Join(room) => self.update(|s| {
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
            Command::JoinWithPassword(room, _) => self.command(Command::Join(Some(room))),
            Command::SetMic(on) => self.update(|s| {
                s.mic = on;
                if let Some(p) = s.people.get_mut(&me) {
                    p.mic = on;
                }
            }),
            Command::SetDeafened(on) => self.update(|s| {
                s.deafened = on;
                if let Some(p) = s.people.get_mut(&me) {
                    p.deafened = on;
                }
            }),
            Command::StartShare(config) => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.sharing = Some(config);
                    p.sharing = Some(config);
                })
            }),
            Command::StopShare => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.sharing = None;
                    p.sharing = None;
                })
            }),
            Command::Watch(user) => self.update(|s| {
                if let Some(c) = s.current.as_mut()
                    && !c.watching.contains(&user)
                {
                    c.watching.push(user);
                }
            }),
            Command::Unwatch(user) => self.update(|s| {
                if let Some(c) = s.current.as_mut() {
                    c.watching.retain(|u| *u != user);
                }
            }),
            Command::SetPlayback(user, sound, playback) => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user) {
                    *p.playback.get_mut(sound) = playback;
                }
            }),
            Command::SetProfile { name, picture } => {
                let hash = picture.map(|p| p.map(|bytes| self.keep_avatar(bytes)));
                self.update(|s| {
                    if let Some(name) = name {
                        if let Some(p) = s.people.get_mut(&me) {
                            p.name = name.clone();
                        }
                        s.profile_name = Some(name);
                    }
                    if let Some(hash) = hash {
                        if let Some(p) = s.people.get_mut(&me) {
                            p.avatar = hash;
                        }
                        s.profile_picture = hash;
                    }
                });
                self.shared.emit(Event::ProfileSaved);
            }
            Command::SetServerProfile { name, picture } => {
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
                self.shared.emit(Event::ServerProfileSaved);
            }
            Command::FetchAvatar(_) => {}
            Command::CreateRoom(name, password) => self.update(|s| {
                let id = RoomId(s.rooms.iter().map(|r| r.id.0).max().unwrap_or(0) + 1);
                s.rooms.push(RoomView {
                    id,
                    name,
                    locked: password.is_some(),
                });
            }),
            Command::RenameRoom(room, name) => self.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.name = name;
                }
            }),
            Command::SetRoomPassword(room, password) => self.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.locked = password.is_some();
                }
            }),
            Command::KickFromRoom { user, room } => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user).filter(|p| p.room == Some(room)) {
                    p.room = None;
                    p.sharing = None;
                }
            }),
            Command::DeleteRoom(room) => self.update(|s| {
                s.rooms.retain(|r| r.id != room);
                for p in s.people.values_mut().filter(|p| p.room == Some(room)) {
                    p.room = None;
                }
                if s.current.as_ref().is_some_and(|c| c.room == room) {
                    s.current = None;
                }
            }),
            Command::CreateInvite => self.shared.emit(Event::Invite {
                code: "K7Q-M2XPD".into(),
                expires: 0,
            }),
            Command::ListUsers | Command::SetRole(..) | Command::RemoveUser { .. } => {
                self.shared.emit(Event::Users(sample_users()));
            }
            Command::SetVerified {
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
                s.verified.retain(|v| v.key != key);
                if verified {
                    let server = s.server.as_ref().map(|s| s.fingerprint);
                    s.verified.push(VerifiedIdentity {
                        key,
                        name,
                        servers: server.into_iter().collect(),
                    });
                    s.key_warnings.retain(|w| w.user != user);
                } else {
                    s.key_warnings.retain(|w| w.verified != key);
                }
            }),
            Command::DismissKeyChange { user, shown } => self.update(|s| {
                if s.person(user).and_then(|p| p.identity_key).map(Fingerprint) == shown {
                    s.key_warnings.retain(|w| w.user != user);
                }
            }),
            Command::Unverify(key) => self.update(|s| {
                s.verified.retain(|v| v.key != key);
                s.key_warnings.retain(|w| w.verified != key);
                for p in s.people.values_mut() {
                    if p.identity_key.map(Fingerprint) == Some(key) {
                        p.verified = false;
                    }
                }
            }),
            Command::PushToTalk(down) => self.update(|s| set_mine(s, me, |_, p| p.speaking = down)),
            Command::Voice(_)
            | Command::Devices { .. }
            | Command::MicTest(_)
            | Command::PlayCue(_) => {}
            Command::LeaveServer => {
                self.update(|s| s.connection = ConnectionStatus::Disconnected);
                self.shared.emit(Event::LeftServer);
            }
            Command::Disconnect => self.update(|s| s.connection = ConnectionStatus::Disconnected),
        }
    }

    fn events(&self) -> broadcast::Receiver<Event> {
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

    fn mic_level(&self) -> Option<f32> {
        let wave = self.started.elapsed().as_secs_f32() * 3.0;
        Some(-38.0 + 14.0 * wave.sin())
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
        speaking: false,
        playback: PersonPlayback::default(),
        verified: false,
        identity_key: None,
        role: Role::User,
    }
}

fn sample_state() -> State {
    let mut people = std::collections::BTreeMap::new();
    let ilse_share = ShareConfig {
        audio: true,
        kbps: 10_000,
        codec: Codec::Av1,
        width: 2560,
        height: 1440,
        fps: 60,
    };
    for (id, name, room) in [
        (1, "Felix", None),
        (2, "Mara", Some(1)),
        (3, "Jonas", Some(1)),
        (4, "Ilse", Some(1)),
        (5, "Teo", Some(2)),
        (6, "Ren", Some(2)),
        (7, "Kaya", Some(4)),
        (8, "Noor", Some(4)),
    ] {
        let mut person = person(name, room);
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
        p.sharing = Some(ilse_share);
    }
    if let Some(p) = people.get_mut(&UserId(7)) {
        p.deafened = true;
    }
    State {
        server: Some(ServerView {
            address: "voice.example.net".into(),
            fingerprint: Fingerprint([0x2c; 32]),
            name: Some("Moss Co-op".into()),
            avatar: None,
        }),
        connection: ConnectionStatus::Connected,
        me: Some(UserId(1)),
        role: Some(Role::Admin),
        rooms: ["General", "Gaming", "Work", "AFK"]
            .iter()
            .enumerate()
            .map(|(i, name)| RoomView {
                id: RoomId(i as u64 + 1),
                name: (*name).into(),
                locked: *name == "Work",
            })
            .collect(),
        people,
        current: None,
        mic: true,
        deafened: false,
        key_warnings: vec![KeyWarning {
            user: UserId(3),
            verified: Fingerprint([0x7c; 32]),
            now: Fingerprint(sample_key(3)),
        }],
        verified: vec![
            VerifiedIdentity {
                key: Fingerprint(sample_key(2)),
                name: "Mara".into(),
                servers: vec![Fingerprint([0x2c; 32]), Fingerprint([0x91; 32])],
            },
            VerifiedIdentity {
                key: Fingerprint([0x7c; 32]),
                name: "Jonas".into(),
                servers: vec![Fingerprint([0x2c; 32])],
            },
            VerifiedIdentity {
                key: Fingerprint(sample_key(5)),
                name: "Teo".into(),
                servers: vec![Fingerprint([0x2c; 32])],
            },
            VerifiedIdentity {
                key: Fingerprint(sample_key(6)),
                name: "Ren".into(),
                servers: vec![Fingerprint([0x2c; 32])],
            },
        ],
        health: Health {
            rtt_ms: 18.0,
            download_kbps: 10_480,
            upload_kbps: 92,
            upload_loss: 0.0,
            download_loss: 0.3,
            dropped_for_you: 0,
            receiving: vec![
                TrackHealth {
                    label: "Ilse, screen".into(),
                    detail: share_detail(&ilse_share, "VideoToolbox"),
                    set_kbps: Some(ilse_share.kbps),
                    arriving_kbps: Some(6_100),
                    loss: Some(4.2),
                },
                TrackHealth {
                    label: "Ilse, screen audio".into(),
                    detail: SHARE_AUDIO_DETAIL.into(),
                    set_kbps: Some(SHARE_AUDIO_KBPS),
                    arriving_kbps: Some(128),
                    loss: Some(0.0),
                },
                TrackHealth {
                    label: "Mara, mic".into(),
                    detail: VOICE_DETAIL.into(),
                    set_kbps: Some(MIC_KBPS),
                    arriving_kbps: Some(61),
                    loss: Some(0.3),
                },
                TrackHealth {
                    label: "Jonas, mic".into(),
                    detail: VOICE_DETAIL.into(),
                    set_kbps: Some(MIC_KBPS),
                    arriving_kbps: None,
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
        profile_name: Some("Felix".into()),
        profile_picture: None,
    }
}

/// A sample person's identity key, as in [`sample_users`].
fn sample_key(id: u64) -> PublicKey {
    [(id as u8).wrapping_mul(37); 32]
}

fn sample_users() -> Vec<User> {
    [
        (1, "Felix", Role::Admin, None),
        (3, "Jonas", Role::Manager, Some(1)),
        (2, "Mara", Role::User, Some(3)),
        (4, "Ilse", Role::User, Some(3)),
        (5, "Teo", Role::User, Some(2)),
        (6, "Ren", Role::User, Some(5)),
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
        let client = MockClient::new();
        let mut events = client.events();
        client.command(Command::Join(Some(RoomId(1))));
        let Ok(Event::State(state)) = events.try_recv() else {
            panic!("no state")
        };
        assert_eq!(state.current.as_ref().map(|c| c.room), Some(RoomId(1)));
        client.command(Command::SetMic(false));
        assert!(!client.state().mic);
        client.command(Command::Watch(UserId(4)));
        assert_eq!(
            client.state().current.as_ref().map(|c| c.watching.clone()),
            Some(vec![UserId(4)])
        );
    }
}
