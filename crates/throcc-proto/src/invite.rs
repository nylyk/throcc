use std::fmt;
use std::str::FromStr;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const ID_CHARS: usize = 3;
pub const SECRET_CHARS: usize = 5;

/// An invite such as `K7Q-M2XPD`: a public lookup id and a secret, in Crockford base32.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteCode {
    pub id: String,
    pub secret: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("an invite code looks like K7Q-M2XPD")]
pub struct BadInviteCode;

impl InviteCode {
    /// Builds a code from 8 random bytes, one character per byte.
    pub fn from_random(bytes: [u8; ID_CHARS + SECRET_CHARS]) -> Self {
        let encode = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| ALPHABET[(b & 31) as usize] as char)
                .collect()
        };
        let (id, secret) = bytes.split_at(ID_CHARS);
        Self {
            id: encode(id),
            secret: encode(secret),
        }
    }
}

impl FromStr for InviteCode {
    type Err = BadInviteCode;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let chars: String = input
            .chars()
            .filter(|&c| c != '-' && !c.is_ascii_whitespace())
            .map(normalize_char)
            .collect::<Option<_>>()
            .ok_or(BadInviteCode)?;
        if chars.len() != ID_CHARS + SECRET_CHARS {
            return Err(BadInviteCode);
        }
        let (id, secret) = chars.split_at(ID_CHARS);
        Ok(Self {
            id: id.to_owned(),
            secret: secret.to_owned(),
        })
    }
}

impl fmt::Display for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.id, self.secret)
    }
}

impl fmt::Debug for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InviteCode({}-*****)", self.id)
    }
}

/// An invite link such as `https://thro.cc/join#voice.example.com+K7Q-M2XPD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteLink {
    pub address: String,
    pub code: InviteCode,
}

const WEB_LINK: &str = "https://thro.cc/join#";
const DESKTOP_LINK: &str = "throcc://join#";
const MAX_ADDRESS_CHARS: usize = 255;
const KEPT_IN_ADDRESS: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':');

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("an invite link looks like https://thro.cc/join#voice.example.com+K7Q-M2XPD")]
pub struct BadInviteLink;

impl fmt::Display for InviteLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let address = utf8_percent_encode(&self.address, KEPT_IN_ADDRESS);
        write!(f, "{WEB_LINK}{address}+{}", self.code)
    }
}

impl FromStr for InviteLink {
    type Err = BadInviteLink;

    /// Parses a web link or the `throcc://join#` link the web page hands to desktop clients.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let fragment = [WEB_LINK, DESKTOP_LINK]
            .iter()
            .find_map(|prefix| input.trim().strip_prefix(prefix))
            .ok_or(BadInviteLink)?;
        let (address, code) = fragment.rsplit_once('+').ok_or(BadInviteLink)?;
        let address = percent_decode_str(address)
            .decode_utf8()
            .map_err(|_| BadInviteLink)?;
        let unfit = |c: char| c.is_whitespace() || c.is_control() || "+/\\@?#".contains(c);
        if address.is_empty()
            || address.chars().count() > MAX_ADDRESS_CHARS
            || address.contains(unfit)
        {
            return Err(BadInviteLink);
        }
        Ok(Self {
            address: address.into_owned(),
            code: code.parse().map_err(|_| BadInviteLink)?,
        })
    }
}

/// Formats partial input for a code field: normalized characters with the dash inserted.
pub fn format_partial(input: &str) -> String {
    let mut chars = normalize(input);
    chars.truncate(ID_CHARS + SECRET_CHARS);
    if chars.len() > ID_CHARS {
        chars.insert(ID_CHARS, '-');
    }
    chars
}

/// Upper-cases, maps O to 0 and I, L to 1, and drops anything outside the alphabet.
pub fn normalize(input: &str) -> String {
    input.chars().filter_map(normalize_char).collect()
}

fn normalize_char(c: char) -> Option<char> {
    match c.to_ascii_uppercase() {
        'O' => Some('0'),
        'I' | 'L' => Some('1'),
        c if c.is_ascii() && ALPHABET.contains(&(c as u8)) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_and_without_dash() {
        let code: InviteCode = "K7Q-M2XPD".parse().unwrap();
        assert_eq!((code.id.as_str(), code.secret.as_str()), ("K7Q", "M2XPD"));
        assert_eq!("k7qm2xpd".parse::<InviteCode>().unwrap(), code);
        assert_eq!(code.to_string(), "K7Q-M2XPD");
    }

    #[test]
    fn maps_confusable_characters() {
        let code: InviteCode = "oil-L0OIi".parse().unwrap();
        assert_eq!(code.to_string(), "011-10011");
    }

    #[test]
    fn rejects_bad_codes() {
        for bad in ["K7Q-M2XP", "K7Q-M2XPDD", "K7Q-M2XPU", "K7Q_M2XPD", ""] {
            assert_eq!(bad.parse::<InviteCode>(), Err(BadInviteCode), "{bad}");
        }
    }

    #[test]
    fn formats_partial_input() {
        assert_eq!(format_partial("k7"), "K7");
        assert_eq!(format_partial("k7q"), "K7Q");
        assert_eq!(format_partial("k7qm"), "K7Q-M");
        assert_eq!(format_partial("k7q-m2xpdzz"), "K7Q-M2XPD");
    }

    fn link(address: &str) -> InviteLink {
        InviteLink {
            address: address.to_owned(),
            code: "K7Q-M2XPD".parse().unwrap(),
        }
    }

    #[test]
    fn formats_links() {
        assert_eq!(
            link("voice.example.com:8476").to_string(),
            "https://thro.cc/join#voice.example.com:8476+K7Q-M2XPD"
        );
        assert_eq!(
            link("[2001:db8::1]:8476").to_string(),
            "https://thro.cc/join#%5B2001:db8::1%5D:8476+K7Q-M2XPD"
        );
    }

    #[test]
    fn parses_links() {
        for address in ["voice.example.com", "[2001:db8::1]:8476", "grüße.example"] {
            let link = link(address);
            assert_eq!(link.to_string().parse::<InviteLink>(), Ok(link));
        }
        assert_eq!(
            "throcc://join#voice.example.com+k7qm2xpd".parse(),
            Ok(link("voice.example.com"))
        );
    }

    #[test]
    fn rejects_bad_links() {
        for bad in [
            "https://thro.cc/join#voice.example.com",
            "https://thro.cc/join#+K7Q-M2XPD",
            "https://thro.cc/join#voice.example.com+K7Q",
            "https://thro.cc/join#a%20b+K7Q-M2XPD",
            "https://thro.cc/join#a%2Fb+K7Q-M2XPD",
            "https://thro.cc/join#%FF+K7Q-M2XPD",
            "https://example.com/join#voice.example.com+K7Q-M2XPD",
            "throcc://other#voice.example.com+K7Q-M2XPD",
        ] {
            assert_eq!(bad.parse::<InviteLink>(), Err(BadInviteLink), "{bad}");
        }
    }

    #[test]
    fn random_codes_parse() {
        let code = InviteCode::from_random([0, 31, 32, 255, 7, 8, 9, 10]);
        assert_eq!(code.to_string().parse::<InviteCode>().unwrap(), code);
    }
}
