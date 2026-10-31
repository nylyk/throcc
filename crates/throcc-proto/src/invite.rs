use std::fmt;
use std::str::FromStr;

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
        let chars: String = bytes
            .iter()
            .map(|b| ALPHABET[(b & 31) as usize] as char)
            .collect();
        Self::from_chars(&chars)
    }

    fn from_chars(chars: &str) -> Self {
        let (id, secret) = chars.split_at(ID_CHARS);
        Self {
            id: id.to_owned(),
            secret: secret.to_owned(),
        }
    }
}

impl FromStr for InviteCode {
    type Err = BadInviteCode;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let chars = normalize(input);
        if chars.len() != ID_CHARS + SECRET_CHARS || !input.chars().all(is_code_char) {
            return Err(BadInviteCode);
        }
        Ok(Self::from_chars(&chars))
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

fn is_code_char(c: char) -> bool {
    c == '-' || c.is_ascii_whitespace() || normalize_char(c).is_some()
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

    #[test]
    fn random_codes_parse() {
        let code = InviteCode::from_random([0, 31, 32, 255, 7, 8, 9, 10]);
        assert_eq!(code.to_string().parse::<InviteCode>().unwrap(), code);
    }
}
