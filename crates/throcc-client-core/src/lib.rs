#![forbid(unsafe_code)]

use thiserror::Error;

pub mod client;
pub mod connection;
pub mod control;
pub mod identity;

pub use client::{Client, Cmd, Event};
pub use connection::Connector;
pub use identity::Keystore;

#[derive(Debug, Error)]
pub enum Error {
    #[error("server key mismatch for {server}: pinned {pinned}, presented {presented}")]
    PinMismatch {
        server: String,
        pinned: String,
        presented: String,
    },

    #[error("connection failed: {0}")]
    Connect(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("keystore: {0}")]
    Keystore(String),

    #[error(transparent)]
    Proto(#[from] throcc_proto::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
