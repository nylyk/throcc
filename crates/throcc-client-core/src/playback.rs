use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;

use crate::keystore::replace_file;
use crate::state::{Playback, Sound};

/// How loud each person's voice and share audio play, kept per server on this computer.
pub struct SavedPlayback {
    path: PathBuf,
    people: BTreeMap<UserId, [Playback; 2]>,
}

impl SavedPlayback {
    pub fn empty(dir: &Path, server: &Fingerprint) -> Self {
        let hex: String = server.0.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            path: dir.join(format!("playback-{hex}")),
            people: BTreeMap::new(),
        }
    }

    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let mut saved = Self::empty(dir, server);
        let text = match fs::read_to_string(&saved.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", saved.path.display())),
        };
        for line in text.lines() {
            let parts: Vec<&str> = line.split(' ').collect();
            let [user, voice, voice_muted, share, share_muted] = parts[..] else {
                continue;
            };
            let playback = |volume: &str, muted: &str| -> Result<Playback> {
                Ok(Playback {
                    volume: volume.parse::<f32>()?.clamp(0.0, 2.0),
                    muted: muted == "1",
                })
            };
            saved.people.insert(
                UserId(user.parse()?),
                [playback(voice, voice_muted)?, playback(share, share_muted)?],
            );
        }
        Ok(saved)
    }

    pub fn get(&self, user: UserId, sound: Sound) -> Playback {
        self.people
            .get(&user)
            .map_or_else(Playback::default, |p| p[index(sound)])
    }

    /// Both sounds of everyone saved.
    pub fn iter(&self) -> impl Iterator<Item = (UserId, Sound, Playback)> + '_ {
        self.people.iter().flat_map(|(user, p)| {
            [Sound::Voice, Sound::Share].map(|sound| (*user, sound, p[index(sound)]))
        })
    }

    pub fn set(&mut self, user: UserId, sound: Sound, playback: Playback) -> Result<()> {
        let mut both = self
            .people
            .get(&user)
            .copied()
            .unwrap_or([Playback::default(); 2]);
        both[index(sound)] = playback;
        if both == [Playback::default(); 2] {
            self.people.remove(&user);
        } else {
            self.people.insert(user, both);
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let text: String = self
            .people
            .iter()
            .map(|(user, [voice, share])| {
                format!(
                    "{} {} {} {} {}\n",
                    user.0,
                    voice.volume,
                    u8::from(voice.muted),
                    share.volume,
                    u8::from(share.muted)
                )
            })
            .collect();
        replace_file(&self.path, text.as_bytes())
    }
}

fn index(sound: Sound) -> usize {
    match sound {
        Sound::Voice => 0,
        Sound::Share => 1,
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
