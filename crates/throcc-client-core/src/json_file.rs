use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::files::{read_if_exists, replace_file};

const VERSION: u64 = 1;

/// The contents of the JSON file at `path`, the default where there is none.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let Some(bytes) = read_if_exists(path)? else {
        return Ok(T::default());
    };
    let read = || -> Result<T> {
        let mut value: Value = serde_json::from_slice(&bytes)?;
        let version = value.as_object_mut().and_then(|o| o.remove("version"));
        if version.as_ref().and_then(Value::as_u64) != Some(VERSION) {
            bail!("not version {VERSION}");
        }
        Ok(serde_json::from_value(value)?)
    };
    read().with_context(|| format!("reading {}", path.display()))
}

/// Applies `change` to the file at `path` as it is on disk, and writes it back. Writers in
/// every process take turns, so none undoes another's change.
pub fn update<T: Serialize + DeserializeOwned + Default>(
    path: &Path,
    change: impl FnOnce(&mut T),
) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock_path = path.with_extension("lock");
    let lock =
        fs::File::create(&lock_path).with_context(|| format!("opening {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("locking {}", lock_path.display()))?;
    let mut contents = load(path)?;
    change(&mut contents);
    let file = Versioned {
        version: VERSION,
        contents: &contents,
    };
    replace_file(path, &serde_json::to_vec_pretty(&file)?)
}

#[derive(Serialize)]
struct Versioned<'a, T> {
    version: u64,
    #[serde(flatten)]
    contents: &'a T,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        by_id: BTreeMap<u64, String>,
    }

    #[test]
    fn changes_load_back_and_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        assert_eq!(load::<Sample>(&path).unwrap(), Sample::default());
        update(&path, |s: &mut Sample| {
            s.by_id.insert(4, "four".into());
        })
        .unwrap();
        assert_eq!(load::<Sample>(&path).unwrap().by_id[&4], "four");

        fs::write(&path, r#"{"version": 2, "by_id": {}}"#).unwrap();
        assert!(load::<Sample>(&path).is_err());
    }

    #[test]
    fn concurrent_writers_keep_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        std::thread::scope(|scope| {
            for writer in 0..4u64 {
                let path = &path;
                scope.spawn(move || {
                    for i in 0..25 {
                        update(path, |s: &mut Sample| {
                            s.by_id.insert(writer * 100 + i, String::new());
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(load::<Sample>(&path).unwrap().by_id.len(), 100);
    }
}
