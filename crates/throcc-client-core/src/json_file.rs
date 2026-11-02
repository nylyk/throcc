use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::files::{read_if_exists, replace_file};

/// Moves a file's JSON object from one version to the next.
pub type Migration = fn(&mut Map<String, Value>);

/// What a JSON file holds. Version 1 is the format throcc 0.1 shipped; migration `i`
/// moves a file from version `i + 1` to `i + 2`, so changing a format means adding one.
pub trait Format: Serialize + DeserializeOwned + Default {
    const MIGRATIONS: &'static [Migration] = &[];

    fn version() -> u64 {
        Self::MIGRATIONS.len() as u64 + 1
    }
}

/// The contents of the JSON file at `path`, the default where there is none. A file from
/// an older version is migrated in memory; one from a newer version is refused.
pub fn load<T: Format>(path: &Path) -> Result<T> {
    let Some(bytes) = read_if_exists(path)? else {
        return Ok(T::default());
    };
    let read = || -> Result<T> {
        let mut value: Value = serde_json::from_slice(&bytes)?;
        let object = value.as_object_mut().context("not a JSON object")?;
        let version = object
            .remove("version")
            .and_then(|v| v.as_u64())
            .filter(|v| *v >= 1)
            .context("no version")?;
        let current = T::version();
        if version > current {
            bail!("version {version} is newer than this throcc's {current}");
        }
        for migrate in &T::MIGRATIONS[version as usize - 1..] {
            migrate(object);
        }
        Ok(serde_json::from_value(value)?)
    };
    read().with_context(|| format!("reading {}", path.display()))
}

/// Applies `change` to the file at `path` as it is on disk, and writes it back at the
/// current version. Writers in every process take turns, so none undoes another's change.
pub fn update<T: Format>(path: &Path, change: impl FnOnce(&mut T)) -> Result<()> {
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
        version: T::version(),
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

    impl Format for Sample {}

    /// Version 2 of `Sample`, which renamed `names` to `by_id`.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Renamed {
        by_id: BTreeMap<u64, String>,
    }

    impl Format for Renamed {
        const MIGRATIONS: &'static [Migration] = &[|v1| {
            if let Some(names) = v1.remove("names") {
                v1.insert("by_id".into(), names);
            }
        }];
    }

    #[test]
    fn an_older_file_is_migrated_and_written_back_at_the_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("renamed.json");
        fs::write(&path, r#"{"version": 1, "names": {"4": "four"}}"#).unwrap();
        assert_eq!(load::<Renamed>(&path).unwrap().by_id[&4], "four");
        update(&path, |_: &mut Renamed| {}).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 2);
        assert_eq!(written["by_id"]["4"], "four");
    }

    #[test]
    fn changes_load_back_and_a_newer_or_unversioned_file_is_refused() {
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
        fs::write(&path, r#"{"by_id": {}}"#).unwrap();
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
