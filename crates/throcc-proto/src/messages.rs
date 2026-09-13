use core::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{Epoch, MediaId, RoomId, UserId};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ServerHello {
    pub server_nonce: [u8; 32],
    pub protocol: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Auth {
    pub pubkey: [u8; 32],
    pub client_nonce: [u8; 32],
    pub invite_code: Option<String>,
    pub want_room: Option<RoomId>,
    #[serde(with = "serde_arrays")]
    pub signature: [u8; 64],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AuthResult {
    Ok(InitialState),
    Err(AuthError),
}

/// Everything a client is handed on admission: enough to render without a
/// second fetch.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InitialState {
    pub me: UserId,
    pub role: Role,
    pub users: Vec<User>,
    pub rooms: Vec<Room>,
    pub placed: Placed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    UnknownKey,
    BadSignature,
    BadInvite,
    ProtocolMismatch,
    Banned,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            AuthError::UnknownKey => {
                "this key is not on the server's allowlist and no invite code was given"
            }
            AuthError::BadSignature => "the server rejected the signature over this connection",
            AuthError::BadInvite => "the invite code is unknown, expired, or already used",
            AuthError::ProtocolMismatch => "the server speaks a different protocol version",
            AuthError::Banned => "this key has been removed from the server",
        };
        f.write_str(reason)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    User,
    Manager,
    Admin,
}

impl Role {
    /// A role may only act on strictly lower ranks.
    pub fn rank(self) -> u8 {
        match self {
            Role::User => 0,
            Role::Manager => 1,
            Role::Admin => 2,
        }
    }

    pub fn from_rank(rank: u8) -> Option<Self> {
        [Role::User, Role::Manager, Role::Admin]
            .into_iter()
            .find(|role| role.rank() == rank)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct User {
    pub id: UserId,
    pub pubkey: [u8; 32],
    pub name: String,
    pub avatar: Option<[u8; 32]>,
    pub role: Role,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    pub epoch: Epoch,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RequestEnvelope {
    pub id: u32,
    pub request: Request,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ResponseEnvelope {
    pub id: u32,
    pub response: Response,
}

/// A reply or an event, tagged, since one stream carries both from server to
/// client once authenticated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Response(ResponseEnvelope),
    Event(Event),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Request {
    SetRoom(Option<RoomId>),

    SetProfile {
        name: String,
        avatar: Option<[u8; 32]>,
    },
    SetMedia {
        mic: bool,
        screen: bool,
        share_audio: bool,
        screen_kbps: u32,
        codec: Codec,
    },

    RequestKeyframe(MediaId),

    CreateRoom {
        name: String,
    },
    RenameRoom {
        room: RoomId,
        name: String,
    },
    DeleteRoom(RoomId),

    CreateInvite,
    SetRole {
        user: UserId,
        role: Role,
    },
    RemoveUser(UserId),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Response {
    Ok,
    Err { code: ErrorCode, message: String },
    Placed(Placed),
    InviteCode { code: String, expires: u64 },
    AvatarHash([u8; 32]),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Denied,
    NotFound,
    Invalid,
    Unimplemented,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Placed {
    pub room: Option<RoomId>,
    pub epoch: Epoch,
    pub tracks: Option<Tracks>,
    pub peers: Vec<PeerState>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Event {
    UserEntered {
        room: RoomId,
        epoch: Epoch,
        peer: PeerState,
    },
    UserExited {
        room: RoomId,
        epoch: Epoch,
        user: UserId,
    },
    PeerMedia {
        user: UserId,
        tracks: Tracks,
        mic: bool,
        screen: bool,
        share_audio: bool,
        screen_kbps: u32,
        codec: Codec,
    },
    ProfileChanged {
        user: UserId,
        name: String,
        avatar: Option<[u8; 32]>,
    },
    KeyframeWanted(MediaId),
    RoomCreated(Room),
    RoomRenamed {
        room: RoomId,
        name: String,
    },
    RoomDeleted(RoomId),
    UserAdded(User),
    RoleChanged {
        user: UserId,
        role: Role,
    },
    UserRemoved(UserId),
    Kicked {
        reason: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Share {
    pub video: MediaId,
    pub audio: Option<MediaId>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Tracks {
    pub mic: MediaId,
    pub shares: Vec<Share>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Av1,
    H265,
    H264,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PeerState {
    pub user: UserId,
    pub tracks: Tracks,
    pub mic: bool,
    pub screen: bool,
    pub share_audio: bool,
    pub screen_kbps: u32,
    pub codec: Codec,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{LENGTH_PREFIX_BYTES, body_len, decode, encode};
    use core::fmt::Debug;

    fn round_trip<T>(message: T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + Debug,
    {
        let frame = encode(&message).unwrap();
        let len = body_len(frame[..LENGTH_PREFIX_BYTES].try_into().unwrap()).unwrap();
        assert_eq!(len, frame.len() - LENGTH_PREFIX_BYTES);
        let decoded: T = decode(&frame[LENGTH_PREFIX_BYTES..]).unwrap();
        assert_eq!(decoded, message);
    }

    fn tracks() -> Tracks {
        Tracks {
            mic: MediaId(1),
            shares: vec![Share {
                video: MediaId(2),
                audio: Some(MediaId(3)),
            }],
        }
    }

    fn peer() -> PeerState {
        PeerState {
            user: UserId(7),
            tracks: tracks(),
            mic: true,
            screen: false,
            share_audio: false,
            screen_kbps: 4000,
            codec: Codec::Av1,
        }
    }

    fn placed() -> Placed {
        Placed {
            room: Some(RoomId(3)),
            epoch: Epoch(9),
            tracks: Some(tracks()),
            peers: vec![peer()],
        }
    }

    fn user() -> User {
        User {
            id: UserId(7),
            pubkey: [1u8; 32],
            name: "someone".into(),
            avatar: Some([2u8; 32]),
            role: Role::Manager,
        }
    }

    #[test]
    fn handshake_messages_round_trip() {
        round_trip(ServerHello {
            server_nonce: [4u8; 32],
            protocol: PROTOCOL_VERSION,
        });
        round_trip(Auth {
            pubkey: [5u8; 32],
            client_nonce: [6u8; 32],
            invite_code: Some("K7QM2X".into()),
            want_room: Some(RoomId(3)),
            signature: [7u8; 64],
        });
        round_trip(AuthResult::Ok(InitialState {
            me: UserId(7),
            role: Role::Admin,
            users: vec![user()],
            rooms: vec![Room {
                id: RoomId(3),
                name: "lounge".into(),
                epoch: Epoch(9),
            }],
            placed: placed(),
        }));
        for error in [
            AuthError::UnknownKey,
            AuthError::BadSignature,
            AuthError::BadInvite,
            AuthError::ProtocolMismatch,
            AuthError::Banned,
        ] {
            round_trip(AuthResult::Err(error));
        }
    }

    #[test]
    fn every_request_round_trips() {
        for request in [
            Request::SetRoom(None),
            Request::SetRoom(Some(RoomId(3))),
            Request::SetProfile {
                name: "someone".into(),
                avatar: None,
            },
            Request::SetMedia {
                mic: true,
                screen: true,
                share_audio: true,
                screen_kbps: 8000,
                codec: Codec::H265,
            },
            Request::RequestKeyframe(MediaId(2)),
            Request::CreateRoom {
                name: "lounge".into(),
            },
            Request::RenameRoom {
                room: RoomId(3),
                name: "quiet".into(),
            },
            Request::DeleteRoom(RoomId(3)),
            Request::CreateInvite,
            Request::SetRole {
                user: UserId(7),
                role: Role::Manager,
            },
            Request::RemoveUser(UserId(7)),
        ] {
            round_trip(RequestEnvelope { id: 42, request });
        }
    }

    #[test]
    fn every_response_round_trips() {
        for response in [
            Response::Ok,
            Response::Err {
                code: ErrorCode::Denied,
                message: "not your rank".into(),
            },
            Response::Placed(placed()),
            Response::InviteCode {
                code: "K7QM2X".into(),
                expires: 1_700_000_000,
            },
            Response::AvatarHash([8u8; 32]),
        ] {
            round_trip(ResponseEnvelope { id: 42, response });
        }
    }

    #[test]
    fn every_event_round_trips() {
        for event in [
            Event::UserEntered {
                room: RoomId(3),
                epoch: Epoch(9),
                peer: peer(),
            },
            Event::UserExited {
                room: RoomId(3),
                epoch: Epoch(10),
                user: UserId(7),
            },
            Event::PeerMedia {
                user: UserId(7),
                tracks: tracks(),
                mic: false,
                screen: true,
                share_audio: true,
                screen_kbps: 2500,
                codec: Codec::H264,
            },
            Event::ProfileChanged {
                user: UserId(7),
                name: "someone else".into(),
                avatar: None,
            },
            Event::KeyframeWanted(MediaId(2)),
            Event::RoomCreated(Room {
                id: RoomId(4),
                name: "new".into(),
                epoch: Epoch(0),
            }),
            Event::RoomRenamed {
                room: RoomId(4),
                name: "renamed".into(),
            },
            Event::RoomDeleted(RoomId(4)),
            Event::UserAdded(user()),
            Event::RoleChanged {
                user: UserId(7),
                role: Role::Admin,
            },
            Event::UserRemoved(UserId(7)),
            Event::Kicked {
                reason: "removed".into(),
            },
        ] {
            round_trip(event);
        }
    }

    #[test]
    fn ranks_order_the_roles() {
        assert!(Role::Admin.rank() > Role::Manager.rank());
        assert!(Role::Manager.rank() > Role::User.rank());
    }

    #[test]
    fn ranks_round_trip_through_their_role() {
        for role in [Role::User, Role::Manager, Role::Admin] {
            assert_eq!(Role::from_rank(role.rank()), Some(role));
        }
        assert_eq!(Role::from_rank(3), None);
    }
}
