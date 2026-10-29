use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use std::path::PathBuf;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::messages::{Codec, ShareConfig, User};

use throcc_crypto::safety;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::PublicKey;
use tokio::sync::{broadcast, mpsc};

use crate::account::{self, SignInError};
use crate::call::Call;
use crate::contacts::Contacts;
use crate::keystore::Keystore;
use crate::media::audio::VoiceSettings;
use crate::media::video::Picture;
use crate::media::video::frame::VideoFrame;
pub use crate::session::ClientOptions;
use crate::session::{Session, Shared};

use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, Person, RoomView, ServerView, State,
    TrackHealth,
};

const EVENTS: usize = 256;

/// What the UI asks the core to do. The core's own type, never `throcc-proto`'s.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Join(Option<RoomId>),
    SetMic(bool),
    SetDeafened(bool),
    StartShare(ShareConfig),
    StopShare,
    Watch(UserId),
    Unwatch(UserId),
    SetVolume(UserId, f32),
    SetName(String),
    /// Sets our profile picture, at most 64 KiB, or clears it.
    SetAvatar(Option<Vec<u8>>),
    FetchAvatar(Hash),
    CreateRoom(String),
    RenameRoom(RoomId, String),
    DeleteRoom(RoomId),
    CreateInvite,
    ListUsers,
    SetRole(UserId, Role),
    RemoveUser {
        user: UserId,
        cascade: bool,
    },
    AcceptKey(UserId),
    SetVerified(UserId, bool),
    Voice(VoiceSettings),
    Devices {
        input: Option<String>,
        output: Option<String>,
    },
    /// Plays your processed mic back to you.
    MicTest(bool),
    PushToTalk(bool),
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
}

/// The UI's view of a client. Implemented by the real client and by [`MockClient`].
pub trait ClientApi: Send + Sync {
    fn command(&self, command: Command);
    fn events(&self) -> broadcast::Receiver<Event>;
    fn state(&self) -> Arc<State>;
    /// The 60-digit safety number with `user`, if both identity keys are known.
    fn safety_number(&self, user: UserId) -> Option<String>;
    /// The newest picture of `user`'s share while watched, or of ours while sharing.
    fn picture(&self, user: UserId) -> Option<Picture>;
    /// The profile picture behind `hash`, fetched in the background on first ask.
    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>>;
}

const COMMANDS: usize = 64;

/// A live client: tokio runs on its own threads, and a session task keeps [`State`]
/// current, drives media and reconnects with the room it was in.
pub struct Client {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    identity: PublicKey,
    dir: PathBuf,
    server: Fingerprint,
    runtime: Option<tokio::runtime::Runtime>,
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
        let link = runtime.block_on(async {
            match invite {
                Some(code) => account::enroll(&mut keystore, address, code).await,
                None => account::sign_in(&keystore, address, None).await,
            }
        })?;
        let server = link.connected.fingerprint;
        let identity = keystore.identity().public();
        let dir = keystore.dir().to_owned();
        let (events, _) = broadcast::channel(EVENTS);
        let shared = Arc::new(Shared {
            state: Mutex::new(Arc::new(State::default())),
            events,
            pictures: Mutex::default(),
            avatars: Mutex::default(),
        });
        let (commands, commands_rx) = mpsc::channel(COMMANDS);
        let call = {
            let _guard = runtime.enter();
            Call::start(link, &keystore).map_err(SignInError::Store)?
        };
        let session = {
            let _guard = runtime.enter();
            Session::new(shared.clone(), keystore, address.to_owned(), options, call)
        };
        runtime.spawn(session.run(commands_rx));
        Ok(Self {
            commands,
            shared,
            identity,
            dir,
            server,
            runtime: Some(runtime),
        })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.commands.try_send(Command::Disconnect).ok();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        }
    }
}

impl ClientApi for Client {
    fn command(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            self.shared
                .events
                .send(Event::Notice("busy; try again".into()))
                .ok();
        }
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        self.shared.events.subscribe()
    }

    fn state(&self) -> Arc<State> {
        self.shared.state()
    }

    fn safety_number(&self, user: UserId) -> Option<String> {
        let contacts = Contacts::load(&self.dir, &self.server).ok()?;
        Some(safety::safety_number(
            &self.identity,
            &contacts.get(user)?.key,
        ))
    }

    fn picture(&self, user: UserId) -> Option<Picture> {
        self.shared.picture(user)
    }

    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>> {
        let (bytes, fetch) = self.shared.avatar(hash);
        if fetch {
            self.command(Command::FetchAvatar(hash));
        }
        bytes
    }
}

/// A client with canned people and rooms that reacts to commands locally.
pub struct MockClient {
    state: Mutex<Arc<State>>,
    events: broadcast::Sender<Event>,
    avatars: Mutex<HashMap<Hash, Arc<[u8]>>>,
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockClient {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENTS);
        Self {
            state: Mutex::new(Arc::new(sample_state())),
            events,
            avatars: Mutex::default(),
        }
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = (**guard).clone();
        change(&mut state);
        *guard = Arc::new(state);
        self.events.send(Event::State(guard.clone())).ok();
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
                if let Some(person) = s.people.get_mut(&me) {
                    person.room = room;
                    person.mic = false;
                    person.sharing = None;
                }
                s.current = room.map(|room| CurrentRoom {
                    room,
                    mic: false,
                    deafened: false,
                    sharing: None,
                    watching: vec![],
                });
            }),
            Command::SetMic(on) => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.mic = on;
                    p.mic = on;
                })
            }),
            Command::SetDeafened(on) => self.update(|s| {
                set_mine(s, me, |c, p| {
                    c.deafened = on;
                    p.deafened = on;
                })
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
            Command::SetVolume(user, volume) => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user) {
                    p.volume = volume;
                }
            }),
            Command::SetName(name) => self.update(|s| {
                if let Some(p) = s.people.get_mut(&me) {
                    p.name = name;
                }
            }),
            Command::SetAvatar(picture) => {
                let hash = picture.map(|bytes| {
                    let hash = Hash(<sha2::Sha256 as sha2::Digest>::digest(&bytes).into());
                    self.avatars
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(hash, bytes.into());
                    hash
                });
                self.update(|s| {
                    if let Some(p) = s.people.get_mut(&me) {
                        p.avatar = hash;
                    }
                });
            }
            Command::FetchAvatar(_) => {}
            Command::CreateRoom(name) => self.update(|s| {
                let id = RoomId(s.rooms.iter().map(|r| r.id.0).max().unwrap_or(0) + 1);
                s.rooms.push(RoomView { id, name });
            }),
            Command::RenameRoom(room, name) => self.update(|s| {
                if let Some(r) = s.rooms.iter_mut().find(|r| r.id == room) {
                    r.name = name;
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
            Command::CreateInvite => {
                self.events
                    .send(Event::Invite {
                        code: "K7Q-M2XPD".into(),
                        expires: 0,
                    })
                    .ok();
            }
            Command::ListUsers => {
                self.events.send(Event::Users(sample_users())).ok();
            }
            Command::SetRole(..) | Command::RemoveUser { .. } => {
                self.events.send(Event::Users(sample_users())).ok();
            }
            Command::AcceptKey(user) => self.update(|s| s.key_warnings.retain(|w| w.user != user)),
            Command::SetVerified(user, verified) => self.update(|s| {
                if let Some(p) = s.people.get_mut(&user) {
                    p.verified = verified;
                }
            }),
            Command::Voice(_)
            | Command::Devices { .. }
            | Command::MicTest(_)
            | Command::PushToTalk(_) => {}
            Command::Disconnect => self.update(|s| s.connection = ConnectionStatus::Disconnected),
        }
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn state(&self) -> Arc<State> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn safety_number(&self, _user: UserId) -> Option<String> {
        Some("482139057716024733195580241196087436295031487266089415270031".into())
    }

    fn picture(&self, _user: UserId) -> Option<Picture> {
        let frame = VideoFrame::test_pattern(1280, 720, 1, 0);
        Some(tokio::sync::watch::channel(Some(Arc::new(frame))).1)
    }

    fn avatar(&self, hash: Hash) -> Option<Arc<[u8]>> {
        self.avatars
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hash)
            .cloned()
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
        volume: 1.0,
        verified: false,
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
    ] {
        people.insert(UserId(id), person(name, room));
    }
    if let Some(p) = people.get_mut(&UserId(2)) {
        p.speaking = true;
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
        server: Some(ServerView { address: "voice.example.net".into(), fingerprint: Fingerprint([0x2c; 32]) }),
        connection: ConnectionStatus::Connected,
        me: Some(UserId(1)),
        role: Some(Role::Admin),
        rooms: ["General", "Gaming", "Work", "AFK"]
            .iter()
            .enumerate()
            .map(|(i, name)| RoomView { id: RoomId(i as u64 + 1), name: (*name).into() })
            .collect(),
        people,
        current: None,
        key_warnings: vec![KeyWarning { user: UserId(3), pinned: Fingerprint([0x7c; 32]), now: Fingerprint([0xb8; 32]) }],
        health: Health {
            rtt_ms: 18.0,
            upload_loss: 0.0,
            download_loss: 0.3,
            dropped_for_you: 0,
            verdict: "Ilse's screen arrives at 6.1 of the 10 Mbps she set. Nothing was dropped for you, so her upload is the bottleneck.".into(),
            receiving: vec![
                TrackHealth { label: "Ilse, screen".into(), set_kbps: Some(10_000), arriving_kbps: Some(6_100), loss: Some(4.2) },
                TrackHealth { label: "Ilse, screen audio".into(), set_kbps: Some(128), arriving_kbps: Some(128), loss: Some(0.0) },
                TrackHealth { label: "Mara, mic".into(), set_kbps: Some(32), arriving_kbps: Some(31), loss: Some(0.3) },
                TrackHealth { label: "Jonas, mic".into(), set_kbps: Some(32), arriving_kbps: None, loss: None },
            ],
            sending: "mic 32 kbps, 0 dropped".into(),
        },
    }
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
        identity_key: [id as u8 * 37; 32],
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
        client.command(Command::SetMic(true));
        assert!(client.state().current.as_ref().is_some_and(|c| c.mic));
        client.command(Command::Watch(UserId(4)));
        assert_eq!(
            client.state().current.as_ref().map(|c| c.watching.clone()),
            Some(vec![UserId(4)])
        );
    }
}
