use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, Hash, RoomId, UserId};

use crate::json_file;

const FILE: &str = "servers.json";

/// What this computer keeps per server.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Servers {
    /// Pinned server fingerprints, by the address they were pinned at.
    pub known: BTreeMap<String, Hex>,
    pub servers: BTreeMap<Hex, Server>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Server {
    pub name: Option<String>,
    pub avatar: Option<Hex>,
    /// The newest epoch each of our users took part in, per room.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub epochs: BTreeMap<UserId, BTreeMap<RoomId, Epoch>>,
}

impl json_file::Format for Servers {}

impl Servers {
    pub fn load(dir: &Path) -> Result<Self> {
        json_file::load(&dir.join(FILE))
    }

    pub fn update(dir: &Path, change: impl FnOnce(&mut Self)) -> Result<()> {
        json_file::update(&dir.join(FILE), change)
    }

    /// Takes what is kept for `server`, leaving nothing behind.
    pub fn take(mut self, server: &Fingerprint) -> Server {
        self.servers.remove(&Hex(server.0)).unwrap_or_default()
    }

    pub fn server_mut(&mut self, server: &Fingerprint) -> &mut Server {
        self.servers.entry(Hex(server.0)).or_default()
    }
}

/// 32 bytes, kept as 64 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Hex(pub [u8; 32]);

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        hex32::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        hex32::deserialize(deserializer).map(Self)
    }
}

/// Serde for 32 bytes as 64 hex digits, for `#[serde(with = "hex32")]`.
pub(crate) mod hex32 {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Hash(*bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let digits = String::deserialize(deserializer)?;
        let parsed: Fingerprint = digits.parse().map_err(serde::de::Error::custom)?;
        Ok(parsed.0)
    }
}
