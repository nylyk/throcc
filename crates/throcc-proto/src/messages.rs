use serde::{Deserialize, Serialize};

use crate::ids::{Epoch, Hash, MediaId, Role, RoomId, UserId};

pub const PROTOCOL: u16 = 1;
pub const MAX_AVATAR_BYTES: usize = 64 * 1024;
pub const MAX_NAME_CHARS: usize = 32;

pub type PublicKey = [u8; 32];
pub type Signature = [u8; 64];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub nonce: [u8; 32],
    pub protocol: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hello {
    Auth {
        pubkey: PublicKey,
        client_nonce: [u8; 32],
        want_room: Option<RoomId>,
        #[serde(with = "sig")]
        signature: Signature,
    },
    Enroll {
        code_id: String,
        pubkey: PublicKey,
        client_nonce: [u8; 32],
        pake_a: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakeReply {
    pub pake_b: Vec<u8>,
    pub confirm_b: [u8; 32],
}

/// The server's answer to `Hello::Enroll`: the PAKE reply, or a refusal before any PAKE.
pub type PakeStep = Result<PakeReply, AuthError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakeFinish {
    pub confirm_a: [u8; 32],
    #[serde(with = "sig")]
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthResult {
    Ok(Box<Welcome>),
    Err(AuthError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub me: UserId,
    pub role: Role,
    pub rooms: Vec<Room>,
    pub presence: Vec<Presence>,
    pub placed: Placed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum AuthError {
    #[error("this server doesn't know your key; join with an invite code")]
    UnknownKey,
    #[error("the signature didn't verify")]
    BadSignature,
    #[error("the invite code is wrong, expired or used up")]
    BadInvite,
    #[error("the server speaks a different protocol version")]
    ProtocolMismatch,
    #[error("you have been removed from this server")]
    Banned,
    #[error("too many attempts; try again later")]
    RateLimited,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    Request { id: u32, req: Request },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Response { id: u32, res: Response },
    Event(Event),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    SetRoom(Option<RoomId>),
    SetProfile {
        name: String,
        avatar: Option<Hash>,
    },
    SetAvatar(Vec<u8>),
    GetAvatar(Hash),
    SetEncKey(SignedEncKey),
    SetMedia(MediaState),
    SetWatching(Vec<MediaId>),
    RequestKeyframe {
        media_id: MediaId,
        kind: Recovery,
    },
    PublishKey {
        epoch: Epoch,
        wrapped: Vec<(UserId, Vec<u8>)>,
    },
    CreateRoom {
        name: String,
    },
    RenameRoom {
        room: RoomId,
        name: String,
    },
    DeleteRoom(RoomId),
    CreateInvite,
    ListUsers,
    SetRole {
        user: UserId,
        role: Role,
    },
    RemoveUser {
        user: UserId,
        cascade: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Err { code: ErrorCode, message: String },
    Placed(Placed),
    Invite { code: String, expires: u64 },
    Users(Vec<User>),
    AvatarHash(Hash),
    Avatar(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    Forbidden,
    NotFound,
    RateLimited,
    Invalid,
    TooLarge,
    StaleEpoch,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    PresenceChanged(Presence),
    PresenceEnded(UserId),
    RoomCreated(Room),
    RoomRenamed {
        room: RoomId,
        name: String,
    },
    RoomDeleted(RoomId),
    RoomState {
        room: RoomId,
        epoch: Epoch,
        peers: Vec<RoomPeer>,
    },
    SenderKey {
        from: UserId,
        epoch: Epoch,
        wrapped: Vec<u8>,
    },
    KeyframeWanted {
        media_id: MediaId,
        kind: Recovery,
    },
    LinkStats {
        tracks: Vec<TrackStats>,
    },
    UserAdded(User),
    RoleChanged {
        user: UserId,
        role: Role,
    },
    UserRemoved(UserId),
    InviteBurned {
        code_id: String,
    },
    Kicked {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub name: String,
    pub role: Role,
    pub invited_by: Option<UserId>,
    pub identity_key: PublicKey,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    pub room: Option<RoomId>,
    pub epoch: Epoch,
    pub tracks: Option<Tracks>,
    pub peers: Vec<RoomPeer>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaState {
    pub mic: bool,
    pub deafened: bool,
    pub share: Option<ShareConfig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareConfig {
    pub audio: bool,
    pub kbps: u32,
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
}

/// Announced by the sender, never negotiated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    Av1,
    Hevc,
    H264,
    Vp9,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Recovery {
    Refresh,
    Idr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub video: MediaId,
    pub audio: MediaId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracks {
    pub mic: MediaId,
    /// One share today; a list so a second monitor is additive.
    pub shares: Vec<Share>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub user: UserId,
    pub name: String,
    pub avatar: Option<Hash>,
    pub room: Option<RoomId>,
    pub mic: bool,
    pub deafened: bool,
    pub sharing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPeer {
    pub user: UserId,
    pub tracks: Tracks,
    pub share: Option<ShareConfig>,
    pub identity_key: PublicKey,
    pub enc_key: SignedEncKey,
}

/// An X25519 encryption key, signed by its owner's identity key for one server and user,
/// valid until `not_after` (unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEncKey {
    pub key: PublicKey,
    pub not_after: u64,
    #[serde(with = "sig")]
    pub sig: Signature,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackStats {
    pub media_id: MediaId,
    pub forwarded: u32,
    pub dropped_units: u32,
}

/// Serde for 64-byte signatures, which serde's array support stops short of.
mod sig {
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};

    use super::Signature;

    pub fn serialize<S: Serializer>(bytes: &Signature, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Signature, D::Error> {
        struct SigVisitor;
        impl Visitor<'_> for SigVisitor {
            type Value = Signature;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("64 bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Signature, E> {
                v.try_into().map_err(|_| E::invalid_length(v.len(), &self))
            }
        }
        d.deserialize_bytes(SigVisitor)
    }
}
