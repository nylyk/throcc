use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519, PublicKeyData};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use throcc_proto::{Fingerprint, SERVER_NAME};

const KEY_FILE: &str = "server_key";

pub struct ServerIdentity {
    key: KeyPair,
    pub fingerprint: Fingerprint,
}

impl ServerIdentity {
    /// Loads `server_key` from the data directory, creating it at mode 0600 on first start.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(KEY_FILE);
        let key = match fs::read(&path) {
            Ok(der) => KeyPair::try_from(&PrivatePkcs8KeyDer::from(der))
                .with_context(|| format!("{} is not an Ed25519 PKCS#8 key", path.display()))?,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let key = KeyPair::generate_for(&PKCS_ED25519)?;
                write_private(&path, &key.serialize_der())
                    .with_context(|| format!("writing {}", path.display()))?;
                key
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let fingerprint = Fingerprint::of_spki(&key.subject_public_key_info());
        Ok(Self { key, fingerprint })
    }

    /// A fresh self-signed certificate for this key. It is never stored.
    pub fn certificate(&self) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let cert = CertificateParams::new(vec![SERVER_NAME.to_owned()])?.self_signed(&self.key)?;
        let key = PrivatePkcs8KeyDer::from(self.key.serialize_der()).into();
        Ok((cert.der().clone(), key))
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let first = ServerIdentity::load_or_create(dir.path()).unwrap();
        let second = ServerIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        ServerIdentity::load_or_create(dir.path()).unwrap();
        let mode = fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn new_key_changes_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let first = ServerIdentity::load_or_create(dir.path()).unwrap();
        fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let second = ServerIdentity::load_or_create(dir.path()).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
    }
}
