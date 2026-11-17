use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use throcc_proto::{DEFAULT_PORT, Fingerprint};

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

    /// The pin for `address`, or for the same address spelled another way.
    pub fn get(&self, address: &str) -> Option<Fingerprint> {
        if let Some(pin) = self.pins.get(address) {
            return Some(*pin);
        }
        let wanted = canonical(address);
        let alike = self
            .pins
            .iter()
            .find(|(known, _)| canonical(known) == wanted);
        alike.map(|(_, pin)| *pin)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Fingerprint)> {
        self.pins.iter().map(|(a, f)| (a.as_str(), *f))
    }

    /// Pins `fingerprint` for `address`, after enrolling there, after the server proved it holds
    /// our account, or when the user trusts the new key it presents.
    pub fn trust(&mut self, address: &str, fingerprint: Fingerprint) -> Result<()> {
        self.pins.insert(address.to_owned(), fingerprint);
        Servers::update(&self.dir, |s| {
            s.known.insert(address.to_owned(), Hex(fingerprint.0));
        })
    }

    /// Takes `address` off the list, as the user chose, while they stay a member there.
    /// What is kept for its key stays, so adding it again finds it.
    pub fn forget(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        Servers::update(&self.dir, |s| {
            s.known.remove(address);
        })
    }

    /// Takes every server off the list with what is kept for each, as when starting over
    /// with a new identity they don't know.
    pub fn forget_all(&mut self) -> Result<()> {
        self.pins.clear();
        Servers::update(&self.dir, |s| {
            s.known.clear();
            s.servers.clear();
        })
    }

    /// Takes `address` off the list after the user left that server. What is kept for its
    /// key goes too unless another address still uses it.
    pub fn leave(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        Servers::update(&self.dir, |s| {
            if let Some(key) = s.known.remove(address)
                && !s.known.values().any(|other| *other == key)
            {
                s.servers.remove(&key);
            }
        })
    }
}

/// `address` lower-cased, without a trailing dot on its host or the default port, so two
/// spellings of one server compare equal.
fn canonical(address: &str) -> String {
    let address = address.trim().to_lowercase();
    let default_port = format!(":{DEFAULT_PORT}");
    let host = match address.strip_suffix(&default_port) {
        Some(host) if !host.contains(':') || host.ends_with(']') => host,
        _ => &address,
    };
    host.strip_suffix('.').unwrap_or(host).to_owned()
}

#[cfg(test)]
mod tests {
    use throcc_proto::ids::{Epoch, RoomId, UserId};

    use super::*;
    use crate::floors::EpochFloors;

    /// Keeps a floor under `server`, as something kept per server.
    fn keep_something(dir: &Path, server: Fingerprint) {
        let floors = EpochFloors::load(dir, &server).unwrap();
        floors.raise(UserId(4), RoomId(1), Epoch(3));
        floors.save().unwrap();
    }

    fn kept(dir: &Path, server: Fingerprint) -> bool {
        EpochFloors::load(dir, &server)
            .unwrap()
            .get(UserId(4), RoomId(1))
            .is_some()
    }

    #[test]
    fn leaving_a_server_drops_what_is_kept_for_it_unless_another_address_uses_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let (shared, alone) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let mut known = KnownServers::load(dir.path()).unwrap();
        known.trust("a.example.net", shared).unwrap();
        known.trust("b.example.net", shared).unwrap();
        known.trust("c.example.net", alone).unwrap();
        for server in [shared, alone] {
            keep_something(dir.path(), server);
        }

        known.leave("a.example.net").unwrap();
        known.leave("c.example.net").unwrap();
        assert!(kept(dir.path(), shared));
        assert!(!kept(dir.path(), alone));
        known.leave("b.example.net").unwrap();
        assert!(!kept(dir.path(), shared));
        assert_eq!(known.get("b.example.net"), None);
    }

    #[test]
    fn removing_a_server_keeps_what_is_kept_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([1; 32]);
        let mut known = KnownServers::load(dir.path()).unwrap();
        known.trust("a.example.net", server).unwrap();
        keep_something(dir.path(), server);

        known.forget("a.example.net").unwrap();
        assert_eq!(known.get("a.example.net"), None);
        assert_eq!(
            KnownServers::load(dir.path()).unwrap().get("a.example.net"),
            None
        );
        assert!(kept(dir.path(), server));
    }

    #[test]
    fn another_spelling_of_a_pinned_address_finds_its_pin() {
        let dir = tempfile::tempdir().unwrap();
        let mut known = KnownServers::load(dir.path()).unwrap();
        known
            .trust("voice.example.net", Fingerprint([1; 32]))
            .unwrap();
        known.trust("[2001:db8::1]", Fingerprint([2; 32])).unwrap();
        for spelling in [
            "Voice.Example.NET",
            "voice.example.net.",
            &format!("voice.example.net:{DEFAULT_PORT}"),
        ] {
            assert_eq!(
                known.get(spelling),
                Some(Fingerprint([1; 32])),
                "{spelling}"
            );
        }
        assert_eq!(known.get("voice.example.net:9000"), None);
        assert_eq!(known.get("other.example.net"), None);
        let ipv6 = format!("[2001:db8::1]:{DEFAULT_PORT}");
        assert_eq!(known.get(&ipv6), Some(Fingerprint([2; 32])));
        assert_eq!(known.get("2001:db8::1:8476"), None);
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
