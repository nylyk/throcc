#[cfg(target_os = "android")]
#[path = "keystore/android.rs"]
mod platform;
#[cfg(not(target_os = "android"))]
#[path = "keystore/keychain.rs"]
mod platform;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use throcc_crypto::hpke::EncryptionKeyPair;
use throcc_crypto::identity::{self, IdentityKey};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::SignedEncryptionKey;
use tracing::warn;
use zeroize::Zeroizing;

use crate::files::{read_if_exists, replace_file};
use crate::known_servers::KnownServers;
use crate::unix_now;

const IDENTITY_FILE: &str = "identity_key";
const ENCRYPTION_KEY_FILE: &str = "encryption_key";
const ENCRYPTION_KEY_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How long before it expires the encryption key is replaced.
const ENCRYPTION_KEY_RENEW_BEFORE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How far in the future a key's creation time may lie before the key is replaced, as one
/// made under a clock running ahead would otherwise stay valid far too long.
const ENCRYPTION_KEY_CLOCK_SKEW: Duration = Duration::from_secs(24 * 60 * 60);

/// The keychain has no identity key and there is no file holding one, yet servers are known,
/// so the identity they know us by is gone, or hidden in a keychain that is locked or hasn't
/// started yet. A new one is started only once the user agrees, with [`Keystore::start_over`].
#[derive(Debug, thiserror::Error)]
#[error(
    "your identity is missing from this computer's keychain, so your servers won't know you; \
     if your keychain or password manager is locked or hasn't started yet, unlock or start it \
     and try again, restarting throcc if it still isn't found"
)]
pub struct IdentityMissing;

/// What [`Keystore::start_over`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartedOver {
    /// The keychain held the identity after all, so nothing was forgotten.
    Recovered,
    /// A new identity was started, and the known servers and the encryption key dropped.
    NewIdentity,
}

/// The install's identity key and encryption key, plus the pinned servers.
pub struct Keystore {
    dir: PathBuf,
    cache: PathBuf,
    identity: IdentityKey,
    encryption_key: EncryptionKeyPair,
    encryption_key_created: u64,
    pub known_servers: KnownServers,
}

impl Keystore {
    /// The pinned servers in the platform config directory, without touching the keychain.
    pub fn default_known_servers() -> Result<KnownServers> {
        KnownServers::load(&config_dir()?)
    }

    /// Opens the platform config directory, keeping the identity key in the OS keychain.
    /// Falls back to a 0600 file only on a platform without a keychain. An empty keychain
    /// takes the identity from that file when there is one. Neither starts a new identity
    /// while servers are known; an empty keychain then fails with [`IdentityMissing`].
    pub fn open_default() -> Result<Self> {
        Ok(Self::open_platform(WhenMissing::Refuse)?.0)
    }

    /// Opens the platform config directory like [`Self::open_default`]. Where the identity
    /// is still missing, it forgets every known server and the encryption key and starts a
    /// new identity. For after [`IdentityMissing`], once the user agreed.
    pub fn start_over() -> Result<(Self, StartedOver)> {
        let (store, new) = Self::open_platform(WhenMissing::StartOver)?;
        let started_over = if new {
            StartedOver::NewIdentity
        } else {
            StartedOver::Recovered
        };
        Ok((store, started_over))
    }

    /// The store, and whether its identity was started just now.
    fn open_platform(when_missing: WhenMissing) -> Result<(Self, bool)> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        let found = match keychain_identity(&dir, when_missing)
            .context("reading the identity key from the OS keychain")?
        {
            Some(found) => found,
            None => {
                warn!("no OS keychain; keeping the identity key in a file");
                Found::kept(fallback_identity(&dir)?)
            }
        };
        Ok((Self::assemble(dir, cache_dir()?, found.key)?, found.new))
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
        let (encryption_key, encryption_key_created) =
            load_encryption_key(&dir.join(ENCRYPTION_KEY_FILE))?;
        Ok(Self {
            dir,
            cache,
            identity,
            encryption_key,
            encryption_key_created,
            known_servers,
        })
    }

    pub fn identity(&self) -> &IdentityKey {
        &self.identity
    }

    pub fn encryption_key(&self) -> &EncryptionKeyPair {
        &self.encryption_key
    }

    /// A copy of the encryption key, for a key manager to hold.
    pub fn encryption_key_pair(&self) -> Result<EncryptionKeyPair> {
        Ok(EncryptionKeyPair::from_secret(
            &self.encryption_key.secret(),
        )?)
    }

    /// Replaces the encryption key when it expires within a week. True if it was replaced.
    pub fn renew_encryption_key_if_due(&mut self) -> Result<bool> {
        if !renewal_due(self.encryption_key_created, unix_now()?) {
            return Ok(false);
        }
        self.renew_encryption_key()?;
        Ok(true)
    }

    /// Replaces the encryption key, so nothing wrapped to the old one opens any more.
    pub fn renew_encryption_key(&mut self) -> Result<()> {
        (self.encryption_key, self.encryption_key_created) =
            new_encryption_key(&self.dir.join(ENCRYPTION_KEY_FILE), unix_now()?)?;
        Ok(())
    }

    /// The encryption key, signed for one server and user until it expires.
    pub fn signed_encryption_key(&self, server: &Fingerprint, user: UserId) -> SignedEncryptionKey {
        let key = self.encryption_key.public();
        let not_after = self.encryption_key_created + ENCRYPTION_KEY_LIFETIME.as_secs();
        let message = identity::encryption_key_message(&server.0, user, not_after, &key);
        SignedEncryptionKey {
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
    Ok(app_dirs()?.config)
}

/// Where throcc keeps what it can fetch again, such as pictures.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(app_dirs()?.cache)
}

#[derive(Clone)]
struct AppDirs {
    config: PathBuf,
    cache: PathBuf,
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn app_dirs() -> Result<AppDirs> {
    let dirs = directories::ProjectDirs::from("", "", "throcc")
        .context("no home directory to keep throcc's data in")?;
    Ok(AppDirs {
        config: dirs.config_dir().to_owned(),
        cache: dirs.cache_dir().to_owned(),
    })
}

#[cfg(any(target_os = "ios", target_os = "android"))]
static APP_DIRS: std::sync::OnceLock<AppDirs> = std::sync::OnceLock::new();

/// Sets where the app keeps its data and what it can fetch again. The first call wins.
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn set_app_dirs(config: PathBuf, cache: PathBuf) {
    let _ = APP_DIRS.set(AppDirs { config, cache });
}

#[cfg(any(target_os = "ios", target_os = "android"))]
fn app_dirs() -> Result<AppDirs> {
    APP_DIRS
        .get()
        .cloned()
        .context("the app hasn't set its directories")
}

fn renewal_due(created: u64, now: u64) -> bool {
    created > now + ENCRYPTION_KEY_CLOCK_SKEW.as_secs()
        || now.saturating_sub(created) + ENCRYPTION_KEY_RENEW_BEFORE.as_secs()
            >= ENCRYPTION_KEY_LIFETIME.as_secs()
}

/// Loads the encryption key, replacing it when it expires within a week.
fn load_encryption_key(path: &Path) -> Result<(EncryptionKeyPair, u64)> {
    let now = unix_now()?;
    if let Ok(bytes) = fs::read(path).map(Zeroizing::new)
        && let Some((secret, created)) = bytes.split_first_chunk::<32>()
        && let Ok(created) = <[u8; 8]>::try_from(created)
        && !renewal_due(u64::from_be_bytes(created), now)
    {
        return Ok((
            EncryptionKeyPair::from_secret(secret)?,
            u64::from_be_bytes(created),
        ));
    }
    new_encryption_key(path, now)
}

fn new_encryption_key(path: &Path, now: u64) -> Result<(EncryptionKeyPair, u64)> {
    let key = EncryptionKeyPair::generate();
    let mut bytes = Zeroizing::new(key.secret().to_vec());
    bytes.extend_from_slice(&now.to_be_bytes());
    replace_file(path, &bytes)?;
    Ok((key, now))
}

/// Whether an empty keychain may start a new identity while servers are known.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhenMissing {
    Refuse,
    /// The user agreed to start over: a new identity with a new encryption key, and no
    /// servers.
    StartOver,
}

/// An identity key, and whether it was started just now.
struct Found {
    key: IdentityKey,
    new: bool,
}

impl Found {
    fn kept(key: IdentityKey) -> Self {
        Self { key, new: false }
    }
}

/// Where the keychain keeps the identity key's seed.
trait SeedEntry {
    /// The seed, or `None` while the keychain holds none.
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn write(&self, seed: &[u8]) -> Result<()>;
}

/// The identity key in the OS keychain, put there when it has none. `None` on a platform
/// without a keychain.
fn keychain_identity(dir: &Path, when_missing: WhenMissing) -> Result<Option<Found>> {
    let Some(entry) = platform::identity_entry(dir)? else {
        return Ok(None);
    };
    identity_in(&entry, dir, when_missing).map(Some)
}

fn identity_in(keychain: &impl SeedEntry, dir: &Path, when_missing: WhenMissing) -> Result<Found> {
    match keychain.read()? {
        Some(seed) => Ok(Found::kept(seed_to_key(&seed)?)),
        None => first_keychain_identity(keychain, dir, when_missing),
    }
}

/// The identity to put in an empty keychain: the one a run without a keychain kept in
/// `dir`, or else a new one while no server is known or once the user chose to start over.
/// The file stays, so the identity also survives the keychain going away again.
fn first_keychain_identity(
    keychain: &impl SeedEntry,
    dir: &Path,
    when_missing: WhenMissing,
) -> Result<Found> {
    let found = match read_if_exists(&dir.join(IDENTITY_FILE))? {
        Some(bytes) => Found::kept(seed_to_key(&Zeroizing::new(bytes))?),
        None if when_missing == WhenMissing::Refuse && knows_servers(dir)? => {
            return Err(IdentityMissing.into());
        }
        None => Found {
            key: IdentityKey::generate(),
            new: true,
        },
    };
    // A keychain that read empty may only have been locked, or not yet the user's own, and
    // been unlocked or started at a prompt since. What it holds then is used, not
    // overwritten. The write itself searches once more and, on Secret Service, replaces what
    // that search finds, so an identity unlocked at exactly that prompt is still lost.
    if let Some(seed) = keychain.read()? {
        return Ok(Found::kept(seed_to_key(&seed)?));
    }
    if !(found.new && when_missing == WhenMissing::StartOver) {
        keychain.write(found.key.seed().as_slice())?;
        return Ok(found);
    }
    // Starting over drops what belonged to the old identity: the encryption key it signed,
    // which would tie the new identity to it, and the servers that knew it. Each step leaves
    // a state that is safe on its own: a renewed encryption key is routine, and the servers
    // go only once the new identity is in the keychain, so a refused or interrupted write
    // never leaves the old identity coming back to an empty server list.
    let mut known = KnownServers::load(dir)?;
    new_encryption_key(&dir.join(ENCRYPTION_KEY_FILE), unix_now()?)?;
    keychain.write(found.key.seed().as_slice())?;
    known.forget_all()?;
    Ok(found)
}

/// The identity key in `dir`. A missing one is created only while no server is known.
fn fallback_identity(dir: &Path) -> Result<IdentityKey> {
    if knows_servers(dir)? && !dir.join(IDENTITY_FILE).exists() {
        bail!(
            "the OS keychain holding your identity key can't be reached; unlock or start your \
             keyring (Secret Service), then restart throcc"
        );
    }
    file_identity(dir)
}

fn knows_servers(dir: &Path) -> Result<bool> {
    Ok(KnownServers::load(dir)?.iter().next().is_some())
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
    use std::cell::{Cell, RefCell};

    use super::*;

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

    /// A keychain entry in memory. `hidden` is a seed the first read misses, as a locked
    /// password manager's is until the user unlocks it at the next prompt.
    #[derive(Default)]
    struct FakeEntry {
        held: RefCell<Option<Vec<u8>>>,
        hidden: RefCell<Option<Vec<u8>>>,
        writes: Cell<usize>,
        refuses_writes: bool,
    }

    impl FakeEntry {
        fn hiding(seed: &[u8]) -> Self {
            let entry = Self::default();
            *entry.hidden.borrow_mut() = Some(seed.to_vec());
            entry
        }

        /// One whose writes fail, as when the user dismisses the keychain's prompt.
        fn refusing_writes() -> Self {
            Self {
                refuses_writes: true,
                ..Self::default()
            }
        }

        fn held(&self) -> Option<Vec<u8>> {
            self.held.borrow().clone()
        }
    }

    impl SeedEntry for FakeEntry {
        fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
            let held = self.held();
            if let Some(hidden) = self.hidden.take() {
                *self.held.borrow_mut() = Some(hidden);
            }
            Ok(held.map(Zeroizing::new))
        }

        fn write(&self, seed: &[u8]) -> Result<()> {
            self.writes.set(self.writes.get() + 1);
            if self.refuses_writes {
                bail!("the keychain refused the write");
            }
            *self.held.borrow_mut() = Some(seed.to_vec());
            Ok(())
        }
    }

    fn knowing_a_server(dir: &Path) -> KnownServers {
        let mut known = KnownServers::load(dir).unwrap();
        known
            .trust("voice.example.net:8476", Fingerprint([3; 32]))
            .unwrap();
        known
    }

    fn encryption_key_file(dir: &Path) -> Vec<u8> {
        fs::read(dir.join(ENCRYPTION_KEY_FILE)).unwrap()
    }

    #[test]
    fn an_empty_keychain_takes_the_identity_kept_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let kept = file_identity(dir.path()).unwrap();
        let entry = FakeEntry::default();
        let found = identity_in(&entry, dir.path(), WhenMissing::Refuse).unwrap();
        assert_eq!(found.key.public(), kept.public());
        assert!(!found.new);
        assert_eq!(entry.held().unwrap(), kept.seed().as_slice());
        assert_eq!(file_identity(dir.path()).unwrap().public(), kept.public());

        let empty = tempfile::tempdir().unwrap();
        let found = identity_in(&FakeEntry::default(), empty.path(), WhenMissing::Refuse).unwrap();
        assert_ne!(found.key.public(), kept.public());
        assert!(found.new);
        assert!(!empty.path().join(IDENTITY_FILE).exists());
    }

    #[test]
    fn an_empty_keychain_starts_no_identity_while_servers_are_known() {
        let dir = tempfile::tempdir().unwrap();
        let mut known = knowing_a_server(dir.path());
        let entry = FakeEntry::default();
        let Err(missing) = identity_in(&entry, dir.path(), WhenMissing::Refuse) else {
            panic!("started an identity while servers are known");
        };
        let missing = missing.context("reading the identity key from the OS keychain");
        assert!(crate::account::SignInError::Store(missing).identity_missing());
        assert_eq!(entry.writes.get(), 0);
        assert!(!dir.path().join(IDENTITY_FILE).exists());

        let kept = file_identity(dir.path()).unwrap();
        let found = identity_in(&FakeEntry::default(), dir.path(), WhenMissing::Refuse).unwrap();
        assert_eq!(found.key.public(), kept.public());

        fs::remove_file(dir.path().join(IDENTITY_FILE)).unwrap();
        known.forget_all().unwrap();
        let entry = FakeEntry::default();
        let found = identity_in(&entry, dir.path(), WhenMissing::Refuse).unwrap();
        assert_ne!(found.key.public(), kept.public());
        assert_eq!(entry.held().unwrap(), found.key.seed().as_slice());
    }

    #[test]
    fn starting_over_forgets_the_servers_and_the_encryption_key_only_for_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        knowing_a_server(dir.path());
        load_encryption_key(&dir.path().join(ENCRYPTION_KEY_FILE)).unwrap();
        let old_encryption_key = encryption_key_file(dir.path());

        let old = IdentityKey::generate();
        let entry = FakeEntry::default();
        entry.write(old.seed().as_slice()).unwrap();
        let found = identity_in(&entry, dir.path(), WhenMissing::StartOver).unwrap();
        assert_eq!(found.key.public(), old.public());
        assert!(!found.new);
        assert!(knows_servers(dir.path()).unwrap());
        assert_eq!(encryption_key_file(dir.path()), old_encryption_key);

        let entry = FakeEntry::default();
        let found = identity_in(&entry, dir.path(), WhenMissing::StartOver).unwrap();
        assert!(found.new);
        assert_ne!(found.key.public(), old.public());
        assert_eq!(entry.held().unwrap(), found.key.seed().as_slice());
        assert!(!knows_servers(dir.path()).unwrap());
        assert_ne!(encryption_key_file(dir.path()), old_encryption_key);
    }

    #[test]
    fn a_failed_write_while_starting_over_keeps_the_servers() {
        let dir = tempfile::tempdir().unwrap();
        knowing_a_server(dir.path());
        let entry = FakeEntry::refusing_writes();
        assert!(identity_in(&entry, dir.path(), WhenMissing::StartOver).is_err());
        assert_eq!(entry.writes.get(), 1);
        assert_eq!(entry.held(), None);
        assert!(knows_servers(dir.path()).unwrap());
    }

    #[test]
    fn starting_over_keeps_an_identity_kept_in_a_file_and_everything_with_it() {
        let dir = tempfile::tempdir().unwrap();
        knowing_a_server(dir.path());
        let kept = file_identity(dir.path()).unwrap();
        load_encryption_key(&dir.path().join(ENCRYPTION_KEY_FILE)).unwrap();
        let old_encryption_key = encryption_key_file(dir.path());

        let entry = FakeEntry::default();
        let found = identity_in(&entry, dir.path(), WhenMissing::StartOver).unwrap();
        assert_eq!(found.key.public(), kept.public());
        assert!(!found.new);
        assert_eq!(entry.held().unwrap(), kept.seed().as_slice());
        assert!(knows_servers(dir.path()).unwrap());
        assert_eq!(encryption_key_file(dir.path()), old_encryption_key);
        assert_eq!(file_identity(dir.path()).unwrap().public(), kept.public());
    }

    #[test]
    fn an_identity_that_shows_up_before_the_write_is_never_overwritten() {
        let old = IdentityKey::generate();
        for when_missing in [WhenMissing::Refuse, WhenMissing::StartOver] {
            let dir = tempfile::tempdir().unwrap();
            if when_missing == WhenMissing::StartOver {
                knowing_a_server(dir.path());
            }
            load_encryption_key(&dir.path().join(ENCRYPTION_KEY_FILE)).unwrap();
            let old_encryption_key = encryption_key_file(dir.path());
            let entry = FakeEntry::hiding(old.seed().as_slice());
            let found = identity_in(&entry, dir.path(), when_missing).unwrap();
            assert_eq!(found.key.public(), old.public());
            assert!(!found.new);
            assert_eq!(entry.writes.get(), 0);
            assert_eq!(entry.held().unwrap(), old.seed().as_slice());
            assert_eq!(
                knows_servers(dir.path()).unwrap(),
                when_missing == WhenMissing::StartOver
            );
            assert_eq!(encryption_key_file(dir.path()), old_encryption_key);
        }
    }

    #[test]
    fn a_corrupt_identity_file_is_never_replaced_in_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        replace_file(&dir.path().join(IDENTITY_FILE), b"short").unwrap();
        let entry = FakeEntry::default();
        assert!(identity_in(&entry, dir.path(), WhenMissing::StartOver).is_err());
        assert_eq!(entry.writes.get(), 0);
    }

    #[test]
    fn a_key_made_under_a_clock_running_ahead_is_renewed() {
        let now = unix_now().unwrap();
        assert!(!renewal_due(now + 60, now));
        assert!(renewal_due(now + 2 * 24 * 60 * 60, now));
        assert!(renewal_due(now - 24 * 24 * 60 * 60, now));
    }

    #[test]
    fn encryption_key_persists_and_is_signed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Keystore::open_in(dir.path()).unwrap();
        let server = Fingerprint([3; 32]);
        let signed = store.signed_encryption_key(&server, UserId(9));
        assert_eq!(
            Keystore::open_in(dir.path())
                .unwrap()
                .encryption_key()
                .public(),
            signed.key
        );
        let message =
            identity::encryption_key_message(&server.0, UserId(9), signed.not_after, &signed.key);
        assert!(identity::verify(&store.identity().public(), &message, &signed.sig).is_ok());
    }

    #[test]
    fn an_encryption_key_near_expiry_is_renewed_on_open_and_while_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Keystore::open_in(dir.path()).unwrap();
        let first = store.encryption_key().public();
        assert!(!store.renew_encryption_key_if_due().unwrap());
        assert_eq!(store.encryption_key().public(), first);

        let now = unix_now().unwrap();
        store.encryption_key_created = now - 24 * 24 * 60 * 60;
        assert!(store.renew_encryption_key_if_due().unwrap());
        let second = store.encryption_key().public();
        assert_ne!(second, first);
        let signed = store.signed_encryption_key(&Fingerprint([3; 32]), UserId(9));
        assert!(signed.not_after >= now + ENCRYPTION_KEY_LIFETIME.as_secs());
        assert_eq!(
            Keystore::open_in(dir.path())
                .unwrap()
                .encryption_key()
                .public(),
            second
        );

        let mut old = Zeroizing::new(EncryptionKeyPair::generate().secret().to_vec());
        old.extend_from_slice(&(now - 24 * 24 * 60 * 60).to_be_bytes());
        replace_file(&dir.path().join(ENCRYPTION_KEY_FILE), &old).unwrap();
        let reopened = Keystore::open_in(dir.path()).unwrap();
        assert!(!renewal_due(reopened.encryption_key_created, now));
    }
}
