use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;

use crate::keystore::replace_file;

/// Pinned server keys by address, stored one `address fingerprint` per line.
pub struct KnownServers {
    path: PathBuf,
    pins: BTreeMap<String, Fingerprint>,
}

impl KnownServers {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut pins = BTreeMap::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let (address, hex) = line
                .split_once(' ')
                .with_context(|| format!("bad line in {}: {line}", path.display()))?;
            pins.insert(address.to_owned(), hex.parse()?);
        }
        Ok(Self {
            path: path.to_owned(),
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
        self.save()
    }

    pub fn forget(&mut self, address: &str) -> Result<()> {
        self.pins.remove(address);
        self.save()
    }

    fn save(&self) -> Result<()> {
        let text: String = self
            .pins
            .iter()
            .map(|(address, fp)| format!("{address} {}\n", fp.to_string().replace(' ', "")))
            .collect();
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        replace_file(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_servers");
        let fp = Fingerprint::of_spki(b"key");
        KnownServers::load(&path)
            .unwrap()
            .trust("voice.example.net", fp)
            .unwrap();
        let reloaded = KnownServers::load(&path).unwrap();
        assert_eq!(reloaded.get("voice.example.net"), Some(fp));
        assert_eq!(reloaded.get("other"), None);
    }
}
