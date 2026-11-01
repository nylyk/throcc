use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::json_file;

const FILE: &str = "settings.json";

/// What this computer keeps for the app as a whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The eight palette colours, as `#RRGGBB`.
    pub palette: Option<Vec<String>>,
    /// The gain on cues, where 1 plays them as recorded.
    pub cue_volume: f32,
    pub profile_name: Option<String>,
    /// The desktop portal's token for sharing the same screen again without asking.
    pub screencast_restore_token: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            palette: None,
            cue_volume: 1.0,
            profile_name: None,
            screencast_restore_token: None,
        }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Result<Self> {
        json_file::load(&dir.join(FILE))
    }

    pub fn update(dir: &Path, change: impl FnOnce(&mut Self)) -> Result<()> {
        json_file::update(&dir.join(FILE), change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_load_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());
        Settings::update(dir.path(), |s| s.cue_volume = 0.4).unwrap();
        Settings::update(dir.path(), |s| s.profile_name = Some("Ilse".into())).unwrap();
        let settings = Settings::load(dir.path()).unwrap();
        assert_eq!(settings.cue_volume, 0.4);
        assert_eq!(settings.profile_name.as_deref(), Some("Ilse"));
    }
}
