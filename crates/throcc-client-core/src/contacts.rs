use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::PublicKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub key: PublicKey,
    pub verified: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactStatus {
    New,
    Known { verified: bool },
    Changed { pinned: PublicKey },
}

/// Identity keys pinned per user on one server, on first sight.
pub struct Contacts {
    path: PathBuf,
    pins: BTreeMap<UserId, Contact>,
}

impl Contacts {
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let hex: String = server.0.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("contacts-{hex}"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut pins = BTreeMap::new();
        for line in text.lines() {
            let mut parts = line.split(' ');
            let (Some(user), Some(key), Some(verified)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let key: Fingerprint = key.parse()?;
            pins.insert(
                UserId(user.parse()?),
                Contact {
                    key: key.0,
                    verified: verified == "1",
                },
            );
        }
        Ok(Self { path, pins })
    }

    pub fn status(&self, user: UserId, key: &PublicKey) -> ContactStatus {
        match self.pins.get(&user) {
            None => ContactStatus::New,
            Some(c) if c.key == *key => ContactStatus::Known {
                verified: c.verified,
            },
            Some(c) => ContactStatus::Changed { pinned: c.key },
        }
    }

    pub fn get(&self, user: UserId) -> Option<Contact> {
        self.pins.get(&user).copied()
    }

    /// Pins `key` for `user`, replacing any earlier key. A replaced key starts unverified.
    pub fn pin(&mut self, user: UserId, key: PublicKey) -> Result<()> {
        let verified = self
            .pins
            .get(&user)
            .is_some_and(|c| c.key == key && c.verified);
        self.pins.insert(user, Contact { key, verified });
        self.save()
    }

    pub fn set_verified(&mut self, user: UserId, verified: bool) -> Result<()> {
        if let Some(contact) = self.pins.get_mut(&user) {
            contact.verified = verified;
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let text: String = self
            .pins
            .iter()
            .map(|(user, c)| {
                let key = Fingerprint(c.key).to_string().replace(' ', "");
                format!("{} {key} {}\n", user.0, u8::from(c.verified))
            })
            .collect();
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_and_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        let mut contacts = Contacts::load(dir.path(), &server).unwrap();
        let user = UserId(4);
        assert_eq!(contacts.status(user, &[1; 32]), ContactStatus::New);
        contacts.pin(user, [1; 32]).unwrap();
        contacts.set_verified(user, true).unwrap();

        let mut contacts = Contacts::load(dir.path(), &server).unwrap();
        assert_eq!(
            contacts.status(user, &[1; 32]),
            ContactStatus::Known { verified: true }
        );
        assert_eq!(
            contacts.status(user, &[2; 32]),
            ContactStatus::Changed { pinned: [1; 32] }
        );
        contacts.pin(user, [2; 32]).unwrap();
        assert_eq!(
            contacts.status(user, &[2; 32]),
            ContactStatus::Known { verified: false }
        );
    }
}
