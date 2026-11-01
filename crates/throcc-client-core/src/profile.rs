use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::keystore::replace_file;
use crate::settings::Settings;

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
        let name = Settings::load(dir)?
            .profile_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let path = dir.join(PICTURE_FILE);
        let picture = match fs::read(&path) {
            Ok(bytes) => Some(bytes.into()),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { name, picture })
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        Settings::update(dir, |s| s.profile_name.clone_from(&self.name))?;
        let path = dir.join(PICTURE_FILE);
        match &self.picture {
            Some(bytes) => replace_file(&path, bytes),
            None => match fs::remove_file(&path) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", path.display()))
                }
                _ => Ok(()),
            },
        }
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
