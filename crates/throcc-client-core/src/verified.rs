use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::PublicKey;

use crate::json_file;
use crate::servers::Hex;
use crate::state::VerifiedIdentity;

const FILE: &str = "verified.json";
/// How many server and user pairs are kept per verified key. Each one takes a signature
/// by that key, so only someone with many accounts comes near it; past it, more aren't
/// noted, which only means a key change there goes without a warning.
const MAX_SEEN: usize = 256;

/// The identity keys the user verified, for this whole install: a key verified once is
/// verified on every server that shows it. Each key also keeps the server and user pairs it
/// was seen under, so a server later showing another key for one of them is noticed.
/// Every change is written at once.
pub(crate) struct VerifiedKeys {
    dir: PathBuf,
    file: VerifiedFile,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct VerifiedFile {
    people: BTreeMap<Hex, Verified>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Verified {
    /// The name they were shown with when verified, to tell them apart in the list.
    name: String,
    seen: BTreeSet<SeenAs>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct SeenAs {
    server: Hex,
    user: UserId,
}

impl VerifiedFile {
    /// Notes that `key` was seen as `seen`, where no other verified key was seen since.
    fn see(&mut self, key: &PublicKey, seen: SeenAs) {
        self.forget_seen(seen, Some(key));
        if let Some(verified) = self.people.get_mut(&Hex(*key))
            && verified.seen.len() < MAX_SEEN
        {
            verified.seen.insert(seen);
        }
    }

    /// Forgets `seen` on every verified key but `except`.
    fn forget_seen(&mut self, seen: SeenAs, except: Option<&PublicKey>) {
        for (key, verified) in &mut self.people {
            if Some(&key.0) != except {
                verified.seen.remove(&seen);
            }
        }
    }
}

impl VerifiedKeys {
    /// None verified, kept in `dir`, for when the file there can't be read. Every change
    /// re-reads it first, so a file that can't be read is never overwritten.
    pub fn empty(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            file: VerifiedFile::default(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            dir: dir.to_owned(),
            file: json_file::load(&dir.join(FILE))?,
        })
    }

    pub fn is_verified(&self, key: &PublicKey) -> bool {
        self.file.people.contains_key(&Hex(*key))
    }

    /// The verified key last seen as `user` on `server`.
    pub fn verified_at(&self, server: &Fingerprint, user: UserId) -> Option<PublicKey> {
        let seen = seen_as(server, user);
        self.file
            .people
            .iter()
            .find(|(_, verified)| verified.seen.contains(&seen))
            .map(|(key, _)| key.0)
    }

    /// Notes that `server` shows `key`, validly signed, as `user`. Written only for a
    /// verified key that wasn't noted there yet.
    pub fn seen(&mut self, server: &Fingerprint, user: UserId, key: &PublicKey) -> Result<()> {
        let seen = seen_as(server, user);
        let noted = self.verified_at(server, user) == Some(*key);
        if noted || !self.is_verified(key) {
            return Ok(());
        }
        self.change(|file| file.see(key, seen))
    }

    /// Verifies `key`, which `server` shows as `user` under `name`.
    pub fn verify(
        &mut self,
        server: &Fingerprint,
        user: UserId,
        key: &PublicKey,
        name: &str,
    ) -> Result<()> {
        self.change(|file| {
            file.people.entry(Hex(*key)).or_default().name = name.to_owned();
            file.see(key, seen_as(server, user));
        })
    }

    /// Takes `key` off the verified ones, on every server.
    pub fn unverify(&mut self, key: &PublicKey) -> Result<()> {
        self.change(|file| {
            file.people.remove(&Hex(*key));
        })
    }

    /// Stops expecting the verified key last seen as `user` on `server` there, after the
    /// user accepted that it changed.
    pub fn dismiss_change(&mut self, server: &Fingerprint, user: UserId) -> Result<()> {
        self.change(|file| file.forget_seen(seen_as(server, user), None))
    }

    /// Every verified key, with the servers it was seen on.
    pub fn list(&self) -> Vec<VerifiedIdentity> {
        self.file
            .people
            .iter()
            .map(|(Hex(key), verified)| VerifiedIdentity {
                key: Fingerprint(*key),
                name: verified.name.clone(),
                servers: verified
                    .seen
                    .iter()
                    .map(|seen| seen.server)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(|Hex(server)| Fingerprint(server))
                    .collect(),
            })
            .collect()
    }

    /// Applies `change` to the file as it is on disk, then takes what it now holds, which
    /// includes changes other processes made.
    fn change(&mut self, change: impl FnOnce(&mut VerifiedFile)) -> Result<()> {
        let mut written = None;
        json_file::update(&self.dir.join(FILE), |file: &mut VerifiedFile| {
            change(file);
            written = Some(file.clone());
        })?;
        self.file = written.unwrap_or_default();
        Ok(())
    }
}

fn seen_as(server: &Fingerprint, user: UserId) -> SeenAs {
    SeenAs {
        server: Hex(server.0),
        user,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: Fingerprint = Fingerprint([1; 32]);
    const THERE: Fingerprint = Fingerprint([2; 32]);

    #[test]
    fn a_verified_key_is_verified_on_every_server_and_after_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert!(reloaded.is_verified(&[4; 32]));
        assert!(!reloaded.is_verified(&[5; 32]));
        assert_eq!(reloaded.verified_at(&HERE, UserId(4)), Some([4; 32]));
        assert_eq!(reloaded.verified_at(&THERE, UserId(9)), None);

        verified.seen(&THERE, UserId(9), &[4; 32]).unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert_eq!(reloaded.verified_at(&THERE, UserId(9)), Some([4; 32]));
        assert_eq!(reloaded.list()[0].servers, [HERE, THERE]);
        assert_eq!(reloaded.list()[0].name, "Mara");
    }

    #[test]
    fn an_unverified_key_is_not_noted() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.seen(&HERE, UserId(4), &[4; 32]).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), None);
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn the_key_seen_last_as_a_user_is_the_one_expected_there() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        verified.verify(&HERE, UserId(5), &[5; 32], "Teo").unwrap();
        verified.seen(&HERE, UserId(4), &[5; 32]).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), Some([5; 32]));

        verified.dismiss_change(&HERE, UserId(4)).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), None);
        assert_eq!(verified.verified_at(&HERE, UserId(5)), Some([5; 32]));

        verified.unverify(&[5; 32]).unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert!(!reloaded.is_verified(&[5; 32]));
        assert_eq!(reloaded.verified_at(&HERE, UserId(5)), None);
        assert!(reloaded.is_verified(&[4; 32]));
    }

    #[test]
    fn changes_from_another_process_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = VerifiedKeys::load(dir.path()).unwrap();
        let mut second = VerifiedKeys::load(dir.path()).unwrap();
        first.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        second.verify(&HERE, UserId(5), &[5; 32], "Teo").unwrap();
        assert!(second.is_verified(&[4; 32]));
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert!(reloaded.is_verified(&[4; 32]) && reloaded.is_verified(&[5; 32]));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        let mode = std::fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
