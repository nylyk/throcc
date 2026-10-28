use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use throcc_crypto::hpke::EncKeyPair;
use throcc_crypto::identity::{self, IdentityKey};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::SignedEncKey;
use tracing::warn;
use zeroize::Zeroizing;

use crate::known_servers::KnownServers;

const KEYCHAIN_SERVICE: &str = "throcc";
const KEYCHAIN_IDENTITY: &str = "identity";
const IDENTITY_FILE: &str = "identity_key";
const KNOWN_SERVERS_FILE: &str = "known_servers";
const ENC_KEY_FILE: &str = "enc_key";
const ENC_KEY_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The install's identity key plus everything pinned per server.
pub struct Keystore {
    dir: PathBuf,
    identity: IdentityKey,
    enc_key: EncKeyPair,
    enc_key_created: u64,
    pub known_servers: KnownServers,
}

impl Keystore {
    /// Opens the platform data directory, keeping the identity key in the OS keychain.
    /// Falls back to a 0600 file when no keychain is available.
    pub fn open_default() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "throcc")
            .context("no home directory to keep throcc's data in")?;
        let dir = dirs.data_dir().to_owned();
        fs::create_dir_all(&dir)?;
        let identity = match keychain_identity() {
            Ok(identity) => identity,
            Err(e) => {
                warn!("OS keychain unavailable ({e:#}); keeping the identity key in a file");
                file_identity(&dir)?
            }
        };
        Self::assemble(dir, identity)
    }

    /// Opens `dir` with the identity key in a 0600 file. Used for profiles and tests.
    pub fn open_in(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let identity = file_identity(dir)?;
        Self::assemble(dir.to_owned(), identity)
    }

    fn assemble(dir: PathBuf, identity: IdentityKey) -> Result<Self> {
        let known_servers = KnownServers::load(&dir.join(KNOWN_SERVERS_FILE))?;
        let (enc_key, enc_key_created) = load_enc_key(&dir.join(ENC_KEY_FILE))?;
        Ok(Self {
            dir,
            identity,
            enc_key,
            enc_key_created,
            known_servers,
        })
    }

    pub fn identity(&self) -> &IdentityKey {
        &self.identity
    }

    pub fn enc_key(&self) -> &EncKeyPair {
        &self.enc_key
    }

    /// The encryption key, signed for one server and user until it is due for rotation.
    pub fn signed_enc_key(&self, server: &Fingerprint, user: UserId) -> SignedEncKey {
        let key = self.enc_key.public();
        let not_after = self.enc_key_created + ENC_KEY_LIFETIME.as_secs();
        let message = identity::enc_key_message(&server.0, user, not_after, &key);
        SignedEncKey {
            key,
            not_after,
            sig: self.identity.sign(&message),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Loads the encryption key, replacing it once it is older than 30 days.
fn load_enc_key(path: &Path) -> Result<(EncKeyPair, u64)> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if let Ok(bytes) = fs::read(path).map(Zeroizing::new)
        && let Some((secret, created)) = bytes.split_first_chunk::<32>()
        && let Ok(created) = <[u8; 8]>::try_from(created)
        && now.saturating_sub(u64::from_be_bytes(created)) < ENC_KEY_LIFETIME.as_secs()
    {
        return Ok((
            EncKeyPair::from_secret(secret)?,
            u64::from_be_bytes(created),
        ));
    }
    let key = EncKeyPair::generate();
    let mut bytes = Zeroizing::new(key.secret().to_vec());
    bytes.extend_from_slice(&now.to_be_bytes());
    write_private(path, &bytes)?;
    Ok((key, now))
}

fn keychain_identity() -> Result<IdentityKey> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_IDENTITY)?;
    match entry.get_secret() {
        Ok(secret) => seed_to_key(&secret),
        Err(keyring::Error::NoEntry) => {
            let key = IdentityKey::generate();
            entry.set_secret(key.seed().as_slice())?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

fn file_identity(dir: &Path) -> Result<IdentityKey> {
    let path = dir.join(IDENTITY_FILE);
    match fs::read(&path) {
        Ok(bytes) => seed_to_key(&Zeroizing::new(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let key = IdentityKey::generate();
            write_private(&path, key.seed().as_slice())?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn seed_to_key(bytes: &[u8]) -> Result<IdentityKey> {
    let Ok(seed) = <&[u8; 32]>::try_from(bytes) else {
        bail!("the stored identity key is corrupt");
    };
    Ok(IdentityKey::from_seed(seed))
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_identity_persists() {
        let dir = tempfile::tempdir().unwrap();
        let first = Keystore::open_in(dir.path()).unwrap().identity().public();
        let second = Keystore::open_in(dir.path()).unwrap().identity().public();
        assert_eq!(first, second);
    }

    #[test]
    fn enc_key_persists_and_is_signed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Keystore::open_in(dir.path()).unwrap();
        let server = Fingerprint([3; 32]);
        let signed = store.signed_enc_key(&server, UserId(9));
        assert_eq!(
            Keystore::open_in(dir.path()).unwrap().enc_key().public(),
            signed.key
        );
        let message =
            identity::enc_key_message(&server.0, UserId(9), signed.not_after, &signed.key);
        assert!(identity::verify(&store.identity().public(), &message, &signed.sig).is_ok());
    }
}
