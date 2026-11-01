use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::pictures;
use crate::settings::Settings;

const PICTURE: &str = "profile";

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
        let picture = pictures::load(dir, PICTURE)?;
        Ok(Self { name, picture })
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        Settings::update(dir, |s| s.profile_name.clone_from(&self.name))?;
        pictures::save(dir, PICTURE, self.picture.as_deref())
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
