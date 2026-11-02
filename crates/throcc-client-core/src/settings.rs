use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::json_file;

const FILE: &str = "settings.json";

/// What this computer keeps for the app as a whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The picked theme's name: a preset's, or `Custom`.
    pub theme: Option<String>,
    /// The Custom theme's eight colours, as `#RRGGBB`.
    pub palette: Option<Vec<String>>,
    /// The gain on cues, where 1 plays them as recorded.
    pub cue_volume: f32,
    pub profile_name: Option<String>,
    /// The sidebar's opacity from 0.5 to 1, unset for the platform's default.
    pub sidebar_opacity: Option<f32>,
}

impl json_file::Format for Settings {}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: None,
            palette: None,
            cue_volume: 1.0,
            profile_name: None,
            sidebar_opacity: None,
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
