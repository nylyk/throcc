use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use throcc_crypto::hpke::EncKeyPair;
use throcc_crypto::identity::{self, IdentityKey};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::SignedEncKey;
use tracing::warn;
use zeroize::Zeroizing;

use crate::files::{read_if_exists, replace_file};
use crate::known_servers::KnownServers;
use crate::unix_now;

const KEYCHAIN_SERVICE: &str = "throcc";
const KEYCHAIN_IDENTITY: &str = "identity";
const IDENTITY_FILE: &str = "identity_key";
const ENC_KEY_FILE: &str = "enc_key";
const ENC_KEY_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How long before it expires the encryption key is replaced.
const ENC_KEY_RENEW_BEFORE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How far in the future a key's creation time may lie before the key is replaced, as one
/// made under a clock running ahead would otherwise stay valid far too long.
const ENC_KEY_CLOCK_SKEW: Duration = Duration::from_secs(24 * 60 * 60);

/// The install's identity key and encryption key, plus the pinned servers.
pub struct Keystore {
    dir: PathBuf,
    cache: PathBuf,
    identity: IdentityKey,
    enc_key: EncKeyPair,
    enc_key_created: u64,
    pub known_servers: KnownServers,
}

impl Keystore {
    /// The pinned servers in the platform config directory, without touching the keychain.
    pub fn default_known_servers() -> Result<KnownServers> {
        KnownServers::load(&config_dir()?)
    }

    /// Opens the platform config directory, keeping the identity key in the OS keychain.
    /// Falls back to a 0600 file only on a platform without a keychain, and refuses to
    /// start a new identity there while servers are known. An empty keychain takes the
    /// identity from that file when there is one.
    pub fn open_default() -> Result<Self> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        let identity = match keychain_identity(&dir) {
            Err(e) if matches!(e.downcast_ref(), Some(keyring::Error::NoDefaultStore)) => {
                warn!("no OS keychain ({e:#}); keeping the identity key in a file");
                fallback_identity(&dir)?
            }
            other => other.context("reading the identity key from the OS keychain")?,
        };
        Self::assemble(dir, cache_dir()?, identity)
    }

    /// Opens `dir` with the identity key in a 0600 file and the cache inside it. Used for
    /// profiles and tests.
    pub fn open_in(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let identity = file_identity(dir)?;
        Self::assemble(dir.to_owned(), dir.to_owned(), identity)
    }

    fn assemble(dir: PathBuf, cache: PathBuf, identity: IdentityKey) -> Result<Self> {
        let known_servers = KnownServers::load(&dir)?;
        let (enc_key, enc_key_created) = load_enc_key(&dir.join(ENC_KEY_FILE))?;
        Ok(Self {
            dir,
            cache,
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

    /// A copy of the encryption key, for a key manager to hold.
    pub fn enc_key_pair(&self) -> Result<EncKeyPair> {
        Ok(EncKeyPair::from_secret(&self.enc_key.secret())?)
    }

    /// Replaces the encryption key when it expires within a week. True if it was replaced.
    pub fn renew_enc_key_if_due(&mut self) -> Result<bool> {
        let now = unix_now()?;
        if !renewal_due(self.enc_key_created, now) {
            return Ok(false);
        }
        (self.enc_key, self.enc_key_created) = new_enc_key(&self.dir.join(ENC_KEY_FILE), now)?;
        Ok(true)
    }

    /// The encryption key, signed for one server and user until it expires.
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

    /// Where the keys, pins and settings are kept.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }
}

/// Where throcc keeps its keys, pins and settings on this machine.
pub fn config_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_owned())
}

/// Where throcc keeps what it can fetch again, such as pictures.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_owned())
}

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "throcc")
        .context("no home directory to keep throcc's data in")
}

fn renewal_due(created: u64, now: u64) -> bool {
    created > now + ENC_KEY_CLOCK_SKEW.as_secs()
        || now.saturating_sub(created) + ENC_KEY_RENEW_BEFORE.as_secs()
            >= ENC_KEY_LIFETIME.as_secs()
}

/// Loads the encryption key, replacing it when it expires within a week.
fn load_enc_key(path: &Path) -> Result<(EncKeyPair, u64)> {
    let now = unix_now()?;
    if let Ok(bytes) = fs::read(path).map(Zeroizing::new)
        && let Some((secret, created)) = bytes.split_first_chunk::<32>()
        && let Ok(created) = <[u8; 8]>::try_from(created)
        && !renewal_due(u64::from_be_bytes(created), now)
    {
        return Ok((
            EncKeyPair::from_secret(secret)?,
            u64::from_be_bytes(created),
        ));
    }
    new_enc_key(path, now)
}

fn new_enc_key(path: &Path, now: u64) -> Result<(EncKeyPair, u64)> {
    let key = EncKeyPair::generate();
    let mut bytes = Zeroizing::new(key.secret().to_vec());
    bytes.extend_from_slice(&now.to_be_bytes());
    replace_file(path, &bytes)?;
    Ok((key, now))
}

/// The identity key in the OS keychain, put there when it has none.
fn keychain_identity(dir: &Path) -> Result<IdentityKey> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_IDENTITY)?;
    match entry.get_secret() {
        Ok(secret) => seed_to_key(&Zeroizing::new(secret)),
        Err(keyring::Error::NoEntry) => {
            first_keychain_identity(dir, |seed| Ok(entry.set_secret(seed)?))
        }
        Err(e) => Err(e.into()),
    }
}

/// The identity to `store` in an empty keychain: the one a run without a keychain kept in
/// `dir`, or else a new one. The file stays, so the identity also survives the keychain
/// going away again.
fn first_keychain_identity(
    dir: &Path,
    store: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<IdentityKey> {
    let key = match read_if_exists(&dir.join(IDENTITY_FILE))? {
        Some(bytes) => seed_to_key(&Zeroizing::new(bytes))?,
        None => IdentityKey::generate(),
    };
    store(key.seed().as_slice())?;
    Ok(key)
}

/// The identity key in `dir`. A missing one is created only while no server is known.
fn fallback_identity(dir: &Path) -> Result<IdentityKey> {
    let known_servers = KnownServers::load(dir)?;
    if known_servers.iter().next().is_some() && !dir.join(IDENTITY_FILE).exists() {
        bail!(
            "the OS keychain holding your identity key can't be reached; unlock or start your \
             keyring (Secret Service) and try again"
        );
    }
    file_identity(dir)
}

fn file_identity(dir: &Path) -> Result<IdentityKey> {
    let path = dir.join(IDENTITY_FILE);
    match read_if_exists(&path)? {
        Some(bytes) => seed_to_key(&Zeroizing::new(bytes)),
        None => {
            let key = IdentityKey::generate();
            replace_file(&path, key.seed().as_slice())?;
            Ok(key)
        }
    }
}

fn seed_to_key(bytes: &[u8]) -> Result<IdentityKey> {
    let Ok(seed) = <&[u8; 32]>::try_from(bytes) else {
        bail!("the stored identity key is corrupt");
    };
    Ok(IdentityKey::from_seed(seed))
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
    fn the_fallback_starts_an_identity_only_while_no_server_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let first = fallback_identity(dir.path()).unwrap().public();
        assert_eq!(fallback_identity(dir.path()).unwrap().public(), first);

        let mut store = Keystore::open_in(dir.path()).unwrap();
        store
            .known_servers
            .trust("voice.example.net:8476", Fingerprint([3; 32]))
            .unwrap();
        fs::remove_file(dir.path().join(IDENTITY_FILE)).unwrap();
        assert!(fallback_identity(dir.path()).is_err());
        assert!(!dir.path().join(IDENTITY_FILE).exists());
    }

    #[test]
    fn an_empty_keychain_takes_the_identity_kept_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let kept = file_identity(dir.path()).unwrap();
        let mut stored = Vec::new();
        let key = first_keychain_identity(dir.path(), |seed| {
            stored = seed.to_vec();
            Ok(())
        })
        .unwrap();
        assert_eq!(key.public(), kept.public());
        assert_eq!(stored, kept.seed().as_slice());
        assert_eq!(file_identity(dir.path()).unwrap().public(), kept.public());

        let empty = tempfile::tempdir().unwrap();
        let new = first_keychain_identity(empty.path(), |_| Ok(())).unwrap();
        assert_ne!(new.public(), kept.public());
        assert!(!empty.path().join(IDENTITY_FILE).exists());
    }

    #[test]
    fn a_corrupt_identity_file_is_never_replaced_in_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        replace_file(&dir.path().join(IDENTITY_FILE), b"short").unwrap();
        let mut stored = false;
        let opened = first_keychain_identity(dir.path(), |_| {
            stored = true;
            Ok(())
        });
        assert!(opened.is_err());
        assert!(!stored);
    }

    #[test]
    fn a_key_made_under_a_clock_running_ahead_is_renewed() {
        let now = unix_now().unwrap();
        assert!(!renewal_due(now + 60, now));
        assert!(renewal_due(now + 2 * 24 * 60 * 60, now));
        assert!(renewal_due(now - 24 * 24 * 60 * 60, now));
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

    #[test]
    fn an_enc_key_near_expiry_is_renewed_on_open_and_while_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Keystore::open_in(dir.path()).unwrap();
        let first = store.enc_key().public();
        assert!(!store.renew_enc_key_if_due().unwrap());
        assert_eq!(store.enc_key().public(), first);

        let now = unix_now().unwrap();
        store.enc_key_created = now - 24 * 24 * 60 * 60;
        assert!(store.renew_enc_key_if_due().unwrap());
        let second = store.enc_key().public();
        assert_ne!(second, first);
        let signed = store.signed_enc_key(&Fingerprint([3; 32]), UserId(9));
        assert!(signed.not_after >= now + ENC_KEY_LIFETIME.as_secs());
        assert_eq!(
            Keystore::open_in(dir.path()).unwrap().enc_key().public(),
            second
        );

        let mut old = Zeroizing::new(EncKeyPair::generate().secret().to_vec());
        old.extend_from_slice(&(now - 24 * 24 * 60 * 60).to_be_bytes());
        replace_file(&dir.path().join(ENC_KEY_FILE), &old).unwrap();
        let reopened = Keystore::open_in(dir.path()).unwrap();
        assert!(!renewal_due(reopened.enc_key_created, now));
    }
}
