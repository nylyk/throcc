use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::PublicKey;

use crate::servers::Servers;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    #[serde(with = "crate::servers::hex32")]
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
    dir: PathBuf,
    server: Fingerprint,
    pins: BTreeMap<UserId, Contact>,
}

impl Contacts {
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let pins = Servers::load(dir)?.take(server).contacts;
        Ok(Self {
            dir: dir.to_owned(),
            server: *server,
            pins,
        })
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
        self.keep(user, Contact { key, verified })
    }

    pub fn set_verified(&mut self, user: UserId, verified: bool) -> Result<()> {
        match self.pins.get(&user) {
            Some(c) => self.keep(user, Contact { verified, ..*c }),
            None => Ok(()),
        }
    }

    fn keep(&mut self, user: UserId, contact: Contact) -> Result<()> {
        self.pins.insert(user, contact);
        Servers::update(&self.dir, |s| {
            s.server_mut(&self.server).contacts.insert(user, contact);
        })
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
