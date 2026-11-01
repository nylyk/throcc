use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

use crate::keystore::replace_file;
use crate::state::MAX_VOLUME;

const FILE: &str = "cue-volume";

/// The gain on cues kept in `dir`, 1 where none was kept.
pub fn load(dir: &Path) -> Result<f32> {
    let path = dir.join(FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text.trim().parse::<f32>()?.clamp(0.0, MAX_VOLUME)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(1.0),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(dir: &Path, volume: f32) -> Result<()> {
    fs::create_dir_all(dir)?;
    replace_file(&dir.join(FILE), format!("{volume}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_cue_volume_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), 1.0);
        save(dir.path(), 0.4).unwrap();
        assert_eq!(load(dir.path()).unwrap(), 0.4);
    }
}
