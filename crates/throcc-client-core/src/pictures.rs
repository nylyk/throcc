use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::keystore::replace_file;

/// The picture kept under `name`, if there is one.
pub fn load(dir: &Path, name: &str) -> Result<Option<Arc<[u8]>>> {
    let path = path(dir, name);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes.into())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Keeps `bytes` under `name`, or removes the picture there when there are none.
pub fn save(dir: &Path, name: &str, bytes: Option<&[u8]>) -> Result<()> {
    let path = path(dir, name);
    match bytes {
        Some(bytes) => {
            fs::create_dir_all(dir.join("pictures"))?;
            replace_file(&path, bytes)
        }
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        },
    }
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join("pictures").join(name)
}
