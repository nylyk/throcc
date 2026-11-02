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
/// was seen under, so a server later showing another key for one of them is noticed, and
/// that change is kept until the user verifies the new key or dismisses it. Every change is
/// written at once, except which key a recorded change now shows.
pub(crate) struct VerifiedKeys {
    dir: PathBuf,
    file: VerifiedFile,
    /// The key shown last where a change is recorded with an earlier one, written with the
    /// next change: a server flipping between keys can't make each room state rewrite the
    /// file.
    shown_since: BTreeMap<SeenAs, PublicKey>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct VerifiedFile {
    people: BTreeMap<Hex, Verified>,
    /// Pairs seen under a verified key that later showed another, unverified key. Each is
    /// one of a verified key's `seen` pairs, so they are bounded alike.
    changed: Vec<Changed>,
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

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Changed {
    #[serde(flatten)]
    at: SeenAs,
    /// The key shown there last that wasn't the verified one.
    now: Hex,
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

    fn changed_at(&self, at: SeenAs) -> Option<PublicKey> {
        self.changed.iter().find(|c| c.at == at).map(|c| c.now.0)
    }

    fn forget_change(&mut self, at: SeenAs) {
        self.changed.retain(|c| c.at != at);
    }
}

impl VerifiedKeys {
    /// None verified, kept in `dir`, for when the file there can't be read. Every change
    /// re-reads it first, so a file that can't be read is never overwritten.
    pub fn empty(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            file: VerifiedFile::default(),
            shown_since: BTreeMap::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            dir: dir.to_owned(),
            file: json_file::load(&dir.join(FILE))?,
            shown_since: BTreeMap::new(),
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

    /// The unverified key `server` showed as `user` last, where a verified key was seen
    /// before, until the user verifies it or dismisses the change. It outlives that key
    /// being shown again, so a server switching back and forth can't hide it.
    pub fn changed_at(&self, server: &Fingerprint, user: UserId) -> Option<PublicKey> {
        let at = seen_as(server, user);
        let written = self.file.changed_at(at)?;
        Some(self.shown_since.get(&at).copied().unwrap_or(written))
    }

    /// Notes that `server` shows `key`, validly signed, as `user`: where it is verified, or
    /// where it replaces a verified key seen there. Written only when that is new, and not
    /// for another replacing key where a change is already recorded.
    pub fn presented(&mut self, server: &Fingerprint, user: UserId, key: &PublicKey) -> Result<()> {
        let at = seen_as(server, user);
        let expected = self.verified_at(server, user);
        if self.is_verified(key) {
            if expected == Some(*key) {
                return Ok(());
            }
            return self.change(|file| {
                file.see(key, at);
                file.forget_change(at);
            });
        }
        if expected.is_none() {
            return Ok(());
        }
        if self.file.changed_at(at).is_some() {
            self.shown_since.insert(at, *key);
            return Ok(());
        }
        self.change(|file| {
            file.forget_change(at);
            file.changed.push(Changed { at, now: Hex(*key) });
        })
    }

    /// Verifies `key`, which `server` shows as `user` under `name`.
    pub fn verify(
        &mut self,
        server: &Fingerprint,
        user: UserId,
        key: &PublicKey,
        name: &str,
    ) -> Result<()> {
        let at = seen_as(server, user);
        self.change(|file| {
            file.people.entry(Hex(*key)).or_default().name = name.to_owned();
            file.see(key, at);
            file.forget_change(at);
            file.changed.retain(|c| c.now.0 != *key);
        })
    }

    /// Takes `key` off the verified ones, on every server, with the changes noted against it.
    pub fn unverify(&mut self, key: &PublicKey) -> Result<()> {
        self.change(|file| {
            if let Some(verified) = file.people.remove(&Hex(*key)) {
                file.changed.retain(|c| !verified.seen.contains(&c.at));
            }
        })
    }

    /// Stops expecting the verified key last seen as `user` on `server` there, after the
    /// user accepted that it changed.
    /// Stops the warning for `user` on `server`. When `presented` is a verified key, the pair
    /// stays watched under it, in the same write, so a later swap is still warned of.
    pub fn dismiss_change(
        &mut self,
        server: &Fingerprint,
        user: UserId,
        presented: Option<&PublicKey>,
    ) -> Result<()> {
        let at = seen_as(server, user);
        let watched = presented.filter(|key| self.is_verified(key)).copied();
        self.shown_since.remove(&at);
        self.change(|file| {
            file.forget_seen(at, None);
            file.forget_change(at);
            if let Some(key) = &watched {
                file.see(key, at);
            }
        })
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
        let shown_since = &self.shown_since;
        json_file::update(&self.dir.join(FILE), |file: &mut VerifiedFile| {
            for changed in &mut file.changed {
                if let Some(key) = shown_since.get(&changed.at) {
                    changed.now = Hex(*key);
                }
            }
            change(file);
            written = Some(file.clone());
        })?;
        self.shown_since.clear();
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

        verified.presented(&THERE, UserId(9), &[4; 32]).unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert_eq!(reloaded.verified_at(&THERE, UserId(9)), Some([4; 32]));
        assert_eq!(reloaded.list()[0].servers, [HERE, THERE]);
        assert_eq!(reloaded.list()[0].name, "Mara");
    }

    #[test]
    fn an_unverified_key_is_not_noted() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.presented(&HERE, UserId(4), &[4; 32]).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), None);
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn the_key_seen_last_as_a_user_is_the_one_expected_there() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        verified.verify(&HERE, UserId(5), &[5; 32], "Teo").unwrap();
        verified.presented(&HERE, UserId(4), &[5; 32]).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), Some([5; 32]));

        verified.dismiss_change(&HERE, UserId(4), None).unwrap();
        assert_eq!(verified.verified_at(&HERE, UserId(4)), None);
        assert_eq!(verified.verified_at(&HERE, UserId(5)), Some([5; 32]));

        verified.unverify(&[5; 32]).unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert!(!reloaded.is_verified(&[5; 32]));
        assert_eq!(reloaded.verified_at(&HERE, UserId(5)), None);
        assert!(reloaded.is_verified(&[4; 32]));
    }

    #[test]
    fn a_changed_key_stays_noted_until_verified_or_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        verified.presented(&HERE, UserId(4), &[9; 32]).unwrap();
        verified.presented(&HERE, UserId(4), &[4; 32]).unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert_eq!(reloaded.changed_at(&HERE, UserId(4)), Some([9; 32]));
        assert_eq!(reloaded.verified_at(&HERE, UserId(4)), Some([4; 32]));
        assert_eq!(reloaded.changed_at(&THERE, UserId(4)), None);

        verified
            .verify(&THERE, UserId(2), &[9; 32], "Mara")
            .unwrap();
        assert_eq!(
            verified.changed_at(&HERE, UserId(4)),
            None,
            "the new key verified"
        );

        verified.presented(&HERE, UserId(4), &[8; 32]).unwrap();
        verified.dismiss_change(&HERE, UserId(4), None).unwrap();
        assert_eq!(verified.changed_at(&HERE, UserId(4)), None);

        verified.presented(&HERE, UserId(4), &[4; 32]).unwrap();
        verified.presented(&HERE, UserId(4), &[8; 32]).unwrap();
        verified.unverify(&[4; 32]).unwrap();
        assert_eq!(verified.changed_at(&HERE, UserId(4)), None);
    }

    #[test]
    fn a_server_flipping_between_new_keys_does_not_rewrite_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut verified = VerifiedKeys::load(dir.path()).unwrap();
        verified.verify(&HERE, UserId(4), &[4; 32], "Mara").unwrap();
        verified.presented(&HERE, UserId(4), &[9; 32]).unwrap();
        let written = std::fs::read(dir.path().join(FILE)).unwrap();
        for _ in 0..10 {
            verified.presented(&HERE, UserId(4), &[8; 32]).unwrap();
            verified.presented(&HERE, UserId(4), &[9; 32]).unwrap();
        }
        verified.presented(&HERE, UserId(4), &[8; 32]).unwrap();
        verified.presented(&HERE, UserId(4), &[4; 32]).unwrap();
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), written);
        assert_eq!(verified.changed_at(&HERE, UserId(4)), Some([8; 32]));

        verified.verify(&HERE, UserId(5), &[5; 32], "Teo").unwrap();
        let reloaded = VerifiedKeys::load(dir.path()).unwrap();
        assert_eq!(reloaded.changed_at(&HERE, UserId(4)), Some([8; 32]));

        verified.presented(&HERE, UserId(4), &[9; 32]).unwrap();
        verified
            .verify(&THERE, UserId(2), &[9; 32], "Mara")
            .unwrap();
        assert_eq!(
            verified.changed_at(&HERE, UserId(4)),
            None,
            "the key shown last verified"
        );
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
