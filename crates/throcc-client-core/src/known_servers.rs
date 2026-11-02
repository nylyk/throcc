use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use throcc_proto::Fingerprint;

use crate::servers::{Hex, Servers};

/// Pinned server keys by address.
pub struct KnownServers {
    dir: PathBuf,
    pins: BTreeMap<String, Fingerprint>,
}

impl KnownServers {
    pub fn load(dir: &Path) -> Result<Self> {
        let pins = Servers::load(dir)?
            .known
            .into_iter()
            .map(|(address, Hex(fp))| (address, Fingerprint(fp)))
            .collect();
        Ok(Self {
            dir: dir.to_owned(),
            pins,
        })
    }

    pub fn get(&self, address: &str) -> Option<Fingerprint> {
        self.pins.get(address).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Fingerprint)> {
        self.pins.iter().map(|(a, f)| (a.as_str(), *f))
    }

    /// Pins `fingerprint` for `address`, as the user chose by enrolling there or by
    /// trusting its new key. The people pinned under a key it replaces come along, so a
    /// server that changed its own key still can't change theirs unnoticed.
    pub fn trust(&mut self, address: &str, fingerprint: Fingerprint) -> Result<()> {
        self.pins.insert(address.to_owned(), fingerprint);
        Servers::update(&self.dir, |s| {
            let new = Hex(fingerprint.0);
            if let Some(old) = s.known.insert(address.to_owned(), new)
                && old != new
            {
                s.carry_contacts(old, new);
            }
        })
    }

    pub fn forget(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        Servers::update(&self.dir, |s| {
            s.known.remove(address);
        })
    }
}

#[cfg(test)]
mod tests {
    use throcc_proto::ids::UserId;

    use super::*;
    use crate::contacts::{ContactStatus, Contacts};

    #[test]
    fn a_new_key_for_the_same_address_keeps_the_people_pinned_under_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let mut known = KnownServers::load(dir.path()).unwrap();
        known.trust("voice.example.net", old).unwrap();
        let mut contacts = Contacts::load(dir.path(), &old).unwrap();
        contacts.pin(UserId(4), [4; 32]);
        contacts.set_verified(UserId(4), true);
        contacts.pin(UserId(5), [5; 32]);
        contacts.unsaved().save().unwrap();
        let mut seen_under_new = Contacts::load(dir.path(), &new).unwrap();
        seen_under_new.pin(UserId(5), [55; 32]);
        seen_under_new.unsaved().save().unwrap();

        known.trust("elsewhere.example.net", new).unwrap();
        assert_eq!(
            Contacts::load(dir.path(), &new).unwrap().get(UserId(4)),
            None,
            "another address carried them"
        );
        known.trust("voice.example.net", new).unwrap();
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
