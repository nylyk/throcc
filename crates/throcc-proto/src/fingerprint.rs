use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

/// SHA-256 of a DER-encoded SubjectPublicKeyInfo.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_spki(spki_der: &[u8]) -> Self {
        Self(Sha256::digest(spki_der).into())
    }

    /// First 8 hex digits, as two groups of 4.
    pub fn short(&self) -> String {
        let full = self.to_string();
        full[..9].to_owned()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, pair) in self.0.chunks(2).enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{:02x}{:02x}", pair[0], pair[1])?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("a fingerprint is 64 hex digits")]
pub struct ParseFingerprintError;

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if digits.len() != 64 {
            return Err(ParseFingerprintError);
        }
        let mut out = [0u8; 32];
        for (byte, pair) in out.iter_mut().zip(digits.chunks(2)) {
            let hex = std::str::from_utf8(pair).map_err(|_| ParseFingerprintError)?;
            *byte = u8::from_str_radix(hex, 16).map_err(|_| ParseFingerprintError)?;
        }
        Ok(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_round_trips() {
        let fp = Fingerprint::of_spki(b"spki");
        let text = fp.to_string();
        assert_eq!(text.len(), 64 + 15);
        assert_eq!(text.parse::<Fingerprint>().unwrap(), fp);
        assert_eq!(fp.short(), text[..9]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!("abcd".parse::<Fingerprint>().is_err());
        assert!("zz".repeat(32).parse::<Fingerprint>().is_err());
    }
}
