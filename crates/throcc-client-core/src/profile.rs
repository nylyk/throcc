use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::keystore::replace_file;

const NAME_FILE: &str = "profile_name";
const PICTURE_FILE: &str = "profile_picture";

/// Our name and picture, kept on this computer and given to every server we join.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: Option<String>,
    pub picture: Option<Arc<[u8]>>,
}

impl Profile {
    /// The profile kept in `dir`, empty where nothing was kept.
    pub fn load(dir: &Path) -> Result<Self> {
        let name = read(&dir.join(NAME_FILE))?
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let picture = read(&dir.join(PICTURE_FILE))?.map(Arc::from);
        Ok(Self { name, picture })
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        write(
            &dir.join(NAME_FILE),
            self.name.as_deref().map(str::as_bytes),
        )?;
        write(&dir.join(PICTURE_FILE), self.picture.as_deref())
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => replace_file(path, bytes),
        None => match fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_profile_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), Profile::default());
        let profile = Profile {
            name: Some("Ilse".into()),
            picture: Some(Arc::from(&b"png"[..])),
        };
        profile.save(dir.path()).unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), profile);
        Profile::default().save(dir.path()).unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), Profile::default());
    }
}
