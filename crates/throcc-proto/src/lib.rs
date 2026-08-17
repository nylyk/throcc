#![forbid(unsafe_code)]

use thiserror::Error;

pub mod fingerprint;

pub use fingerprint::Fingerprint;

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
