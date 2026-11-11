use throcc_client_core::client::{UiCommand, UiEvent};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::messages;

use crate::state::{Role, Sound, SoundLevel};

/// What an app asks the core to do while connected.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum Command {
    /// Moves us to `room`, or to the lobby.
    Join {
        room: Option<u64>,
    },
    JoinWithPassword {
        room: u64,
        password: String,
    },
    SetMic {
        on: bool,
    },
    SetDeafened {
        on: bool,
    },
    Watch {
        user: u64,
    },
    Unwatch {
        user: u64,
    },
    SetSoundLevel {
        user: u64,
        sound: Sound,
        level: SoundLevel,
    },
    /// Names the server, or clears its name. Admins only.
    NameServer {
        name: Option<String>,
    },
    /// Sets the server's picture, at most 64 KiB, or clears it. Admins only.
    SetServerPicture {
        picture: Option<Vec<u8>>,
    },
    CreateRoom {
        name: String,
        password: Option<String>,
    },
    RenameRoom {
        room: u64,
        name: String,
    },
    /// Locks the room behind a password, or unlocks it.
    SetRoomPassword {
        room: u64,
        password: Option<String>,
    },
    DeleteRoom {
        room: u64,
    },
    CreateInvite,
    ListUsers,
    SetRole {
        user: u64,
        role: Role,
    },
    /// Moves someone from `room` to the lobby.
    KickFromRoom {
        user: u64,
        room: u64,
    },
    RemoveUser {
        user: u64,
        cascade: bool,
    },
    /// Verifies the identity key whose safety number the user compared, or takes it off.
    SetVerified {
        user: u64,
        key: Vec<u8>,
        verified: bool,
    },
    /// Stops warning that this user's verified key changed. `shown` is the key looked at.
    DismissKeyChange {
        user: u64,
        shown: Option<Vec<u8>>,
    },
    Block {
        user: u64,
    },
    Unblock {
        user: u64,
    },
    /// Ends our membership of this server and forgets it.
    LeaveServer,
    /// Says the user is here and wants the link, as when the app comes to the foreground.
    Reconnect,
}

impl Command {
    /// The core's command, or `None` where a key isn't 32 bytes.
    pub(crate) fn into_core(self) -> Option<UiCommand> {
        Some(match self {
            Self::Join { room } => UiCommand::Join(room.map(RoomId)),
            Self::JoinWithPassword { room, password } => {
                UiCommand::JoinWithPassword(RoomId(room), password)
            }
            Self::SetMic { on } => UiCommand::SetMic(on),
            Self::SetDeafened { on } => UiCommand::SetDeafened(on),
            Self::Watch { user } => UiCommand::Watch(UserId(user)),
            Self::Unwatch { user } => UiCommand::Unwatch(UserId(user)),
            Self::SetSoundLevel { user, sound, level } => {
                UiCommand::SetSoundLevel(UserId(user), sound.into(), level.into())
            }
            Self::NameServer { name } => UiCommand::SetServerProfile {
                name: Some(name),
                picture: None,
            },
            Self::SetServerPicture { picture } => UiCommand::SetServerProfile {
                name: None,
                picture: Some(picture),
            },
            Self::CreateRoom { name, password } => UiCommand::CreateRoom(name, password),
            Self::RenameRoom { room, name } => UiCommand::RenameRoom(RoomId(room), name),
            Self::SetRoomPassword { room, password } => {
                UiCommand::SetRoomPassword(RoomId(room), password)
            }
            Self::DeleteRoom { room } => UiCommand::DeleteRoom(RoomId(room)),
            Self::CreateInvite => UiCommand::CreateInvite,
            Self::ListUsers => UiCommand::ListUsers,
            Self::SetRole { user, role } => UiCommand::SetRole(UserId(user), role.into()),
            Self::KickFromRoom { user, room } => UiCommand::KickFromRoom {
                user: UserId(user),
                room: RoomId(room),
            },
            Self::RemoveUser { user, cascade } => UiCommand::RemoveUser {
                user: UserId(user),
                cascade,
            },
            Self::SetVerified {
                user,
                key,
                verified,
            } => UiCommand::SetVerified {
                user: UserId(user),
                key: fingerprint(&key)?,
                verified,
            },
            Self::DismissKeyChange { user, shown } => UiCommand::DismissKeyChange {
                user: UserId(user),
                shown: match shown {
                    Some(shown) => Some(fingerprint(&shown)?),
                    None => None,
                },
            },
            Self::Block { user } => UiCommand::Block(UserId(user)),
            Self::Unblock { user } => UiCommand::Unblock(UserId(user)),
            Self::LeaveServer => UiCommand::LeaveServer,
            Self::Reconnect => UiCommand::Reconnect,
        })
    }
}

pub(crate) fn fingerprint(bytes: &[u8]) -> Option<Fingerprint> {
    Some(Fingerprint(bytes.try_into().ok()?))
}

/// What the core tells an app beside the state.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum Event {
    Invite {
        code: String,
        expires: u64,
    },
    Users {
        users: Vec<User>,
    },
    Notice {
        text: String,
    },
    RemovedFromServer {
        reason: String,
    },
    /// A picture asked for with `Core::avatar` has arrived.
    AvatarReady {
        hash: Vec<u8>,
    },
    ProfileSaved,
    ProfileRefused {
        reason: String,
    },
    ServerProfileSaved,
    ServerProfileRefused {
        reason: String,
    },
    PasswordRefused {
        reason: String,
    },
    /// We left the server for good, and it is forgotten.
    LeftServer,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub role: Role,
    pub invited_by: Option<u64>,
    pub identity_key: Vec<u8>,
    /// When the account was made, in unix seconds.
    pub created_at: u64,
}

impl From<messages::User> for User {
    fn from(user: messages::User) -> Self {
        Self {
            id: user.id.0,
            name: user.name,
            role: user.role.into(),
            invited_by: user.invited_by.map(|by| by.0),
            identity_key: user.identity_key.to_vec(),
            created_at: user.created_at,
        }
    }
}

impl Event {
    /// The event for an app, or `None` for a state change, which goes out as the state.
    pub(crate) fn from_core(event: UiEvent) -> Option<Self> {
        Some(match event {
            UiEvent::State(_) => return None,
            UiEvent::Invite { code, expires } => Self::Invite { code, expires },
            UiEvent::Users(users) => Self::Users {
                users: users.into_iter().map(User::from).collect(),
            },
            UiEvent::Notice(text) => Self::Notice { text },
            UiEvent::RemovedFromServer(reason) => Self::RemovedFromServer { reason },
            UiEvent::AvatarReady(hash) => Self::AvatarReady {
                hash: hash.0.to_vec(),
            },
            UiEvent::ProfileSaved => Self::ProfileSaved,
            UiEvent::ProfileRefused(reason) => Self::ProfileRefused { reason },
            UiEvent::ServerProfileSaved => Self::ServerProfileSaved,
            UiEvent::ServerProfileRefused(reason) => Self::ServerProfileRefused { reason },
            UiEvent::PasswordRefused(reason) => Self::PasswordRefused { reason },
            UiEvent::LeftServer => Self::LeftServer,
        })
    }
}
