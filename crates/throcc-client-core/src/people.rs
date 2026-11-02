use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::json_file;
use crate::servers::Hex;
use crate::state::PersonPlayback;
use crate::verified::VerifiedRecords;

pub(crate) const FILE: &str = "people.json";

/// What this computer keeps per person, by identity key, for every server.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct People {
    #[serde(flatten)]
    pub verified: VerifiedRecords,
    pub playback: BTreeMap<Hex, PersonPlayback>,
}

impl json_file::Format for People {}

impl People {
    pub fn load(dir: &Path) -> Result<Self> {
        json_file::load(&dir.join(FILE))
    }

    pub fn update(dir: &Path, change: impl FnOnce(&mut Self)) -> Result<()> {
        json_file::update(&dir.join(FILE), change)
    }
}
