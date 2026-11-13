use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::blocked::BlockedRecords;
use crate::json_file;
use crate::servers::Hex;
use crate::state::Playback;
use crate::verified::VerifiedRecords;

pub(crate) const FILE: &str = "people.json";

/// What this computer keeps per person, by identity key, for every server.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct People {
    #[serde(flatten)]
    pub verified: VerifiedRecords,
    pub playback: BTreeMap<Hex, Playback>,
    pub blocked: BlockedRecords,
}

impl json_file::Format for People {
    const MIGRATIONS: &'static [json_file::Migration] = &[add_blocked];
}

/// Moves version 1 to 2, which has the blocked people. The new version keeps a throcc that
/// knows no blocks from writing the file back without them.
fn add_blocked(v1: &mut Map<String, Value>) {
    v1.entry("blocked").or_insert_with(|| Map::new().into());
}

impl People {
    pub fn load(dir: &Path) -> Result<Self> {
        json_file::load(&dir.join(FILE))
    }

    pub fn update(dir: &Path, change: impl FnOnce(&mut Self)) -> Result<()> {
        json_file::update(&dir.join(FILE), change)
    }
}
