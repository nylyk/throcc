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

    pub fn trust(&mut self, address: &str, fingerprint: Fingerprint) -> Result<()> {
        self.pins.insert(address.to_owned(), fingerprint);
        Servers::update(&self.dir, |s| {
            s.known.insert(address.to_owned(), Hex(fingerprint.0));
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
    use super::*;

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
