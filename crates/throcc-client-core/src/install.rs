//! What this install keeps beside any one connection: the settings, who is verified and
//! blocked, and the servers joined with how they last looked. A file that can't be read
//! never stops the app: it starts without it and says why.

use std::path::Path;
use std::sync::Arc;

use throcc_proto::Fingerprint;
use throcc_proto::ids::Hash;
use tracing::warn;

use crate::avatars;
use crate::blocked::BlockedStore;
use crate::client::SAMPLE_SERVER_PICTURE;
use crate::keystore::{self, Keystore};
use crate::server_look::ServerLook;
use crate::settings::Settings;
use crate::state::{BLOCKED_NAME, BlockedPerson};
use crate::verified::VerifiedStore;

/// This install's settings, or the defaults with why they couldn't be read.
pub fn load_settings() -> (Settings, Option<String>) {
    match keystore::config_dir().and_then(|dir| Settings::load(&dir)) {
        Ok(settings) => (settings, None),
        Err(e) => {
            warn!("no saved settings: {e:#}");
            let problem = format!("Couldn't read your settings ({e}), so changes aren't kept.");
            (Settings::default(), Some(problem))
        }
    }
}

/// The verified people kept on this install, with why they couldn't be read if they
/// couldn't, in which case nobody counts as verified.
pub fn load_verified() -> (VerifiedStore, Option<String>) {
    match keystore::config_dir() {
        Ok(dir) => {
            let verified = VerifiedStore::empty(&dir);
            let problem = read_verified(&verified);
            (verified, problem)
        }
        Err(e) => (VerifiedStore::in_memory(), Some(e.to_string())),
    }
}

/// Reads `verified` from its file again, returning why it couldn't be read if it couldn't.
pub fn read_verified(verified: &VerifiedStore) -> Option<String> {
    let e = verified.reload().err()?;
    warn!("no verified keys: {e:#}");
    Some(format!(
        "Couldn't read who you verified ({e}), so everyone shows as not verified."
    ))
}

/// The blocked people kept on this install, with why they couldn't be read if they
/// couldn't, in which case nobody counts as blocked.
pub fn load_blocked() -> (BlockedStore, Option<String>) {
    let dir = match keystore::config_dir() {
        Ok(dir) => dir,
        Err(e) => return (BlockedStore::in_memory(), Some(e.to_string())),
    };
    match BlockedStore::load(&dir) {
        Ok(blocked) => (blocked, None),
        Err(e) => {
            warn!("no blocked people: {e:#}");
            let problem = format!("Couldn't read who you blocked ({e}), so nobody is blocked.");
            (BlockedStore::empty(&dir), Some(problem))
        }
    }
}

/// A server this install has joined, as it looked when last connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownServer {
    pub address: String,
    pub fingerprint: Fingerprint,
    pub name: Option<String>,
    pub picture: Option<Arc<[u8]>>,
}

/// The servers this install has joined, in the order they were joined.
pub fn known_servers() -> Vec<KnownServer> {
    let Ok(known) = Keystore::default_known_servers() else {
        return Vec::new();
    };
    let dir = keystore::config_dir().ok();
    let servers = known.iter().map(|(address, fingerprint)| {
        let look = dir
            .as_deref()
            .and_then(|dir| ServerLook::load(dir, &fingerprint).ok())
            .unwrap_or_default();
        KnownServer {
            address: address.to_owned(),
            fingerprint,
            name: look.name,
            picture: look.avatar.and_then(cached_picture),
        }
    });
    servers.collect()
}

/// The servers the sample data has joined.
pub fn sample_servers() -> Vec<KnownServer> {
    let server = |address: &str, key: u8, name: &str, picture: &[u8]| KnownServer {
        address: address.to_owned(),
        fingerprint: Fingerprint([key; 32]),
        name: Some(name.to_owned()),
        picture: Some(picture.into()),
    };
    vec![
        server("owls.thro.cc", 0x2c, "Night Owls", SAMPLE_SERVER_PICTURE),
        server(
            "192.168.1.20",
            0x5e,
            "LAN Party",
            include_bytes!("../assets/mock/lan-party.jpg"),
        ),
        server(
            "192.0.2.40",
            0x91,
            "Survival SMP",
            include_bytes!("../assets/mock/survival-smp.jpg"),
        ),
    ]
}

/// The address `server` is known by among `servers`, or its short fingerprint.
pub fn address_of(servers: &[KnownServer], server: &Fingerprint) -> String {
    let known = servers.iter().find(|known| known.fingerprint == *server);
    known.map_or_else(|| server.short(), |known| known.address.clone())
}

/// Someone verified on this install, as the verified list shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedVerified {
    pub key: Fingerprint,
    /// The name they were verified under, or [`BLOCKED_NAME`] while blocked.
    pub name: String,
    /// The servers they were seen on, by address where the server is known.
    pub servers: Vec<String>,
}

/// Everyone verified on this install.
pub fn verified_people(
    verified: &VerifiedStore,
    blocked: &BlockedStore,
    servers: &[KnownServer],
) -> Vec<ListedVerified> {
    let listed = verified.list().into_iter().map(|person| ListedVerified {
        name: if blocked.is_blocked(&person.key.0) {
            BLOCKED_NAME.to_owned()
        } else {
            person.name
        },
        servers: person
            .servers
            .iter()
            .map(|server| address_of(servers, server))
            .collect(),
        key: person.key,
    });
    listed.collect()
}

/// Someone blocked on this install, as the blocked list shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedBlocked {
    pub person: BlockedPerson,
    /// The picture they had when blocked, where it can still be found.
    pub picture: Option<Arc<[u8]>>,
    /// The server they were blocked on, by address where it is known.
    pub server: Option<String>,
}

/// Everyone blocked on this install, each with the picture they had when blocked, from the
/// cache or else from `fetched`.
pub fn blocked_people(
    blocked: &BlockedStore,
    servers: &[KnownServer],
    fetched: impl Fn(Hash) -> Option<Arc<[u8]>>,
) -> Vec<ListedBlocked> {
    let listed = blocked.list().into_iter().map(|person| ListedBlocked {
        picture: person
            .avatar
            .and_then(|hash| cached_picture(hash).or_else(|| fetched(hash))),
        server: person.server.map(|server| address_of(servers, &server)),
        person,
    });
    listed.collect()
}

/// The picture behind `hash` from this install's cache.
fn cached_picture(hash: Hash) -> Option<Arc<[u8]>> {
    let cache = keystore::cache_dir().ok()?;
    avatars::load(Path::new(&cache), hash)
}
