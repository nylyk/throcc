use core::fmt;
use core::str::FromStr;

use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// SHA-256 over the DER encoding of a certificate's SubjectPublicKeyInfo.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn from_cert_der(der: &[u8]) -> Result<Self> {
        Ok(Self::from_spki_der(spki_of_cert(der)?))
    }

    pub fn from_spki_der(spki: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(spki);
        Self(hasher.finalize().into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for Fingerprint {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Fingerprint {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.len() != 64 {
            return Err(Error::Fingerprint("expected 64 hex characters"));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            let high = hex_nibble(s.as_bytes()[i * 2])?;
            let low = hex_nibble(s.as_bytes()[i * 2 + 1])?;
            *byte = (high << 4) | low;
        }
        Ok(Self(out))
    }
}

fn hex_nibble(character: u8) -> Result<u8> {
    match character {
        b'0'..=b'9' => Ok(character - b'0'),
        b'a'..=b'f' => Ok(character - b'a' + 10),
        b'A'..=b'F' => Ok(character - b'A' + 10),
        _ => Err(Error::Fingerprint("not hexadecimal")),
    }
}

impl serde::Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> core::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> core::result::Result<Self, D::Error> {
        let s = <&str as serde::Deserialize>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

const TAG_INTEGER: u8 = 0x02;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_VERSION: u8 = 0xa0;

fn spki_of_cert(der: &[u8]) -> Result<&[u8]> {
    let certificate = read_element(der, TAG_SEQUENCE)?.content;
    let mut to_be_signed = read_element(certificate, TAG_SEQUENCE)?.content;

    // An absent version tag means v1.
    if to_be_signed.first() == Some(&TAG_VERSION) {
        to_be_signed = skip_element(to_be_signed, TAG_VERSION)?;
    }
    to_be_signed = skip_element(to_be_signed, TAG_INTEGER)?;
    // signature, issuer, validity, subject
    for _ in 0..4 {
        to_be_signed = skip_element(to_be_signed, TAG_SEQUENCE)?;
    }

    Ok(read_element(to_be_signed, TAG_SEQUENCE)?.full)
}

struct Element<'a> {
    full: &'a [u8],
    content: &'a [u8],
}

fn skip_element(der: &[u8], tag: u8) -> Result<&[u8]> {
    let element = read_element(der, tag)?;
    Ok(&der[element.full.len()..])
}

fn read_element(der: &[u8], tag: u8) -> Result<Element<'_>> {
    if der.first() != Some(&tag) {
        return Err(Error::Certificate("unexpected DER tag"));
    }
    let first_length_byte = *der.get(1).ok_or(Error::Certificate("truncated length"))?;

    // Under 0x80 the byte is the length itself; at or above, its low seven bits
    // count the length's own bytes.
    let (length, header_length) = if first_length_byte < 0x80 {
        (first_length_byte as usize, 2)
    } else {
        let length_bytes = (first_length_byte & 0x7f) as usize;
        // Zero means the indefinite form, which is not valid DER. Four bytes is
        // already more than any certificate we will be handed.
        if length_bytes == 0 || length_bytes > 4 {
            return Err(Error::Certificate("unsupported DER length form"));
        }
        let bytes = der
            .get(2..2 + length_bytes)
            .ok_or(Error::Certificate("truncated length"))?;
        let mut length = 0usize;
        for byte in bytes {
            length = (length << 8) | *byte as usize;
        }
        (length, 2 + length_bytes)
    };

    let end = header_length
        .checked_add(length)
        .ok_or(Error::Certificate("length overflow"))?;
    let full = der
        .get(..end)
        .ok_or(Error::Certificate("truncated element"))?;

    Ok(Element {
        full,
        content: &full[header_length..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// openssl x509 -in cert.der -inform DER -pubkey -noout
    ///   | openssl pkey -pubin -outform DER | openssl dgst -sha256
    const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/cert.der");
    const EXPECTED_FINGERPRINT: &str = include_str!("../tests/fixtures/cert.spki.sha256");

    #[test]
    fn matches_openssl() {
        let fingerprint = Fingerprint::from_cert_der(CERT_DER).unwrap();
        assert_eq!(fingerprint.to_string(), EXPECTED_FINGERPRINT.trim());
    }

    #[test]
    fn round_trips_through_hex() {
        let fingerprint = Fingerprint::from_cert_der(CERT_DER).unwrap();
        assert_eq!(
            fingerprint.to_string().parse::<Fingerprint>().unwrap(),
            fingerprint
        );
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!("nothex".parse::<Fingerprint>().is_err());
        assert!("zz".repeat(32).parse::<Fingerprint>().is_err());
        assert!("ab".repeat(31).parse::<Fingerprint>().is_err());
    }

    #[test]
    fn rejects_truncated_certificate() {
        assert!(Fingerprint::from_cert_der(&CERT_DER[..CERT_DER.len() / 2]).is_err());
        assert!(Fingerprint::from_cert_der(&[]).is_err());
        assert!(Fingerprint::from_cert_der(&[0x30, 0x82]).is_err());
    }

    #[test]
    fn hashes_the_whole_spki_element() {
        let spki = spki_of_cert(CERT_DER).unwrap();
        assert_eq!(spki[0], TAG_SEQUENCE);
        assert_eq!(
            Fingerprint::from_spki_der(spki).to_string(),
            EXPECTED_FINGERPRINT.trim()
        );
    }
}
