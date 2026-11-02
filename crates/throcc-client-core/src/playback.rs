use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use tracing::warn;

use crate::servers::Servers;
use crate::state::{MAX_VOLUME, PersonPlayback, Playback, Sound};

/// How loud each person's voice and share audio play, kept per server on this computer.
/// Changes reach the file on [`SavedPlayback::save`] and on drop, not at every step of a
/// dragged slider.
pub struct SavedPlayback {
    dir: PathBuf,
    server: Fingerprint,
    people: BTreeMap<UserId, PersonPlayback>,
    unsaved: BTreeSet<UserId>,
}

impl SavedPlayback {
    pub fn empty(dir: &Path, server: &Fingerprint) -> Self {
        Self {
            dir: dir.to_owned(),
            server: *server,
            people: BTreeMap::new(),
            unsaved: BTreeSet::new(),
        }
    }

    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let mut saved = Self::empty(dir, server);
        saved.people = Servers::load(dir)?.take(server).playback;
        for person in saved.people.values_mut() {
            for sound in Sound::ALL {
                let playback = person.get_mut(sound);
                playback.volume = playback.volume.clamp(0.0, MAX_VOLUME);
            }
        }
        Ok(saved)
    }

    pub fn person(&self, user: UserId) -> PersonPlayback {
        self.people.get(&user).copied().unwrap_or_default()
    }

    /// Both sounds of everyone saved.
    pub fn iter(&self) -> impl Iterator<Item = (UserId, Sound, Playback)> + '_ {
        self.people
            .iter()
            .flat_map(|(user, p)| Sound::ALL.map(|sound| (*user, sound, p.get(sound))))
    }

    pub fn set(&mut self, user: UserId, sound: Sound, playback: Playback) {
        let mut person = self.person(user);
        *person.get_mut(sound) = playback;
        if person == PersonPlayback::default() {
            self.people.remove(&user);
        } else {
            self.people.insert(user, person);
        }
        self.unsaved.insert(user);
    }

    /// Writes the changes made since the last save. Changes a failed save missed stay for
    /// the next one.
    pub fn save(&mut self) -> Result<()> {
        if self.unsaved.is_empty() {
            return Ok(());
        }
        Servers::update(&self.dir, |s| {
            let kept = &mut s.server_mut(&self.server).playback;
            for user in &self.unsaved {
                match self.people.get(user) {
                    Some(person) => kept.insert(*user, *person),
                    None => kept.remove(user),
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

    #[test]
    fn saved_playback_loads_back_per_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        let quiet = Playback {
            volume: 0.35,
            muted: false,
        };
        let muted = Playback {
            volume: 1.5,
            muted: true,
        };
        let mut saved = SavedPlayback::load(dir.path(), &server).unwrap();
        saved.set(UserId(4), Sound::Voice, quiet);
        saved.set(UserId(4), Sound::Share, muted);
        assert_eq!(
            SavedPlayback::load(dir.path(), &server)
                .unwrap()
                .person(UserId(4)),
            PersonPlayback::default(),
            "nothing is written before a save"
        );
        saved.save().unwrap();

        let saved = SavedPlayback::load(dir.path(), &server).unwrap();
        let expected = PersonPlayback {
            voice: quiet,
            share: muted,
        };
        assert_eq!(saved.person(UserId(4)), expected);
        assert_eq!(saved.person(UserId(5)), PersonPlayback::default());
        let other = SavedPlayback::load(dir.path(), &Fingerprint([8; 32])).unwrap();
        assert_eq!(other.person(UserId(4)), PersonPlayback::default());
    }

    #[test]
    fn a_failed_save_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, b"not a directory yet").unwrap();
        let server = Fingerprint([7; 32]);
        let mut saved = SavedPlayback::empty(&config, &server);
        let quiet = Playback {
            volume: 0.3,
            muted: false,
        };
        saved.set(UserId(4), Sound::Voice, quiet);
        assert!(saved.save().is_err());

        std::fs::remove_file(&config).unwrap();
        saved.save().unwrap();
        let loaded = SavedPlayback::load(&config, &server).unwrap();
        assert_eq!(loaded.person(UserId(4)).voice, quiet);
    }

    #[test]
    fn back_to_default_forgets_the_person() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        let mut saved = SavedPlayback::load(dir.path(), &server).unwrap();
        let quiet = Playback {
            volume: 0.5,
            muted: false,
        };
        saved.set(UserId(4), Sound::Voice, quiet);
        saved.save().unwrap();
        saved.set(UserId(4), Sound::Voice, Playback::default());
        assert_eq!(saved.iter().count(), 0);
        drop(saved);
        let saved = SavedPlayback::load(dir.path(), &server).unwrap();
        assert_eq!(saved.iter().count(), 0, "a drop saves what changed");
    }
}
