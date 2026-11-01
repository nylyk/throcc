use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;

use crate::servers::Servers;
use crate::state::{MAX_VOLUME, Playback, Sound};

/// How loud one person's voice and share audio play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonPlayback {
    pub voice: Playback,
    pub share: Playback,
}

impl PersonPlayback {
    fn get(&self, sound: Sound) -> Playback {
        match sound {
            Sound::Voice => self.voice,
            Sound::Share => self.share,
        }
    }

    fn get_mut(&mut self, sound: Sound) -> &mut Playback {
        match sound {
            Sound::Voice => &mut self.voice,
            Sound::Share => &mut self.share,
        }
    }
}

/// How loud each person's voice and share audio play, kept per server on this computer.
pub struct SavedPlayback {
    dir: PathBuf,
    server: Fingerprint,
    people: BTreeMap<UserId, PersonPlayback>,
}

impl SavedPlayback {
    pub fn empty(dir: &Path, server: &Fingerprint) -> Self {
        Self {
            dir: dir.to_owned(),
            server: *server,
            people: BTreeMap::new(),
        }
    }

    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let mut saved = Self::empty(dir, server);
        saved.people = Servers::load(dir)?.take(server).playback;
        for person in saved.people.values_mut() {
            for sound in [Sound::Voice, Sound::Share] {
                let playback = person.get_mut(sound);
                playback.volume = playback.volume.clamp(0.0, MAX_VOLUME);
            }
        }
        Ok(saved)
    }

    pub fn get(&self, user: UserId, sound: Sound) -> Playback {
        self.people
            .get(&user)
            .map_or_else(Playback::default, |p| p.get(sound))
    }

    /// Both sounds of everyone saved.
    pub fn iter(&self) -> impl Iterator<Item = (UserId, Sound, Playback)> + '_ {
        self.people.iter().flat_map(|(user, p)| {
            [Sound::Voice, Sound::Share].map(|sound| (*user, sound, p.get(sound)))
        })
    }

    pub fn set(&mut self, user: UserId, sound: Sound, playback: Playback) -> Result<()> {
        let mut person = self.people.get(&user).copied().unwrap_or_default();
        *person.get_mut(sound) = playback;
        let kept = (person != PersonPlayback::default()).then_some(person);
        match kept {
            Some(person) => self.people.insert(user, person),
            None => self.people.remove(&user),
        };
        Servers::update(&self.dir, |s| {
            let people = &mut s.server_mut(&self.server).playback;
            match kept {
                Some(person) => people.insert(user, person),
                None => people.remove(&user),
            };
        })
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
        saved.set(UserId(4), Sound::Voice, quiet).unwrap();
        saved.set(UserId(4), Sound::Share, muted).unwrap();

        let saved = SavedPlayback::load(dir.path(), &server).unwrap();
        assert_eq!(saved.get(UserId(4), Sound::Voice), quiet);
        assert_eq!(saved.get(UserId(4), Sound::Share), muted);
        assert_eq!(saved.get(UserId(5), Sound::Voice), Playback::default());
        let other = SavedPlayback::load(dir.path(), &Fingerprint([8; 32])).unwrap();
        assert_eq!(other.get(UserId(4), Sound::Voice), Playback::default());
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
        saved.set(UserId(4), Sound::Voice, quiet).unwrap();
        saved
            .set(UserId(4), Sound::Voice, Playback::default())
            .unwrap();
        assert_eq!(saved.iter().count(), 0);
    }
}
