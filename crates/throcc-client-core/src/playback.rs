use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use throcc_proto::messages::PublicKey;
use tracing::warn;

use crate::people::People;
use crate::servers::Hex;
use crate::state::{MAX_VOLUME, PersonPlayback, Playback, Sound};

/// How loud each person's voice and share audio play, by identity key, on every server.
/// Changes reach the file on [`SavedPlayback::save`] and on drop, not at every step of a
/// dragged slider.
pub struct SavedPlayback {
    dir: PathBuf,
    people: BTreeMap<PublicKey, PersonPlayback>,
    unsaved: BTreeSet<PublicKey>,
}

impl SavedPlayback {
    pub fn empty(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            people: BTreeMap::new(),
            unsaved: BTreeSet::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let mut saved = Self::empty(dir);
        saved.people = People::load(dir)?
            .playback
            .into_iter()
            .map(|(Hex(key), person)| (key, person))
            .collect();
        for person in saved.people.values_mut() {
            for sound in Sound::ALL {
                let playback = person.get_mut(sound);
                playback.volume = playback.volume.clamp(0.0, MAX_VOLUME);
            }
        }
        Ok(saved)
    }

    pub fn person(&self, key: &PublicKey) -> PersonPlayback {
        self.people.get(key).copied().unwrap_or_default()
    }

    pub fn set(&mut self, key: &PublicKey, sound: Sound, playback: Playback) {
        let mut person = self.person(key);
        *person.get_mut(sound) = playback;
        if person == PersonPlayback::default() {
            self.people.remove(key);
        } else {
            self.people.insert(*key, person);
        }
        self.unsaved.insert(*key);
    }

    /// Writes the changes made since the last save. Changes a failed save missed stay for
    /// the next one.
    pub fn save(&mut self) -> Result<()> {
        if self.unsaved.is_empty() {
            return Ok(());
        }
        People::update(&self.dir, |people| {
            for key in &self.unsaved {
                match self.people.get(key) {
                    Some(person) => people.playback.insert(Hex(*key), *person),
                    None => people.playback.remove(&Hex(*key)),
                };
            }
        })?;
        self.unsaved.clear();
        Ok(())
    }
}

impl Drop for SavedPlayback {
    fn drop(&mut self) {
        if let Err(e) = self.save() {
            warn!("saving playback: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARA: PublicKey = [4; 32];
    const TEO: PublicKey = [5; 32];

    #[test]
    fn saved_playback_loads_back_by_identity_key() {
        let dir = tempfile::tempdir().unwrap();
        let quiet = Playback {
            volume: 0.35,
            muted: false,
        };
        let muted = Playback {
            volume: 1.5,
            muted: true,
        };
        let mut saved = SavedPlayback::load(dir.path()).unwrap();
        saved.set(&MARA, Sound::Voice, quiet);
        saved.set(&MARA, Sound::Share, muted);
        assert_eq!(
            SavedPlayback::load(dir.path()).unwrap().person(&MARA),
            PersonPlayback::default(),
            "nothing is written before a save"
        );
        saved.save().unwrap();

        let saved = SavedPlayback::load(dir.path()).unwrap();
        let expected = PersonPlayback {
            voice: quiet,
            share: muted,
        };
        assert_eq!(saved.person(&MARA), expected);
        assert_eq!(saved.person(&TEO), PersonPlayback::default());
    }

    #[test]
    fn saving_keeps_who_is_verified() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = crate::verified::VerifiedKeys::load(dir.path()).unwrap();
        let server = throcc_proto::Fingerprint([1; 32]);
        let user = throcc_proto::ids::UserId(4);
        verified.verify(&server, user, &MARA, "Mara").unwrap();
        let mut saved = SavedPlayback::load(dir.path()).unwrap();
        let quiet = Playback {
            volume: 0.3,
            muted: false,
        };
        saved.set(&MARA, Sound::Voice, quiet);
        saved.save().unwrap();
        let reloaded = crate::verified::VerifiedKeys::load(dir.path()).unwrap();
        assert!(reloaded.is_verified(&MARA));
        assert_eq!(
            SavedPlayback::load(dir.path()).unwrap().person(&MARA).voice,
            quiet
        );
    }

    #[test]
    fn a_failed_save_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, b"not a directory yet").unwrap();
        let mut saved = SavedPlayback::empty(&config);
        let quiet = Playback {
            volume: 0.3,
            muted: false,
        };
        saved.set(&MARA, Sound::Voice, quiet);
        assert!(saved.save().is_err());

        std::fs::remove_file(&config).unwrap();
        saved.save().unwrap();
        let loaded = SavedPlayback::load(&config).unwrap();
        assert_eq!(loaded.person(&MARA).voice, quiet);
    }

    #[test]
    fn back_to_default_forgets_the_person() {
        let dir = tempfile::tempdir().unwrap();
        let mut saved = SavedPlayback::load(dir.path()).unwrap();
        let quiet = Playback {
            volume: 0.5,
            muted: false,
        };
        saved.set(&MARA, Sound::Voice, quiet);
        saved.save().unwrap();
        saved.set(&MARA, Sound::Voice, Playback::default());
        assert!(saved.people.is_empty());
        drop(saved);
        let saved = SavedPlayback::load(dir.path()).unwrap();
        assert!(saved.people.is_empty(), "a drop saves what changed");
    }
}
