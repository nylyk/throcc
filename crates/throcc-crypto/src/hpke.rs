use hpke::aead::AesGcm128;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, RoomId, UserId};
use throcc_proto::messages::PublicKey;
use zeroize::Zeroizing;

type Private = <X25519HkdfSha256 as Kem>::PrivateKey;
type Public = <X25519HkdfSha256 as Kem>::PublicKey;
type Encapped = <X25519HkdfSha256 as Kem>::EncappedKey;

const ENCAPPED_BYTES: usize = 32;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HpkeError {
    #[error("not an X25519 key")]
    BadKey,
    #[error("the wrapped sender key didn't open")]
    Open,
    #[error("wrapping failed")]
    Seal,
}

/// An X25519 encryption key pair, signed by the identity key and rotated every 30 days.
pub struct EncKeyPair {
    private: Private,
    public: Public,
}

impl EncKeyPair {
    pub fn generate() -> Self {
        let (private, public) = X25519HkdfSha256::gen_keypair();
        Self { private, public }
    }

    pub fn from_secret(secret: &[u8; 32]) -> Result<Self, HpkeError> {
        let private = Private::from_bytes(secret).map_err(|_| HpkeError::BadKey)?;
        let public = X25519HkdfSha256::sk_to_pk(&private);
        Ok(Self { private, public })
    }

    pub fn secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.private.to_bytes().into())
    }

    pub fn public(&self) -> PublicKey {
        self.public.to_bytes().into()
    }

    fn keypair(&self) -> (Private, Public) {
        (self.private.clone(), self.public.clone())
    }
}

/// Binds a wrapped sender secret to one room generation, sender and recipient.
pub struct WrapContext {
    pub server: Fingerprint,
    pub room: RoomId,
    pub epoch: Epoch,
    pub from: UserId,
    pub to: UserId,
}

impl WrapContext {
    fn info(&self) -> Vec<u8> {
        let mut info = b"throcc/sender-key/1".to_vec();
        info.extend_from_slice(&self.server.0);
        info.extend_from_slice(&self.room.0.to_be_bytes());
        info.extend_from_slice(&self.epoch.0.to_be_bytes());
        info.extend_from_slice(&self.from.0.to_be_bytes());
        info.extend_from_slice(&self.to.0.to_be_bytes());
        info
    }
}

/// Wraps `secret` to `recipient` in HPKE auth mode, which proves the sender's key.
pub fn wrap(
    sender: &EncKeyPair,
    recipient: &PublicKey,
    context: &WrapContext,
    secret: &[u8; 32],
) -> Result<Vec<u8>, HpkeError> {
    let recipient = Public::from_bytes(recipient).map_err(|_| HpkeError::BadKey)?;
    let mode = OpModeS::Auth(sender.keypair());
    let (encapped, ciphertext) = hpke::single_shot_seal::<AesGcm128, HkdfSha256, X25519HkdfSha256>(
        &mode,
        &recipient,
        &context.info(),
        secret,
        b"",
    )
    .map_err(|_| HpkeError::Seal)?;
    let mut out = encapped.to_bytes().to_vec();
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn unwrap(
    recipient: &EncKeyPair,
    sender: &PublicKey,
    context: &WrapContext,
    wrapped: &[u8],
) -> Result<Zeroizing<[u8; 32]>, HpkeError> {
    let sender = Public::from_bytes(sender).map_err(|_| HpkeError::BadKey)?;
    let (encapped, ciphertext) = wrapped
        .split_at_checked(ENCAPPED_BYTES)
        .ok_or(HpkeError::Open)?;
    let encapped = Encapped::from_bytes(encapped).map_err(|_| HpkeError::Open)?;
    let plaintext = Zeroizing::new(
        hpke::single_shot_open::<AesGcm128, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Auth(sender),
            &recipient.private,
            &encapped,
            &context.info(),
            ciphertext,
            b"",
        )
        .map_err(|_| HpkeError::Open)?,
    );
    let mut secret = Zeroizing::new([0u8; 32]);
    if plaintext.len() != secret.len() {
        return Err(HpkeError::Open);
    }
    secret.copy_from_slice(&plaintext);
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(epoch: u32) -> WrapContext {
        WrapContext {
            server: Fingerprint([8; 32]),
            room: RoomId(1),
            epoch: Epoch(epoch),
            from: UserId(2),
            to: UserId(3),
        }
    }

    #[test]
    fn wrap_round_trips() {
        let alice = EncKeyPair::generate();
        let bob = EncKeyPair::generate();
        let wrapped = wrap(&alice, &bob.public(), &context(5), &[9; 32]).unwrap();
        let secret = unwrap(&bob, &alice.public(), &context(5), &wrapped).unwrap();
        assert_eq!(*secret, [9; 32]);
    }

    #[test]
    fn unwrap_checks_sender_and_context() {
        let alice = EncKeyPair::generate();
        let bob = EncKeyPair::generate();
        let mallory = EncKeyPair::generate();
        let wrapped = wrap(&alice, &bob.public(), &context(5), &[9; 32]).unwrap();
        assert_eq!(
            unwrap(&bob, &mallory.public(), &context(5), &wrapped).err(),
            Some(HpkeError::Open)
        );
        assert_eq!(
            unwrap(&bob, &alice.public(), &context(6), &wrapped).err(),
            Some(HpkeError::Open)
        );
        assert_eq!(
            unwrap(&mallory, &alice.public(), &context(5), &wrapped).err(),
            Some(HpkeError::Open)
        );
        assert_eq!(
            unwrap(&bob, &alice.public(), &context(5), &wrapped[..10]).err(),
            Some(HpkeError::Open)
        );
    }

    #[test]
    fn secret_round_trips() {
        let key = EncKeyPair::generate();
        assert_eq!(
            EncKeyPair::from_secret(&key.secret()).unwrap().public(),
            key.public()
        );
    }

    #[test]
    fn a_pinned_wrap_still_opens() {
        let alice = EncKeyPair::from_secret(&[0x11; 32]).unwrap();
        let bob = EncKeyPair::from_secret(&[0x22; 32]).unwrap();
        let hex = "80ac5e1aa2cda2bed51f33b7446b353d4cb0b6bd8d7537da971b4b67d992b713\
                   fcab8196c40683d7d08362c585979811dfc9290a0f8a71aa6fe9b22301683036\
                   10149bb547bfdf9146ef5540a2ec4490";
        let wrapped: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let secret = unwrap(&bob, &alice.public(), &context(5), &wrapped).unwrap();
        assert_eq!(*secret, [9; 32]);
    }
}
