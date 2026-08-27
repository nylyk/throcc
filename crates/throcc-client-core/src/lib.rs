#![forbid(unsafe_code)]

use thiserror::Error;

pub mod auth;
pub mod client;
pub mod connection;
pub mod control;
pub mod identity;
pub mod media;

pub use auth::Welcome;
pub use client::{Client, Command, Event};
pub use connection::Connector;
pub use identity::Keystore;
pub use media::send::MediaSender;

#[derive(Debug, Error)]
pub enum Error {
    #[error("server key mismatch for {server}: pinned {pinned}, presented {presented}")]
    PinMismatch {
        server: String,
        pinned: String,
        presented: String,
    },

    #[error("the server rejected this client: {0}")]
    Rejected(throcc_proto::AuthError),

    #[error("connection failed: {0}")]
    Connect(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("command dropped: {0}")]
    CommandDropped(String),

    #[error("audio: {0}")]
    Audio(String),

    #[error("keystore: {0}")]
    Keystore(String),

    #[error(transparent)]
    Proto(#[from] throcc_proto::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
