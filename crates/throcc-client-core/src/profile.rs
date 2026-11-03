use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use throcc_proto::ids::Hash;
use throcc_proto::messages::valid_name;

use crate::files::{read_if_exists, remove_if_exists, replace_file};
use crate::settings::Settings;

const PICTURE_FILE: &str = "profile_picture";

/// Our name and picture, kept on this computer and given to every server we join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub picture: Option<Arc<[u8]>>,
}

impl Profile {
    /// The profile kept in `dir`, or `None` until a valid name is kept there.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let Some(name) = Settings::load(dir)?
            .profile_name
            .as_deref()
            .and_then(valid_name)
            .map(str::to_owned)
        else {
            return Ok(None);
        };
        let picture = read_if_exists(&dir.join(PICTURE_FILE))?.map(Arc::from);
        Ok(Some(Self { name, picture }))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        Settings::update(dir, |s| s.profile_name = Some(self.name.clone()))?;
        let path = dir.join(PICTURE_FILE);
        match &self.picture {
            Some(bytes) => replace_file(&path, bytes),
            None => remove_if_exists(&path),
        }
    }

    pub fn avatar(&self) -> Option<Hash> {
        self.picture.as_deref().map(Hash::of)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_profile_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), None);
        let mut profile = Profile {
            name: "Ilse".into(),
            picture: Some(Arc::from(&b"png"[..])),
        };
        profile.save(dir.path()).unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), Some(profile.clone()));
        profile.picture = None;
        profile.save(dir.path()).unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), Some(profile));
    }

    #[test]
    fn a_kept_name_the_server_would_refuse_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        Settings::update(dir.path(), |s| s.profile_name = Some("   ".into())).unwrap();
        assert_eq!(Profile::load(dir.path()).unwrap(), None);
    }
}
