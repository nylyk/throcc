use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use throcc_proto::ids::UserId;
use throcc_proto::messages::{PublicKey, Signature};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad signature")]
pub struct BadSignature;

/// An Ed25519 identity key.
pub struct IdentityKey(SigningKey);

impl IdentityKey {
    pub fn generate() -> Self {
        Self(SigningKey::from_bytes(&random()))
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn public(&self) -> PublicKey {
        self.0.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.0.sign(message).to_bytes()
    }
}

pub fn verify(
    public: &PublicKey,
    message: &[u8],
    signature: &Signature,
) -> Result<(), BadSignature> {
    let key = VerifyingKey::from_bytes(public).map_err(|_| BadSignature)?;
    key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
        .map_err(|_| BadSignature)
}

/// What a returning client signs to log in.
pub fn auth_message(
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    exporter: &[u8; 32],
) -> Vec<u8> {
    [
        b"throcc/auth/1".as_slice(),
        server_nonce,
        client_nonce,
        exporter,
    ]
    .concat()
}

/// What an identity key signs to vouch for an X25519 encryption key: bound to one server
/// (by its SPKI hash), one user and an expiry.
pub fn enc_key_message(
    server: &[u8; 32],
    user: UserId,
    not_after: u64,
    enc_key: &PublicKey,
) -> Vec<u8> {
    [
        b"throcc/enc-key/1".as_slice(),
        server,
        &user.0.to_be_bytes(),
        &not_after.to_be_bytes(),
        enc_key,
    ]
    .concat()
}

/// Bytes from the operating system's CSPRNG.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the OS random number generator failed");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_verify_and_bind_the_message() {
        let key = IdentityKey::generate();
        let message = auth_message(&[1; 32], &[2; 32], &[3; 32]);
        let signature = key.sign(&message);
        assert_eq!(verify(&key.public(), &message, &signature), Ok(()));
        let other = auth_message(&[1; 32], &[2; 32], &[4; 32]);
        assert_eq!(verify(&key.public(), &other, &signature), Err(BadSignature));
    }

    #[test]
    fn seed_round_trips() {
        let key = IdentityKey::generate();
        assert_eq!(IdentityKey::from_seed(&key.seed()).public(), key.public());
    }
}
