#![forbid(unsafe_code)]

use thiserror::Error;

pub mod auth;
pub mod fingerprint;
pub mod frame;
pub mod framing;
pub mod ids;
pub mod messages;

pub use fingerprint::Fingerprint;
pub use frame::{
    FrameHeader, HEADER_BYTES, MIN_DATAGRAM_BYTES, PAYLOAD_BUDGET, check_datagram_size,
};
pub use ids::{Epoch, MediaId, RoomId, UserId};
pub use messages::{
    Auth, AuthError, AuthResult, Codec, ErrorCode, Event, PROTOCOL_VERSION, PeerState, Placed,
    Request, RequestEnvelope, Response, ResponseEnvelope, Role, Room, ServerHello, ServerMessage,
    Share, Tracks, User,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("buffer too short: need {need} bytes, have {have}")]
    Short { need: usize, have: usize },

    #[error("frame of {len} bytes exceeds the {max} byte cap")]
    TooLarge { len: usize, max: usize },

    #[error("{0}")]
    Datagram(String),

    #[error("malformed certificate: {0}")]
    Certificate(&'static str),

    #[error("malformed fingerprint: {0}")]
    Fingerprint(&'static str),

    #[error(transparent)]
    Postcard(#[from] postcard::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub const ALPN: &[u8] = b"throcc/1";
pub const DEFAULT_PORT: u16 = 8476;
