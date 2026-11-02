use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::{PublicKey, valid_name};

use crate::lock;
use crate::servers::Servers;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    #[serde(with = "crate::servers::hex32")]
    pub key: PublicKey,
    pub verified: bool,
    /// The first few names the server showed them with. Later names don't replace them,
    /// so someone new taking a name they had stands out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactStatus {
    New,
    Known { verified: bool },
    Changed { pinned: PublicKey },
}

/// Identity keys pinned per user on one server, on first sight. Changes are kept in
/// memory at once and reach the file through [`UnsavedPins::save`].
/// How many names are noted per pinned user. The first ones are kept, so a server can't
/// flush out a name by renaming someone over and over.
const MAX_NAMES: usize = 4;
/// How many users are pinned per server at most. Beyond it nobody new is pinned, rather
/// than forgetting anyone: a forgotten pin would let a server swap that user's key unseen.
pub const MAX_CONTACTS: usize = 4096;

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
    failure: Mutex<SaveFailure>,
}

#[derive(Default)]
enum SaveFailure {
    #[default]
    None,
    Unreported(String),
    Reported,
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
        let mut failure = lock(&self.failure);
        match &saved {
            Ok(()) => *failure = SaveFailure::None,
            Err(e) => {
                if matches!(*failure, SaveFailure::None) {
                    *failure = SaveFailure::Unreported(format!("{e:#}"));
                }
                let mut unsaved = lock(&self.pins);
                for (user, contact) in pins {
                    unsaved.entry(user).or_insert(contact);
                }
            }
        }
        saved
    }

    /// Why pins stopped reaching the file, once per run of failed writes.
    pub fn take_failure(&self) -> Option<String> {
        let mut failure = lock(&self.failure);
        match std::mem::take(&mut *failure) {
            SaveFailure::Unreported(why) => {
                *failure = SaveFailure::Reported;
                Some(why)
            }
            other => {
                *failure = other;
                None
            }
        }
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
            failure: Mutex::default(),
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

    pub fn get(&self, user: UserId) -> Option<&Contact> {
        self.pins.get(&user)
    }

    /// Pins `key` for `user`, replacing any earlier key. A replaced key starts unverified.
    /// False, pinning nothing, for a new user once [`MAX_CONTACTS`] are pinned.
    pub fn pin(&mut self, user: UserId, key: PublicKey) -> bool {
        let earlier = self.pins.get(&user);
        if earlier.is_none() && self.pins.len() >= MAX_CONTACTS {
            return false;
        }
        let verified = earlier.is_some_and(|c| c.key == key && c.verified);
        let names = earlier.map(|c| c.names.clone()).unwrap_or_default();
        self.keep(
            user,
            Contact {
                key,
                verified,
                names,
            },
        );
        true
    }

    pub fn set_verified(&mut self, user: UserId, verified: bool) {
        if let Some(c) = self.pins.get(&user) {
            let contact = Contact {
                verified,
                ..c.clone()
            };
            self.keep(user, contact);
        }
    }

    /// Notes a name a pinned user is shown with, until [`MAX_NAMES`] are noted.
    pub fn remember_name(&mut self, user: UserId, name: &str) {
        let Some(name) = valid_name(name) else {
            return;
        };
        let Some(c) = self.pins.get(&user) else {
            return;
        };
        let noted = c.names.iter().any(|n| same_name(n, name));
        if !noted && c.names.len() < MAX_NAMES {
            let mut contact = c.clone();
            contact.names.push(name.to_owned());
            self.keep(user, contact);
        }
    }

    /// Someone pinned here other than `user` who goes by `name`: by a name noted for them,
    /// or the one `shown` gives them now.
    pub fn namesake<'a>(
        &self,
        user: UserId,
        name: &str,
        shown: impl IntoIterator<Item = (UserId, &'a str)>,
    ) -> Option<UserId> {
        let noted = self
            .pins
            .iter()
            .filter(|(_, c)| c.names.iter().any(|n| same_name(n, name)))
            .map(|(u, _)| *u);
        let shown = shown
            .into_iter()
            .filter(|(u, n)| self.pins.contains_key(u) && same_name(n, name))
            .map(|(u, _)| u);
        noted.chain(shown).find(|other| *other != user)
    }

    /// Takes in what `older`, an earlier connection's contacts for this server, knew, in
    /// case some of it never reached the file.
    pub fn carry_from(&mut self, older: &Contacts) {
        if older.unsaved.server != self.unsaved.server {
            return;
        }
        for (user, contact) in &older.pins {
            if self.pins.get(user) != Some(contact) {
                self.keep(*user, contact.clone());
            }
        }
    }

    fn keep(&mut self, user: UserId, contact: Contact) {
        lock(&self.unsaved.pins).insert(user, contact.clone());
        self.pins.insert(user, contact);
    }
}

/// True for valid names that are the same ignoring case and surrounding space.
fn same_name(a: &str, b: &str) -> bool {
    match (valid_name(a), valid_name(b)) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
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

    #[test]
    fn pins_a_failed_write_missed_are_kept_and_the_failure_is_told_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let server = Fingerprint([7; 32]);
        let mut contacts = Contacts::load(&config, &server).unwrap();
        std::fs::write(&config, b"not a directory yet").unwrap();
        contacts.pin(UserId(4), [1; 32]);
        let unsaved = contacts.unsaved();
        assert!(unsaved.save().is_err());
        contacts.pin(UserId(5), [2; 32]);
        assert!(unsaved.save().is_err());
        assert!(unsaved.take_failure().is_some());
        assert_eq!(unsaved.take_failure(), None, "told twice");

        std::fs::remove_file(&config).unwrap();
        unsaved.save().unwrap();
        assert_eq!(unsaved.take_failure(), None);
        let saved = Contacts::load(&config, &server).unwrap();
        assert!(saved.get(UserId(4)).is_some() && saved.get(UserId(5)).is_some());
    }

    #[test]
    fn an_earlier_connections_contacts_carry_over_only_on_the_same_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        let mut older = Contacts::load(dir.path(), &server).unwrap();
        older.pin(UserId(4), [1; 32]);
        older.set_verified(UserId(4), true);

        let mut elsewhere = Contacts::load(dir.path(), &Fingerprint([8; 32])).unwrap();
        elsewhere.carry_from(&older);
        assert_eq!(elsewhere.get(UserId(4)), None);
        let mut newer = Contacts::load(dir.path(), &server).unwrap();
        newer.carry_from(&older);
        assert_eq!(
            newer.status(UserId(4), &[1; 32]),
            ContactStatus::Known { verified: true }
        );
        assert!(!newer.unsaved().is_empty());
    }

    #[test]
    fn nobody_new_is_pinned_past_the_limit_and_nobody_pinned_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        let mut contacts = Contacts::load(dir.path(), &server).unwrap();
        for id in 0..MAX_CONTACTS as u64 {
            assert!(contacts.pin(UserId(id), [1; 32]));
        }
        assert!(!contacts.pin(UserId(MAX_CONTACTS as u64), [1; 32]));
        assert_eq!(
            contacts.status(UserId(MAX_CONTACTS as u64), &[1; 32]),
            ContactStatus::New
        );
        assert!(
            contacts.pin(UserId(0), [2; 32]),
            "a pinned user's key can still change"
        );
        contacts.unsaved().save().unwrap();
        let reloaded = Contacts::load(dir.path(), &server).unwrap();
        assert_eq!(
            reloaded.status(UserId(1), &[2; 32]),
            ContactStatus::Changed { pinned: [1; 32] }
        );
    }

    #[test]
    fn a_namesake_is_found_by_a_name_noted_or_shown_now() {
        let dir = tempfile::tempdir().unwrap();
        let mut contacts = Contacts::load(dir.path(), &Fingerprint([7; 32])).unwrap();
        let (bob, carol, ghost, stranger) = (UserId(2), UserId(3), UserId(9), UserId(10));
        contacts.pin(bob, [2; 32]);
        for name in ["user2", "Bob", "bob", "Robert", "R", "Rob", "Bobby"] {
            contacts.remember_name(bob, name);
        }
        assert_eq!(
            contacts.get(bob).unwrap().names,
            ["user2", "Bob", "Robert", "R"]
        );
        contacts.pin(carol, [3; 32]);
        contacts.pin(ghost, [9; 32]);

        assert_eq!(contacts.namesake(ghost, " bOB ", []), Some(bob));
        assert_eq!(contacts.namesake(bob, "Bob", []), None);
        assert_eq!(
            contacts.namesake(ghost, "Carol", [(carol, "carol")]),
            Some(carol)
        );
        assert_eq!(
            contacts.namesake(ghost, "Dave", [(stranger, "Dave")]),
            None,
            "someone not pinned isn't known"
        );
        assert_eq!(
            contacts.namesake(ghost, "Bob\u{200B}", [(carol, "Bob\u{200B}")]),
            None,
            "a name that hides text matches nothing"
        );
    }
}
