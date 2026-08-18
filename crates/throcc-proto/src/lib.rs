#![forbid(unsafe_code)]

use thiserror::Error;

pub mod fingerprint;
pub mod framing;
pub mod ids;
pub mod msg;

pub use fingerprint::Fingerprint;
pub use ids::{Epoch, MediaId, RoomId, UserId};
pub use msg::{
    Auth, AuthErr, AuthResult, Codec, ErrCode, Event, PROTO_VERSION, PeerState, Placed, Req,
    ReqEnvelope, Resp, RespEnvelope, Role, Room, ServerHello, ServerMessage, Share, Tracks, User,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("buffer too short: need {need} bytes, have {have}")]
    Short { need: usize, have: usize },

    #[error("frame of {len} bytes exceeds the {max} byte cap")]
    TooLarge { len: usize, max: usize },

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
