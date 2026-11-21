use std::fmt::{self, Write};

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

use crate::identity::random;

/// Every sealed transfer starts with these bytes, which name the format and its version.
const MAGIC: &[u8; 16] = b"throcc/identity1";
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const HEADER_BYTES: usize = MAGIC.len() + SALT_BYTES + NONCE_BYTES;
/// How many bytes [`seal`] adds to its plaintext.
pub const OVERHEAD_BYTES: usize = HEADER_BYTES + TAG_BYTES;
const KDF_MEMORY_KIB: u32 = 128 * 1024;
const KDF_PASSES: u32 = 3;
const KDF_LANES: u32 = 1;
/// Crockford's base 32 digits.
const CODE_DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_CHARS: usize = 12;
const CODE_BITS: u32 = 5 * CODE_CHARS as u32;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    #[error("this isn't an identity from throcc")]
    NotATransfer,
    #[error("the code doesn't open this identity")]
    WrongCode,
}

/// The secret a transfer is sealed under: 60 random bits, written as 12 Crockford base 32
/// digits in groups of four.
#[derive(Clone, PartialEq, Eq)]
pub struct TransferCode(u64);

impl TransferCode {
    pub fn generate() -> Self {
        Self(u64::from_be_bytes(random()) >> (64 - CODE_BITS))
    }

    /// The code in `typed`, ignoring case, whitespace and dashes, and reading O as 0 and I or L
    /// as 1. None unless it is exactly 12 digits.
    pub fn parse(typed: &str) -> Option<Self> {
        let mut value = 0u64;
        let mut digits = 0;
        for c in typed.chars().filter(|c| !c.is_whitespace() && *c != '-') {
            let c = match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let digit = CODE_DIGITS.iter().position(|d| char::from(*d) == c)?;
            value = (value << 5) | digit as u64;
            digits += 1;
            if digits > CODE_CHARS {
                return None;
            }
        }
        (digits == CODE_CHARS).then_some(Self(value))
    }
}

impl fmt::Display for TransferCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in 0..CODE_CHARS {
            if i > 0 && i % 4 == 0 {
                f.write_str("-")?;
            }
            let shift = 5 * (CODE_CHARS - 1 - i);
            let digit = CODE_DIGITS[((self.0 >> shift) & 31) as usize];
            f.write_char(char::from(digit))?;
        }
        Ok(())
    }
}

impl fmt::Debug for TransferCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransferCode(..)")
    }
}

/// Seals `plaintext` under `code`: `MAGIC || salt || nonce || AES-256-GCM(plaintext) || tag`,
/// keyed by Argon2id over the code and salt, with the header as associated data.
pub fn seal(code: &TransferCode, plaintext: &[u8]) -> Vec<u8> {
    let salt: [u8; SALT_BYTES] = random();
    let nonce: [u8; NONCE_BYTES] = random();
    let mut sealed = Vec::with_capacity(plaintext.len() + OVERHEAD_BYTES);
    sealed.extend_from_slice(MAGIC);
    sealed.extend_from_slice(&salt);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(plaintext);
    let (header, body) = sealed.split_at_mut(HEADER_BYTES);
    let tag = cipher(code, &salt)
        .encrypt_inout_detached(&Nonce::from(nonce), header, body.into())
        .expect("AES-GCM seals any plaintext that fits in memory");
    sealed.extend_from_slice(&tag);
    sealed
}

/// Opens what [`seal`] made under `code`.
pub fn open(code: &TransferCode, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, TransferError> {
    if !looks_sealed(sealed) {
        return Err(TransferError::NotATransfer);
    }
    let (header, rest) = sealed.split_at(HEADER_BYTES);
    let (body, tag) = rest.split_at(rest.len() - TAG_BYTES);
    let salt: [u8; SALT_BYTES] = header[MAGIC.len()..][..SALT_BYTES].try_into().unwrap();
    let nonce: [u8; NONCE_BYTES] = header[MAGIC.len() + SALT_BYTES..].try_into().unwrap();
    let tag = Tag::try_from(tag).unwrap();
    let mut plaintext = Zeroizing::new(body.to_vec());
    cipher(code, &salt)
        .decrypt_inout_detached(
            &Nonce::from(nonce),
            header,
            plaintext.as_mut_slice().into(),
            &tag,
        )
        .map_err(|_| TransferError::WrongCode)?;
    Ok(plaintext)
}

/// Whether `bytes` start as a sealed transfer does, so a file can be told apart before
/// anyone types a code.
pub fn looks_sealed(bytes: &[u8]) -> bool {
    bytes.len() >= HEADER_BYTES + TAG_BYTES && bytes.starts_with(MAGIC)
}

fn cipher(code: &TransferCode, salt: &[u8; SALT_BYTES]) -> Aes256Gcm {
    let params = Params::new(KDF_MEMORY_KIB, KDF_PASSES, KDF_LANES, Some(32))
        .expect("the KDF parameters are valid");
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(&code.0.to_be_bytes(), salt, key.as_mut())
        .expect("Argon2 takes any password with a 16-byte salt");
    Aes256Gcm::new_from_slice(key.as_ref()).expect("AES-256 takes a 32-byte key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_transfer_opens_only_under_its_code() {
        let code = TransferCode::generate();
        let sealed = seal(&code, b"identity");
        assert!(looks_sealed(&sealed));
        assert_eq!(open(&code, &sealed).unwrap().as_slice(), b"identity");

        let other = TransferCode::parse("0000-0000-0001").unwrap();
        assert_eq!(open(&other, &sealed), Err(TransferError::WrongCode));
        for at in [
            MAGIC.len(),
            HEADER_BYTES - 1,
            HEADER_BYTES,
            sealed.len() - 1,
        ] {
            let mut tampered = sealed.clone();
            tampered[at] ^= 1;
            assert_eq!(
                open(&code, &tampered),
                Err(TransferError::WrongCode),
                "{at}"
            );
        }
        assert_eq!(
            open(&code, &sealed[..HEADER_BYTES + TAG_BYTES - 1]),
            Err(TransferError::NotATransfer)
        );
        assert_eq!(
            open(&code, b"something else entirely, quite long"),
            Err(TransferError::NotATransfer)
        );
    }

    #[test]
    fn every_seal_is_new() {
        let code = TransferCode::generate();
        assert_ne!(seal(&code, b"identity"), seal(&code, b"identity"));
    }

    #[test]
    fn a_seal_from_an_independent_aes_gcm_opens() {
        use ring::aead;
        let code = TransferCode::parse("7K3M-Q9XZ-AB12").unwrap();
        let salt = [5; SALT_BYTES];
        let nonce = [9; NONCE_BYTES];
        let header = [MAGIC.as_slice(), &salt, &nonce].concat();
        let params = Params::new(KDF_MEMORY_KIB, KDF_PASSES, KDF_LANES, Some(32)).unwrap();
        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(&code.0.to_be_bytes(), &salt, &mut key)
            .unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &key).unwrap());
        let mut body = b"identity".to_vec();
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&header),
            &mut body,
        )
        .unwrap();
        let sealed = [header.as_slice(), &body].concat();
        assert_eq!(open(&code, &sealed).unwrap().as_slice(), b"identity");
    }

    #[test]
    fn a_code_reads_back_however_it_is_typed() {
        for _ in 0..100 {
            let code = TransferCode::generate();
            let shown = code.to_string();
            assert_eq!(shown.len(), CODE_CHARS + 2);
            assert_eq!(TransferCode::parse(&shown), Some(code.clone()));
            let typed = shown.to_lowercase().replace('-', " ");
            assert_eq!(TransferCode::parse(&typed), Some(code));
        }
        assert_eq!(
            TransferCode::parse("o1lI-0000-0000"),
            TransferCode::parse("0111-0000-0000")
        );
        assert_eq!(
            TransferCode::parse("\t0000\u{a0}0000-0000\n"),
            TransferCode::parse("0000-0000-0000")
        );
        assert_eq!(TransferCode::parse("0000-0000-000"), None);
        assert_eq!(TransferCode::parse("0000-0000-00000"), None);
        assert_eq!(TransferCode::parse("0000-0000-000U"), None);
        assert_eq!(
            TransferCode::parse("ZZZZ-ZZZZ-ZZZZ").unwrap().0,
            (1 << 60) - 1
        );
    }
}
