use std::sync::Arc;

use serde::{Deserialize, Serialize};
use throcc_proto::ids::Hash;
use throcc_proto::messages::{
    MAX_AVATAR_BYTES, MAX_NAME_CHARS, MAX_PASSWORD_CHARS, shows_something, valid_name,
    valid_password,
};

/// Our name and picture, kept on this computer and given to every server we join.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default, with = "base64_bytes")]
    pub picture: Option<Arc<[u8]>>,
}

impl Profile {
    pub fn avatar(&self) -> Option<Hash> {
        self.picture.as_deref().map(Hash::of)
    }
}

/// What a typed name is to a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameCheck {
    Empty,
    /// The name as a server keeps it, trimmed.
    Fine(String),
    TooLong,
    ShowsNothing,
    HidesText,
}

impl NameCheck {
    /// Why a server would refuse the name, in words for the user.
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::Empty | Self::Fine(_) => None,
            Self::TooLong => Some(format!("Names can be at most {MAX_NAME_CHARS} characters.")),
            Self::ShowsNothing => Some("Names need at least one visible character.".into()),
            Self::HidesText => Some("Names can’t contain control or invisible characters.".into()),
        }
    }

    /// The typed name: `Ok(None)` when empty, and why the server would refuse it if it would.
    pub fn into_result(self) -> Result<Option<String>, String> {
        match self {
            Self::Empty => Ok(None),
            Self::Fine(name) => Ok(Some(name)),
            refused => Err(refused.problem().unwrap_or_default()),
        }
    }
}

/// What `typed` is as a name, as every server judges it.
pub fn check_name(typed: &str) -> NameCheck {
    if typed.trim().is_empty() {
        return NameCheck::Empty;
    }
    match valid_name(typed) {
        Some(name) => NameCheck::Fine(name.to_owned()),
        None if typed.trim().chars().count() > MAX_NAME_CHARS => NameCheck::TooLong,
        None if !shows_something(typed) => NameCheck::ShowsNothing,
        None => NameCheck::HidesText,
    }
}

/// Why a server would refuse `typed` as a room password, if it would.
pub fn password_problem(typed: &str) -> Option<String> {
    (!valid_password(typed))
        .then(|| format!("Passwords can be at most {MAX_PASSWORD_CHARS} characters."))
}

/// A typed password: `Ok(None)` when empty, and why the server would refuse it if it would.
pub fn check_password(typed: &str) -> Result<Option<String>, String> {
    if typed.is_empty() {
        return Ok(None);
    }
    match password_problem(typed) {
        Some(problem) => Err(problem),
        None => Ok(Some(typed.to_owned())),
    }
}

/// Why a server would refuse `picture` as a profile picture, if it would.
pub fn picture_problem(picture: &[u8]) -> Option<String> {
    (picture.len() > MAX_AVATAR_BYTES)
        .then(|| format!("Pictures can be at most {} KiB.", MAX_AVATAR_BYTES / 1024))
}

mod base64_bytes {
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Arc<[u8]>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => s.serialize_some(&STANDARD.encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<[u8]>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| {
                STANDARD
                    .decode(text)
                    .map(Arc::from)
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_judged_as_servers_judge_them() {
        assert_eq!(check_name(""), NameCheck::Empty);
        assert_eq!(check_name("   "), NameCheck::Empty);
        assert_eq!(check_name("  Lobby "), NameCheck::Fine("Lobby".into()));
        let longest = "é".repeat(MAX_NAME_CHARS);
        assert_eq!(check_name(&longest), NameCheck::Fine(longest.clone()));
        assert_eq!(check_name(&format!("{longest}x")), NameCheck::TooLong);
        assert_eq!(check_name("a\tb"), NameCheck::HidesText);
        assert_eq!(check_name("adm\u{202E}nimda"), NameCheck::HidesText);
        assert_eq!(check_name("\u{200B}"), NameCheck::HidesText);
        for blank in [
            "\u{2800}",
            "\u{200D}",
            "\u{FE0F}\u{FE0F}",
            "\u{2800} \u{200C}",
        ] {
            assert_eq!(check_name(blank), NameCheck::ShowsNothing, "{blank:?}");
        }
        let too_long = NameCheck::TooLong.problem().unwrap();
        assert_eq!(too_long, "Names can be at most 32 characters.");
        assert_eq!(
            check_name("x\u{202E}").into_result(),
            Err(NameCheck::HidesText.problem().unwrap())
        );
    }

    #[test]
    fn passwords_are_judged_as_servers_judge_them() {
        assert_eq!(check_password(""), Ok(None));
        assert_eq!(check_password(" "), Ok(Some(" ".into())));
        let longest = "p".repeat(MAX_PASSWORD_CHARS);
        assert_eq!(check_password(&longest), Ok(Some(longest.clone())));
        let refused = check_password(&format!("{longest}p")).unwrap_err();
        assert_eq!(refused, "Passwords can be at most 128 characters.");
    }
}
