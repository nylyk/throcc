use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;

use crate::{Error, Result};

#[derive(Serialize, Deserialize)]
struct KeystoreFile {
    /// The Ed25519 seed, hex.
    identity: String,
    /// authority → pinned SPKI hash.
    #[serde(default)]
    known_servers: BTreeMap<String, Fingerprint>,
}

pub struct Keystore {
    path: PathBuf,
    identity: SigningKey,
    known_servers: BTreeMap<String, Fingerprint>,
}

impl Keystore {
    /// `None` selects the default location. Generates an identity on first launch.
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let path = match path {
            Some(path) => path,
            None => default_path()?,
        };

        match fs::read_to_string(&path) {
            Ok(text) => {
                let stored: KeystoreFile = serde_json::from_str(&text)
                    .map_err(|e| Error::Keystore(format!("{}: {e}", path.display())))?;
                let seed = decode_seed(&stored.identity)?;
                Ok(Self {
                    path,
                    identity: SigningKey::from_bytes(&seed),
                    known_servers: stored.known_servers,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let keystore = Self {
                    path,
                    identity: SigningKey::generate(&mut rand::rng()),
                    known_servers: BTreeMap::new(),
                };
                keystore.save()?;
                tracing::info!(path = %keystore.path.display(), "generated a new client identity");
                Ok(keystore)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> &SigningKey {
        &self.identity
    }

    pub fn pinned(&self, server: &str) -> Option<Fingerprint> {
        self.known_servers.get(server).copied()
    }

    pub fn pin(&mut self, server: &str, fingerprint: Fingerprint) -> Result<()> {
        self.known_servers.insert(server.to_string(), fingerprint);
        self.save()
    }

    /// Writes to a temporary file and renames, so an interrupted write cannot
    /// truncate the identity key away.
    fn save(&self) -> Result<()> {
        let stored = KeystoreFile {
            identity: hex(&self.identity.to_bytes()),
            known_servers: self.known_servers.clone(),
        };
        let json = serde_json::to_string_pretty(&stored)
            .map_err(|e| Error::Keystore(format!("serializing: {e}")))?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary_path = self.path.with_extension("tmp");
        match fs::remove_file(&temporary_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
        write_private_file(&temporary_path, json.as_bytes())?;
        fs::rename(&temporary_path, &self.path)?;
        Ok(())
    }
}

fn default_path() -> Result<PathBuf> {
    let project_dirs = directories::ProjectDirs::from("", "", "throcc")
        .ok_or_else(|| Error::Keystore("no home directory to store the keystore in".into()))?;
    Ok(project_dirs.config_dir().join("keystore.json"))
}

/// Permissions are set *at creation*, so there is no world-readable window.
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_seed(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 {
        return Err(Error::Keystore("identity key is not 32 bytes".into()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| Error::Keystore("identity key is not hexadecimal".into()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generates_once_and_reloads() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keystore.json");

        let first = Keystore::open(Some(path.clone())).unwrap();
        let second = Keystore::open(Some(path)).unwrap();
        assert_eq!(
            first.identity().to_bytes(),
            second.identity().to_bytes(),
            "the identity key must survive a reload"
        );
    }

    #[test]
    fn pins_persist() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keystore.json");
        let fingerprint = Fingerprint::from([7u8; 32]);

        let mut keystore = Keystore::open(Some(path.clone())).unwrap();
        assert_eq!(keystore.pinned("example:8476"), None);
        keystore.pin("example:8476", fingerprint).unwrap();

        let reloaded = Keystore::open(Some(path)).unwrap();
        assert_eq!(reloaded.pinned("example:8476"), Some(fingerprint));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keystore.json");
        Keystore::open(Some(path.clone())).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
