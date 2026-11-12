use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::Hash;
use throcc_proto::messages::PublicKey;

use crate::lock;
use crate::people::People;
use crate::servers::Hex;
use crate::state::BlockedPerson;

/// Someone blocked, as kept in [`People`] under their identity key.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BlockedRecord {
    name: String,
    avatar: Option<Hex>,
    server: Option<Hex>,
}

pub(crate) type BlockedRecords = BTreeMap<Hex, BlockedRecord>;

struct Blocked {
    /// Where the file is kept. None keeps the list in memory only.
    dir: Option<PathBuf>,
    records: BlockedRecords,
}

/// The people blocked on this install, shared by the app and every session it starts. A
/// block follows an identity key, so it holds on every server that shows the key.
#[derive(Clone)]
pub struct BlockedStore(Arc<Mutex<Blocked>>);

impl BlockedStore {
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self::of(Some(dir), People::load(dir)?.blocked))
    }

    /// Nobody blocked, kept in `dir`, for when the file there can't be read. Every change
    /// re-reads it first, so a file that can't be read is never overwritten.
    pub fn empty(dir: &Path) -> Self {
        Self::of(Some(dir), BlockedRecords::new())
    }

    /// Nobody blocked, kept in memory only.
    pub fn in_memory() -> Self {
        Self::of(None, BlockedRecords::new())
    }

    pub fn list(&self) -> Vec<BlockedPerson> {
        let blocked = lock(&self.0);
        let people = blocked.records.iter().map(|(key, record)| BlockedPerson {
            key: Fingerprint(key.0),
            name: record.name.clone(),
            avatar: record.avatar.map(|Hex(hash)| Hash(hash)),
            server: record.server.map(|Hex(server)| Fingerprint(server)),
        });
        people.collect()
    }

    pub fn is_blocked(&self, key: &PublicKey) -> bool {
        lock(&self.0).records.contains_key(&Hex(*key))
    }

    /// Blocks `person`, keeping how they look for the list.
    pub fn block(&self, person: BlockedPerson) -> Result<()> {
        let record = BlockedRecord {
            name: person.name,
            avatar: person.avatar.map(|hash| Hex(hash.0)),
            server: person.server.map(|server| Hex(server.0)),
        };
        self.change(|records| {
            records.insert(Hex(person.key.0), record);
        })
    }

    pub fn unblock(&self, key: &Fingerprint) -> Result<()> {
        self.change(|records| {
            records.remove(&Hex(key.0));
        })
    }

    /// Applies `change` to the file as it is on disk, then takes what the file holds, which
    /// includes changes other processes made.
    fn change(&self, change: impl FnOnce(&mut BlockedRecords)) -> Result<()> {
        let mut blocked = lock(&self.0);
        match blocked.dir.clone() {
            Some(dir) => {
                let mut written = None;
                People::update(&dir, |people| {
                    change(&mut people.blocked);
                    written = Some(people.blocked.clone());
                })?;
                blocked.records = written.unwrap_or_default();
            }
            None => change(&mut blocked.records),
        }
        Ok(())
    }

    fn of(dir: Option<&Path>, records: BlockedRecords) -> Self {
        Self(Arc::new(Mutex::new(Blocked {
            dir: dir.map(Path::to_owned),
            records,
        })))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;
    use crate::people::FILE;

    fn mara() -> BlockedPerson {
        BlockedPerson {
            key: Fingerprint([4; 32]),
            name: "Mara".into(),
            avatar: Some(Hash([6; 32])),
            server: Some(Fingerprint([1; 32])),
        }
    }

    #[test]
    fn a_block_is_kept_by_identity_key_with_how_they_looked() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlockedStore::load(dir.path()).unwrap();
        assert!(!store.is_blocked(&[4; 32]));
        store.block(mara()).unwrap();
        assert!(store.is_blocked(&[4; 32]));

        let reloaded = BlockedStore::load(dir.path()).unwrap();
        assert_eq!(reloaded.list(), vec![mara()]);
        assert!(!reloaded.is_blocked(&[5; 32]));

        reloaded.unblock(&Fingerprint([4; 32])).unwrap();
        assert!(BlockedStore::load(dir.path()).unwrap().list().is_empty());
    }

    #[test]
    fn a_file_from_0_1_gains_an_empty_blocked_list_and_moves_to_version_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        fs::write(
            &path,
            r#"{"version": 1, "verified": {}, "changed": [], "playback": {}}"#,
        )
        .unwrap();
        let store = BlockedStore::load(dir.path()).unwrap();
        assert!(store.list().is_empty());

        store.block(mara()).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
        assert_eq!(written["blocked"].as_object().unwrap().len(), 1);
        assert!(written["verified"].is_object());
    }
}
