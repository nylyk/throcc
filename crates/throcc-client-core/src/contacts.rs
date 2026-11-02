use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::PublicKey;

use crate::lock;
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

/// Identity keys pinned per user on one server, on first sight. Changes are kept in
/// memory at once and reach the file through [`UnsavedPins::save`].
pub struct Contacts {
    pins: BTreeMap<UserId, Contact>,
    unsaved: Arc<UnsavedPins>,
}

/// Pins changed but not yet written, shared with whoever writes them, so the lock guarding
/// the contacts is never held for file I/O.
pub struct UnsavedPins {
    dir: PathBuf,
    server: Fingerprint,
    pins: Mutex<BTreeMap<UserId, Contact>>,
    /// Held across a write, so the pins taken later land later.
    writing: Mutex<()>,
}

impl UnsavedPins {
    pub fn is_empty(&self) -> bool {
        lock(&self.pins).is_empty()
    }

    /// Writes the pins changed so far. Pins a failed write missed stay for the next one.
    pub fn save(&self) -> Result<()> {
        let _writing = lock(&self.writing);
        let pins = std::mem::take(&mut *lock(&self.pins));
        if pins.is_empty() {
            return Ok(());
        }
        let saved = Servers::update(&self.dir, |s| {
            s.server_mut(&self.server).contacts.extend(pins.clone());
        });
        if saved.is_err() {
            let mut unsaved = lock(&self.pins);
            for (user, contact) in pins {
                unsaved.entry(user).or_insert(contact);
            }
        }
        saved
    }
}

impl Contacts {
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let pins = Servers::load(dir)?.take(server).contacts;
        let unsaved = UnsavedPins {
            dir: dir.to_owned(),
            server: *server,
            pins: Mutex::default(),
            writing: Mutex::default(),
        };
        Ok(Self {
            pins,
            unsaved: Arc::new(unsaved),
        })
    }

    pub fn unsaved(&self) -> Arc<UnsavedPins> {
        self.unsaved.clone()
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
    pub fn pin(&mut self, user: UserId, key: PublicKey) {
        let verified = self
            .pins
            .get(&user)
            .is_some_and(|c| c.key == key && c.verified);
        self.keep(user, Contact { key, verified })
    }

    pub fn set_verified(&mut self, user: UserId, verified: bool) {
        if let Some(c) = self.pins.get(&user) {
            self.keep(user, Contact { verified, ..*c });
        }
    }

    fn keep(&mut self, user: UserId, contact: Contact) {
        self.pins.insert(user, contact);
        lock(&self.unsaved.pins).insert(user, contact);
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
        contacts.pin(user, [1; 32]);
        contacts.set_verified(user, true);
        let unsaved = Contacts::load(dir.path(), &server).unwrap();
        assert_eq!(unsaved.status(user, &[1; 32]), ContactStatus::New);
        contacts.unsaved().save().unwrap();
        assert!(contacts.unsaved().is_empty());

        let mut contacts = Contacts::load(dir.path(), &server).unwrap();
        assert_eq!(
            contacts.status(user, &[1; 32]),
            ContactStatus::Known { verified: true }
        );
        assert_eq!(
            contacts.status(user, &[2; 32]),
            ContactStatus::Changed { pinned: [1; 32] }
        );
        contacts.pin(user, [2; 32]);
        assert_eq!(
            contacts.status(user, &[2; 32]),
            ContactStatus::Known { verified: false }
        );
    }
}
