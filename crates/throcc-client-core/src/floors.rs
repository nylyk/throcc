use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Epoch, RoomId, UserId};

use crate::lock;
use crate::servers::Servers;

/// The newest epoch we took part in, per user of ours and room, on one server. Key managers
/// share it and it is kept with the server's data, so a server can't bring back an older
/// epoch's sender keys and media after a trip through the lobby, a reconnect or a restart.
/// Raising a floor only marks it unsaved; [`EpochFloors::save`] writes it, so nothing on
/// the media path waits on the file.
#[derive(Clone, Default)]
pub struct EpochFloors(Arc<Mutex<Floors>>);

#[derive(Default)]
struct Floors {
    /// Where they are kept. None keeps them in memory only.
    kept_in: Option<(PathBuf, Fingerprint)>,
    at: HashMap<(UserId, RoomId), Epoch>,
    unsaved: bool,
    /// Set once the last write is done, as before the server's data is removed.
    finished: bool,
}

impl EpochFloors {
    /// The floors kept for `server` in `dir`.
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let at = Servers::load(dir)?
            .take(server)
            .epochs
            .into_iter()
            .flat_map(|(user, rooms)| {
                rooms
                    .into_iter()
                    .map(move |(room, epoch)| ((user, room), epoch))
            })
            .collect();
        Ok(Self(Arc::new(Mutex::new(Floors {
            kept_in: Some((dir.to_owned(), *server)),
            at,
            ..Floors::default()
        }))))
    }

    pub fn get(&self, user: UserId, room: RoomId) -> Option<Epoch> {
        lock(&self.0).at.get(&(user, room)).copied()
    }

    pub fn raise(&self, user: UserId, room: RoomId, epoch: Epoch) {
        let mut floors = lock(&self.0);
        let floor = floors.at.entry((user, room)).or_insert(epoch);
        if *floor < epoch {
            *floor = epoch;
        }
        floors.unsaved = true;
    }

    /// Writes the floors if any rose since the last write. Each kept floor only rises, so
    /// a writer holding older ones never lowers it.
    pub fn save(&self) -> Result<()> {
        let (dir, server, at) = {
            let mut floors = lock(&self.0);
            let Some((dir, server)) = floors.kept_in.clone() else {
                return Ok(());
            };
            if !floors.unsaved || floors.finished {
                return Ok(());
            }
            floors.unsaved = false;
            (dir, server, floors.at.clone())
        };
        let saved = Servers::update(&dir, |s| {
            let kept = &mut s.server_mut(&server).epochs;
            for ((user, room), epoch) in at {
                let floor = kept.entry(user).or_default().entry(room).or_insert(epoch);
                *floor = (*floor).max(epoch);
            }
        });
        if saved.is_err() {
            lock(&self.0).unsaved = true;
        }
        saved
    }

    /// Writes the floors, and none after: the server's data may be about to go, and a later
    /// write would bring it back.
    pub fn save_last(&self) -> Result<()> {
        let saved = self.save();
        lock(&self.0).finished = true;
        saved
    }

    /// Drops the floors kept for `server`, as for a new account there, whose user ids and
    /// rooms are no longer the ones they were for.
    pub fn forget(dir: &Path, server: &Fingerprint) -> Result<()> {
        Servers::update(dir, |s| s.server_mut(server).epochs.clear())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Fingerprint = Fingerprint([7; 32]);
    const ME: UserId = UserId(4);
    const ROOM: RoomId = RoomId(1);

    #[test]
    fn floors_load_back_per_server_and_only_rise() {
        let dir = tempfile::tempdir().unwrap();
        let floors = EpochFloors::load(dir.path(), &SERVER).unwrap();
        floors.raise(ME, ROOM, Epoch(9));
        floors.raise(ME, ROOM, Epoch(3));
        assert_eq!(
            EpochFloors::load(dir.path(), &SERVER)
                .unwrap()
                .get(ME, ROOM),
            None,
            "written before a save"
        );
        floors.save().unwrap();

        let older = EpochFloors::load(dir.path(), &SERVER).unwrap();
        assert_eq!(older.get(ME, ROOM), Some(Epoch(9)));
        floors.raise(ME, ROOM, Epoch(12));
        floors.save().unwrap();
        older.raise(ME, RoomId(2), Epoch(1));
        older.save().unwrap();
        let reloaded = EpochFloors::load(dir.path(), &SERVER).unwrap();
        assert_eq!(
            reloaded.get(ME, ROOM),
            Some(Epoch(12)),
            "a stale writer lowered it"
        );
        assert_eq!(reloaded.get(ME, RoomId(2)), Some(Epoch(1)));
        assert_eq!(reloaded.get(UserId(5), ROOM), None);
        let elsewhere = EpochFloors::load(dir.path(), &Fingerprint([8; 32])).unwrap();
        assert_eq!(elsewhere.get(ME, ROOM), None);
    }

    #[test]
    fn a_failed_save_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let floors = EpochFloors::load(&config, &SERVER).unwrap();
        std::fs::write(&config, b"not a directory yet").unwrap();
        floors.raise(ME, ROOM, Epoch(5));
        assert!(floors.save().is_err());
        std::fs::remove_file(&config).unwrap();
        floors.save().unwrap();
        let reloaded = EpochFloors::load(&config, &SERVER).unwrap();
        assert_eq!(reloaded.get(ME, ROOM), Some(Epoch(5)));
    }

    #[test]
    fn nothing_is_written_after_the_last_save_and_a_new_account_starts_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let floors = EpochFloors::load(dir.path(), &SERVER).unwrap();
        floors.raise(ME, ROOM, Epoch(5));
        floors.save_last().unwrap();
        EpochFloors::forget(dir.path(), &SERVER).unwrap();
        floors.raise(ME, ROOM, Epoch(6));
        floors.save().unwrap();
        let reloaded = EpochFloors::load(dir.path(), &SERVER).unwrap();
        assert_eq!(reloaded.get(ME, ROOM), None);
    }
}
