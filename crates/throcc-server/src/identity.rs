use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use throcc_proto::Fingerprint;

pub const KEY_FILE: &str = "server_key";

pub struct ServerIdentity {
    pub certificate: CertificateDer<'static>,
    pub private_key: PrivateKeyDer<'static>,
    pub fingerprint: Fingerprint,
}

pub fn load_or_create(data_dir: &Path) -> Result<ServerIdentity> {
    let key = load_or_create_key(data_dir)?;
    derive_identity(&key)
}

fn load_or_create_key(data_dir: &Path) -> Result<SigningKey> {
    let path = data_dir.join(KEY_FILE);

    match fs::read(&path) {
        Ok(bytes) => {
            let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                anyhow::anyhow!("{} is {} bytes, expected 32", path.display(), bytes.len())
            })?;
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(data_dir)
                .with_context(|| format!("creating data directory {}", data_dir.display()))?;
            let key = SigningKey::generate(&mut rand::rng());
            write_private_file(&path, &key.to_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(path = %path.display(), "generated a new server key");
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
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

fn derive_identity(key: &SigningKey) -> Result<ServerIdentity> {
    let pkcs8 = key.to_pkcs8_der().context("encoding the key as PKCS#8")?;
    let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(pkcs8.as_bytes()),
        &PKCS_ED25519,
    )
    .context("loading the key into rcgen")?;

    let mut params = CertificateParams::new(vec!["throcc".to_string()])
        .context("building certificate parameters")?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "throcc");
    params.distinguished_name = distinguished_name;

    let certificate = params
        .self_signed(&key_pair)
        .context("self-signing the certificate")?;
    let certificate_der = certificate.der().clone();

    let fingerprint = Fingerprint::from_cert_der(&certificate_der)
        .context("hashing the SPKI of the certificate")?;

    Ok(ServerIdentity {
        certificate: certificate_der,
        private_key: PrivateKeyDer::try_from(pkcs8.as_bytes().to_vec())
            .map_err(|e| anyhow::anyhow!("re-encoding the key for rustls: {e}"))?,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_then_reloads_the_same_identity() {
        let data_dir = TempDir::new().unwrap();
        let first = load_or_create(data_dir.path()).unwrap();
        let second = load_or_create(data_dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;

        let data_dir = TempDir::new().unwrap();
        load_or_create(data_dir.path()).unwrap();
        let mode = fs::metadata(data_dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "server_key must be 0600");
    }

    #[test]
    fn the_fingerprint_follows_the_key_not_the_certificate() {
        let data_dir = TempDir::new().unwrap();
        let key = load_or_create_key(data_dir.path()).unwrap();
        let first = derive_identity(&key).unwrap();
        let second = derive_identity(&key).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.certificate, second.certificate);

        let other = derive_identity(&SigningKey::generate(&mut rand::rng())).unwrap();
        assert_ne!(first.fingerprint, other.fingerprint);
    }

    #[test]
    fn refuses_a_key_file_of_the_wrong_size() {
        let data_dir = TempDir::new().unwrap();
        fs::write(data_dir.path().join(KEY_FILE), b"not a seed").unwrap();
        assert!(load_or_create(data_dir.path()).is_err());
    }
}
