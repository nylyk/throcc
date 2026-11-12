use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use throcc_proto::ids::UserId;
use throcc_proto::messages::{PublicKey, Signature, SignedEncryptionKey};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad signature")]
pub struct BadSignature;

/// An Ed25519 identity key.
pub struct IdentityKey(SigningKey);

impl IdentityKey {
    pub fn generate() -> Self {
        let seed = Zeroizing::new(random());
        Self(SigningKey::from_bytes(&seed))
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
pub fn encryption_key_message(
    server: &[u8; 32],
    user: UserId,
    not_after: u64,
    encryption_key: &PublicKey,
) -> Vec<u8> {
    [
        b"throcc/enc-key/1".as_slice(),
        server,
        &user.0.to_be_bytes(),
        &not_after.to_be_bytes(),
        encryption_key,
    ]
    .concat()
}

/// Checks that `identity_key` signed `signed` for `user` on the server with the SPKI hash
/// `server`.
pub fn verify_encryption_key(
    identity_key: &PublicKey,
    server: &[u8; 32],
    user: UserId,
    signed: &SignedEncryptionKey,
) -> Result<(), BadSignature> {
    let message = encryption_key_message(server, user, signed.not_after, &signed.key);
    verify(identity_key, &message, &signed.sig)
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
    use crate::hex;

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
    fn auth_message_and_its_signature_are_pinned() {
        let message = auth_message(&[1; 32], &[2; 32], &[3; 32]);
        assert_eq!(
            hex(&message),
            concat!(
                "7468726f63632f617574682f31",
                "0101010101010101010101010101010101010101010101010101010101010101",
                "0202020202020202020202020202020202020202020202020202020202020202",
                "0303030303030303030303030303030303030303030303030303030303030303",
            )
        );
        assert_eq!(
            hex(&IdentityKey::from_seed(&[4; 32]).sign(&message)),
            concat!(
                "20cf9e1910d859fabedd3b31ee3cfbf430e2e485c456c2f119a0794f04614010",
                "c7fbab7ff0174574efe9e9d2bce68d02b9a3494209538356a9996de007a4e900",
            )
        );
    }

    #[test]
    fn an_encryption_key_proves_its_identity_only_for_its_server_and_user() {
        let identity = IdentityKey::from_seed(&[3; 32]);
        let (server, user, not_after, key) = ([1; 32], UserId(7), 99, [2; 32]);
        let signed = SignedEncryptionKey {
            key,
            not_after,
            sig: identity.sign(&encryption_key_message(&server, user, not_after, &key)),
        };
        let public = identity.public();
        assert_eq!(
            verify_encryption_key(&public, &server, user, &signed),
            Ok(())
        );
        let other = IdentityKey::from_seed(&[4; 32]).public();
        let refused = Err(BadSignature);
        assert_eq!(
            verify_encryption_key(&other, &server, user, &signed),
            refused
        );
        assert_eq!(
            verify_encryption_key(&public, &server, UserId(8), &signed),
            refused
        );
        assert_eq!(
            verify_encryption_key(&public, &[9; 32], user, &signed),
            refused
        );
        let later = SignedEncryptionKey {
            not_after: 100,
            ..signed
        };
        assert_eq!(
            verify_encryption_key(&public, &server, user, &later),
            refused
        );
    }

    /// The signed encryption key is part of the protocol: peers and the server verify these exact bytes.
    #[test]
    fn encryption_key_message_and_its_signature_are_pinned() {
        let message = encryption_key_message(
            &[1; 32],
            UserId(0x0102_0304_0506_0708),
            0x1122_3344_5566_7788,
            &[2; 32],
        );
        assert_eq!(
            hex(&message),
            concat!(
                "7468726f63632f656e632d6b65792f31",
                "0101010101010101010101010101010101010101010101010101010101010101",
                "0102030405060708",
                "1122334455667788",
                "0202020202020202020202020202020202020202020202020202020202020202",
            )
        );
        let key = IdentityKey::from_seed(&[3; 32]);
        assert_eq!(
            hex(&key.public()),
            "ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1"
        );
        assert_eq!(
            hex(&key.sign(&message)),
            concat!(
                "ece8743eeedded9e657f1755b8e2e18f29428939a768067d5076f2430120a655",
                "85f34284689dd7132236b949b5a7c2d8325322af79d91e7ffbd1e87e3da9e006",
            )
        );
    }

    #[test]
    fn seed_round_trips() {
        let key = IdentityKey::generate();
        assert_eq!(IdentityKey::from_seed(&key.seed()).public(), key.public());
    }
}
