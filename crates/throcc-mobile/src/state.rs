use throcc_client_core::state as core;
use throcc_proto::ids;

/// Everything the apps show while connected, sent whole after every change.
#[derive(Clone, Debug, Default, PartialEq, uniffi::Record)]
pub struct CoreState {
    pub server: Option<Server>,
    pub connection: Connection,
    pub me: Option<u64>,
    pub role: Option<Role>,
    pub rooms: Vec<Room>,
    pub people: Vec<Person>,
    pub current: Option<CurrentRoom>,
    pub mic: bool,
    pub deafened: bool,
    pub key_warnings: Vec<KeyWarning>,
    pub health: Health,
    /// What to tell the user about a connection that isn't up.
    pub notice: Option<ConnectionNotice>,
    /// The room we are in while the connection is up or coming back, which the call's audio
    /// follows.
    pub live_room: Option<u64>,
    /// Whether a session holds on to the server, so picking it again only shows it.
    pub holds_session: bool,
}

/// What became of a connection that isn't up, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConnectionNotice {
    /// All of it in one line.
    pub summary: String,
    /// What happens next, or what the user can do.
    pub hint: String,
    /// What went wrong, where something did.
    pub problem: Option<String>,
    pub action: Option<NoticeAction>,
    /// Whether the core is at work on it.
    pub busy: bool,
    /// Whether the connection ended, rather than coming back.
    pub serious: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NoticeAction {
    /// `Command::Reconnect`.
    Reconnect,
    /// Connect to the server again, which signs the other device out.
    SignInHere,
    /// Connect to the server again.
    TryAgain,
}

/// What picking a room does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JoinAction {
    AlreadyIn,
    /// Ask for its password, then `Command::JoinWithPassword`.
    AskPassword,
    Join,
}

/// What we know of the identity key the server shows for someone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum KeyTrust {
    /// It replaced a key the user verified.
    KeyChanged,
    Unverified,
    /// Verified, or our own.
    Verified,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Server {
    pub address: String,
    pub fingerprint: Vec<u8>,
    pub name: Option<String>,
    pub avatar: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum Connection {
    #[default]
    Disconnected,
    Connected,
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    /// The link was down too long to sign in again unasked; `Command::Reconnect` does.
    AwaitingReconnect,
    SignedInElsewhere,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Role {
    User,
    Manager,
    Admin,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Room {
    pub id: u64,
    pub name: String,
    pub locked: bool,
    /// What picking it does.
    pub join: JoinAction,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Person {
    pub user: u64,
    pub name: String,
    pub avatar: Option<Vec<u8>>,
    pub room: Option<u64>,
    pub mic: bool,
    pub deafened: bool,
    pub sharing: Option<Share>,
    /// How many people in our room watch their share.
    pub audience: u32,
    pub speaking: bool,
    pub voice: SoundLevel,
    pub share_audio: SoundLevel,
    pub verified: bool,
    /// The name they were verified under, where the server now shows another.
    pub verified_as: Option<String>,
    pub trust: KeyTrust,
    /// The letter their picture stands in with.
    pub initial: String,
    /// The identity key they presented this session, in a room with us.
    pub identity_key: Option<Vec<u8>>,
    pub role: Role,
    /// Whether the user blocked them. A blocked person keeps their place in the room.
    pub blocked: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Share {
    /// The share's height, frame rate and codec, such as "1440p60 AV1".
    pub label: String,
    pub audio: bool,
    /// The size of its pictures, in pixels.
    pub width: u32,
    pub height: u32,
    /// Its width over its height, within what a phone can show: a sender may announce any
    /// size.
    pub aspect: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Sound {
    Voice,
    Share,
}

#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct SoundLevel {
    /// From 0 to 4, where 1 plays the sound as it arrives.
    pub volume: f32,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CurrentRoom {
    pub room: u64,
    pub sharing: bool,
    /// Whose shares we receive.
    pub watching: Vec<u64>,
    /// Whose watched shares have a picture to show.
    pub showing: Vec<u64>,
}

/// The server showed `user`, whose identity key `verified` was verified, with the key `now`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct KeyWarning {
    pub user: u64,
    pub verified: Vec<u8>,
    pub now: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, uniffi::Record)]
pub struct Health {
    pub rtt_ms: f32,
    pub download_kbps: u32,
    pub upload_kbps: u32,
    pub upload_loss: f32,
    pub download_loss: f32,
    /// Sharers our connection can't keep up with.
    pub slow_shares: Vec<u64>,
    /// Sharers whose pictures this device decodes too slowly to keep up with.
    pub lagging_shares: Vec<u64>,
}

impl From<&core::State> for CoreState {
    fn from(state: &core::State) -> Self {
        Self {
            server: state.server.as_ref().map(|server| Server {
                address: server.address.clone(),
                fingerprint: server.fingerprint.0.to_vec(),
                name: server.name.clone(),
                avatar: server.avatar.map(|hash| hash.0.to_vec()),
            }),
            connection: (&state.connection).into(),
            me: state.me.map(|me| me.0),
            role: state.role.map(Role::from),
            rooms: state
                .rooms
                .iter()
                .map(|room| Room {
                    join: state.join_action(room.id).into(),
                    ..Room::from(room)
                })
                .collect(),
            people: state
                .people
                .iter()
                .map(|(user, person)| Person {
                    trust: state.trust(*user).into(),
                    ..Person::from((user, person))
                })
                .collect(),
            current: state.current.as_ref().map(|current| CurrentRoom {
                room: current.room.0,
                sharing: current.sharing.is_some(),
                watching: current.watching.iter().map(|user| user.0).collect(),
                showing: current.showing.iter().map(|user| user.0).collect(),
            }),
            mic: state.mic,
            deafened: state.deafened,
            key_warnings: state.key_warnings.iter().map(KeyWarning::from).collect(),
            health: Health {
                rtt_ms: state.health.rtt_ms,
                download_kbps: state.health.download_kbps,
                upload_kbps: state.health.upload_kbps,
                upload_loss: state.health.upload_loss,
                download_loss: state.health.download_loss,
                slow_shares: state.health.slow_shares.iter().map(|u| u.0).collect(),
                lagging_shares: state.health.lagging_shares.iter().map(|u| u.0).collect(),
            },
            notice: state.connection.notice().map(ConnectionNotice::from),
            live_room: state.live_room().map(|room| room.0),
            holds_session: state.connection.holds_session(),
        }
    }
}

impl From<&core::ConnectionStatus> for Connection {
    fn from(connection: &core::ConnectionStatus) -> Self {
        match connection {
            core::ConnectionStatus::Disconnected => Self::Disconnected,
            core::ConnectionStatus::Connected => Self::Connected,
            core::ConnectionStatus::Reconnecting { attempt, reason } => Self::Reconnecting {
                attempt: *attempt,
                reason: reason.clone(),
            },
            core::ConnectionStatus::AwaitingReconnect => Self::AwaitingReconnect,
            core::ConnectionStatus::SignedInElsewhere => Self::SignedInElsewhere,
            core::ConnectionStatus::Failed(reason) => Self::Failed {
                reason: reason.clone(),
            },
        }
    }
}

impl From<ids::Role> for Role {
    fn from(role: ids::Role) -> Self {
        match role {
            ids::Role::User => Self::User,
            ids::Role::Manager => Self::Manager,
            ids::Role::Admin => Self::Admin,
        }
    }
}

impl From<Role> for ids::Role {
    fn from(role: Role) -> Self {
        match role {
            Role::User => Self::User,
            Role::Manager => Self::Manager,
            Role::Admin => Self::Admin,
        }
    }
}

impl From<&core::RoomView> for Room {
    fn from(room: &core::RoomView) -> Self {
        Self {
            id: room.id.0,
            name: room.name.clone(),
            locked: room.locked,
            join: JoinAction::Join,
        }
    }
}

impl From<(&ids::UserId, &core::Person)> for Person {
    fn from((user, person): (&ids::UserId, &core::Person)) -> Self {
        Self {
            user: user.0,
            name: person.name.clone(),
            avatar: person.avatar.map(|hash| hash.0.to_vec()),
            room: person.room.map(|room| room.0),
            mic: person.mic,
            deafened: person.deafened,
            sharing: person.sharing.as_ref().map(|config| Share {
                label: core::share_label(config),
                audio: config.audio,
                width: config.width.into(),
                height: config.height.into(),
                aspect: (f32::from(config.width) / f32::from(config.height.max(1)))
                    .clamp(0.25, 4.0),
            }),
            audience: person.audience as u32,
            speaking: person.speaking,
            voice: person.playback.voice.into(),
            share_audio: person.playback.share.into(),
            verified: person.verified,
            verified_as: person.verified_as().map(str::to_owned),
            trust: if person.verified {
                KeyTrust::Verified
            } else {
                KeyTrust::Unverified
            },
            initial: core::initial(&person.name),
            identity_key: person.identity_key.map(|key| key.to_vec()),
            role: person.role.into(),
            blocked: person.blocked,
        }
    }
}

impl From<core::SoundLevel> for SoundLevel {
    fn from(level: core::SoundLevel) -> Self {
        Self {
            volume: level.volume,
            muted: level.muted,
        }
    }
}

impl From<SoundLevel> for core::SoundLevel {
    fn from(level: SoundLevel) -> Self {
        Self {
            volume: level.volume.clamp(0.0, core::MAX_VOLUME),
            muted: level.muted,
        }
    }
}

impl From<Sound> for core::Sound {
    fn from(sound: Sound) -> Self {
        match sound {
            Sound::Voice => Self::Voice,
            Sound::Share => Self::Share,
        }
    }
}

impl From<&core::KeyWarning> for KeyWarning {
    fn from(warning: &core::KeyWarning) -> Self {
        Self {
            user: warning.user.0,
            verified: warning.verified.0.to_vec(),
            now: warning.now.0.to_vec(),
        }
    }
}

impl From<core::ConnectionNotice> for ConnectionNotice {
    fn from(notice: core::ConnectionNotice) -> Self {
        Self {
            summary: notice.summary,
            hint: notice.hint,
            problem: notice.problem,
            action: notice.action.map(|action| match action {
                core::NoticeAction::Reconnect => NoticeAction::Reconnect,
                core::NoticeAction::SignInHere => NoticeAction::SignInHere,
                core::NoticeAction::TryAgain => NoticeAction::TryAgain,
            }),
            busy: notice.busy,
            serious: notice.serious,
        }
    }
}

impl From<core::JoinAction> for JoinAction {
    fn from(action: core::JoinAction) -> Self {
        match action {
            core::JoinAction::AlreadyIn => Self::AlreadyIn,
            core::JoinAction::AskPassword => Self::AskPassword,
            core::JoinAction::Join => Self::Join,
        }
    }
}

impl From<core::KeyTrust> for KeyTrust {
    fn from(trust: core::KeyTrust) -> Self {
        match trust {
            core::KeyTrust::KeyChanged => Self::KeyChanged,
            core::KeyTrust::Unverified => Self::Unverified,
            core::KeyTrust::Verified => Self::Verified,
        }
    }
}

impl From<Connection> for core::ConnectionStatus {
    fn from(connection: Connection) -> Self {
        match connection {
            Connection::Disconnected => Self::Disconnected,
            Connection::Connected => Self::Connected,
            Connection::Reconnecting { attempt, reason } => Self::Reconnecting { attempt, reason },
            Connection::AwaitingReconnect => Self::AwaitingReconnect,
            Connection::SignedInElsewhere => Self::SignedInElsewhere,
            Connection::Failed { reason } => Self::Failed(reason),
        }
    }
}
