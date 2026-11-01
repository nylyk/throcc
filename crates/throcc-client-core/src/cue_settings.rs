use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

use crate::keystore::replace_file;
use crate::state::{MAX_VOLUME, Playback};

const FILE: &str = "cues";

/// A group of cues that share a volume and a mute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueCategory {
    People,
    Microphone,
    Connection,
}

impl CueCategory {
    pub const ALL: [CueCategory; 3] = [
        CueCategory::People,
        CueCategory::Microphone,
        CueCategory::Connection,
    ];

    fn key(self) -> &'static str {
        match self {
            CueCategory::People => "people",
            CueCategory::Microphone => "microphone",
            CueCategory::Connection => "connection",
        }
    }
}

/// How loud each category of cues plays, kept on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CueSettings([Playback; 3]);

impl CueSettings {
    pub fn get(&self, category: CueCategory) -> Playback {
        self.0[category as usize]
    }

    pub fn set(&mut self, category: CueCategory, playback: Playback) {
        self.0[category as usize] = playback;
    }

    /// The settings kept in `dir`, defaults where nothing was kept.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut settings = Self::default();
        for line in text.lines() {
            let parts: Vec<&str> = line.split(' ').collect();
            let [key, volume, muted] = parts[..] else {
                continue;
            };
            let Some(category) = CueCategory::ALL.into_iter().find(|c| c.key() == key) else {
                continue;
            };
            settings.set(
                category,
                Playback {
                    volume: volume.parse::<f32>()?.clamp(0.0, MAX_VOLUME),
                    muted: muted == "1",
                },
            );
        }
        Ok(settings)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let text: String = CueCategory::ALL
            .iter()
            .map(|category| {
                let playback = self.get(*category);
                format!(
                    "{} {} {}\n",
                    category.key(),
                    playback.volume,
                    u8::from(playback.muted)
                )
            })
            .collect();
        replace_file(&dir.join(FILE), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_cue_settings_load_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            CueSettings::load(dir.path()).unwrap(),
            CueSettings::default()
        );
        let mut settings = CueSettings::default();
        settings.set(
            CueCategory::People,
            Playback {
                volume: 0.4,
                muted: false,
            },
        );
        settings.set(
            CueCategory::Connection,
            Playback {
                volume: 1.0,
                muted: true,
            },
        );
        settings.save(dir.path()).unwrap();
        assert_eq!(CueSettings::load(dir.path()).unwrap(), settings);
    }
}
