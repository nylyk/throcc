use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use throcc_proto::Fingerprint;

use crate::lock;
use crate::servers::{Hex, Servers};

/// Pinned server keys by address.
pub struct KnownServers {
    dir: PathBuf,
    pins: BTreeMap<String, Fingerprint>,
    /// Addresses whose new key the user trusted, whose people pinned under the old key
    /// haven't come along yet.
    replaced: Mutex<BTreeSet<String>>,
}

impl KnownServers {
    pub fn load(dir: &Path) -> Result<Self> {
        let servers = Servers::load(dir)?;
        let pins = servers
            .known
            .into_iter()
            .map(|(address, Hex(fp))| (address, Fingerprint(fp)))
            .collect();
        Ok(Self {
            dir: dir.to_owned(),
            pins,
            replaced: Mutex::new(servers.replaced.into_keys().collect()),
        })
    }

    pub fn get(&self, address: &str) -> Option<Fingerprint> {
        self.pins.get(address).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Fingerprint)> {
        self.pins.iter().map(|(a, f)| (a.as_str(), *f))
    }

    /// Pins `fingerprint` for `address` after enrolling there. Nobody pinned under an
    /// earlier key comes along: a new account where the key changed most likely means a
    /// rebuilt server, whose user ids now belong to other people.
    pub fn trust(&mut self, address: &str, fingerprint: Fingerprint) -> Result<()> {
        self.pins.insert(address.to_owned(), fingerprint);
        lock(&self.replaced).remove(address);
        Servers::update(&self.dir, |s| {
            s.known.insert(address.to_owned(), Hex(fingerprint.0));
            s.replaced.remove(address);
        })
    }

    /// Pins the new key `address` presents, as the user chose. The people pinned under the
    /// old key come along once we sign in there with the new one ([`Self::signed_in`]), so a
    /// server that changed its own key still can't change theirs unnoticed.
    pub fn trust_new_key(&mut self, address: &str, fingerprint: Fingerprint) -> Result<()> {
        self.pins.insert(address.to_owned(), fingerprint);
        let new = Hex(fingerprint.0);
        let mut replaced = false;
        Servers::update(&self.dir, |s| {
            if let Some(old) = s.known.insert(address.to_owned(), new)
                && old != new
            {
                s.replaced.entry(address.to_owned()).or_insert(old);
            }
            replaced = s.replaced.contains_key(address);
        })?;
        if replaced {
            lock(&self.replaced).insert(address.to_owned());
        }
        Ok(())
    }

    /// Brings along the people pinned under the key `address` had before the user trusted
    /// its new one, now that our account signed in under the new key: the account still
    /// being there means the server kept its users, so their ids still name the same people.
    pub fn signed_in(&self, address: &str) -> Result<()> {
        if !lock(&self.replaced).contains(address) {
            return Ok(());
        }
        Servers::update(&self.dir, |s| {
            if let Some(old) = s.replaced.remove(address)
                && let Some(new) = s.known.get(address).copied()
            {
                s.carry_contacts(old, new);
            }
        })?;
        lock(&self.replaced).remove(address);
        Ok(())
    }

    /// Takes `address` off the list, as the user chose, while they stay a member there.
    /// The people pinned under its key stay, so adding it again still notices a changed key.
    pub fn forget(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        lock(&self.replaced).remove(address);
        Servers::update(&self.dir, |s| {
            s.known.remove(address);
            s.replaced.remove(address);
        })
    }

    /// Takes `address` off the list after the user left that server. What is kept for its
    /// key, such as the people pinned there, goes too unless another address still uses it.
    pub fn leave(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        lock(&self.replaced).remove(address);
        Servers::update(&self.dir, |s| {
            s.replaced.remove(address);
            if let Some(key) = s.known.remove(address)
                && !s.known.values().any(|other| *other == key)
            {
                s.servers.remove(&key);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use throcc_proto::ids::UserId;

    use super::*;
    use crate::contacts::{ContactStatus, Contacts};

    /// A known server at `old` with user 4 pinned and verified and user 5 pinned, and user 5
    /// pinned differently under `new`.
    fn pinned_under(dir: &Path, old: Fingerprint, new: Fingerprint) -> KnownServers {
        let mut known = KnownServers::load(dir).unwrap();
        known.trust("voice.example.net", old).unwrap();
        let mut contacts = Contacts::load(dir, &old).unwrap();
        contacts.pin(UserId(4), [4; 32]);
        contacts.set_verified(UserId(4), true);
        contacts.pin(UserId(5), [5; 32]);
        contacts.unsaved().save().unwrap();
        let mut seen_under_new = Contacts::load(dir, &new).unwrap();
        seen_under_new.pin(UserId(5), [55; 32]);
        seen_under_new.unsaved().save().unwrap();
        known
    }

    #[test]
    fn a_trusted_new_key_brings_the_people_pinned_under_the_old_one_once_signed_in() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let mut known = pinned_under(dir.path(), old, new);

        known.trust_new_key("elsewhere.example.net", new).unwrap();
        known.signed_in("elsewhere.example.net").unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &new).unwrap().get(UserId(4)),
            None,
            "another address carried them"
        );
        known.trust_new_key("voice.example.net", new).unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &new).unwrap().get(UserId(4)),
            None,
            "carried before signing in"
        );
        KnownServers::load(dir.path())
            .unwrap()
            .signed_in("voice.example.net")
            .unwrap();
        let carried = Contacts::load(dir.path(), &new).unwrap();
        assert_eq!(
            carried.status(UserId(4), &[4; 32]),
            ContactStatus::Known { verified: true }
        );
        assert_eq!(
            carried.status(UserId(4), &[9; 32]),
            ContactStatus::Changed { pinned: [4; 32] }
        );
        assert_eq!(
            carried.status(UserId(5), &[55; 32]),
            ContactStatus::Known { verified: false },
            "a pin made under the new key was overwritten"
        );
    }

    #[test]
    fn enrolling_where_the_key_changed_brings_nobody_along() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let mut known = pinned_under(dir.path(), old, new);
        known.trust("voice.example.net", new).unwrap();
        known.signed_in("voice.example.net").unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &new).unwrap().get(UserId(4)),
            None
        );

        let mut known = pinned_under(dir.path(), old, new);
        known.trust_new_key("voice.example.net", new).unwrap();
        known.trust("voice.example.net", new).unwrap();
        known.signed_in("voice.example.net").unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &new).unwrap().get(UserId(4)),
            None,
            "a new account after trusting the new key carried them"
        );
    }

    #[test]
    fn leaving_a_server_drops_its_people_unless_another_address_uses_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let (shared, alone) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let mut known = KnownServers::load(dir.path()).unwrap();
        known.trust("a.example.net", shared).unwrap();
        known.trust("b.example.net", shared).unwrap();
        known.trust("c.example.net", alone).unwrap();
        for server in [shared, alone] {
            let mut contacts = Contacts::load(dir.path(), &server).unwrap();
            contacts.pin(UserId(4), [4; 32]);
            contacts.unsaved().save().unwrap();
        }
        let pinned = |server| {
            Contacts::load(dir.path(), &server)
                .unwrap()
                .get(UserId(4))
                .is_some()
        };

        known.leave("a.example.net").unwrap();
        known.leave("c.example.net").unwrap();
        assert!(pinned(shared));
        assert!(!pinned(alone));
        known.leave("b.example.net").unwrap();
        assert!(!pinned(shared));
        assert_eq!(known.get("b.example.net"), None);
    }

    #[test]
    fn removing_a_server_keeps_its_people_for_when_it_is_added_again() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([1; 32]);
        let mut known = KnownServers::load(dir.path()).unwrap();
        known.trust("a.example.net", server).unwrap();
        let mut contacts = Contacts::load(dir.path(), &server).unwrap();
        contacts.pin(UserId(4), [4; 32]);
        contacts.unsaved().save().unwrap();

        known.forget("a.example.net").unwrap();
        assert_eq!(known.get("a.example.net"), None);
        assert_eq!(
            KnownServers::load(dir.path()).unwrap().get("a.example.net"),
            None
        );
        known.trust("a.example.net", server).unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &server)
                .unwrap()
                .status(UserId(4), &[9; 32]),
            ContactStatus::Changed { pinned: [4; 32] }
        );
    }

    #[test]
    fn pins_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let fp = Fingerprint::of_spki(b"key");
        KnownServers::load(dir.path())
            .unwrap()
            .trust("voice.example.net", fp)
            .unwrap();
        let reloaded = KnownServers::load(dir.path()).unwrap();
        assert_eq!(reloaded.get("voice.example.net"), Some(fp));
        assert_eq!(reloaded.get("other"), None);
    }
}
