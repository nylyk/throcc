use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use throcc_proto::ids::Hash;
use throcc_proto::messages::MAX_AVATAR_BYTES;

use crate::files::replace_file;

const DIR: &str = "avatars";
/// How long a cached picture stays after it was last used.
const KEPT_UNUSED: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How much the cache holds at most; the pictures used longest ago go first.
const MAX_CACHED_BYTES: u64 = 64 * 1024 * 1024;

/// Whether `bytes` are the picture behind `hash`, within the size every picture keeps to.
pub fn matches(bytes: &[u8], hash: Hash) -> bool {
    bytes.len() <= MAX_AVATAR_BYTES && Hash::of(bytes) == hash
}

/// The cached picture behind `hash`, marked as used. A file that doesn't match its hash is
/// left for the server to replace.
pub fn load(cache: &Path, hash: Hash) -> Option<Arc<[u8]>> {
    let path = path(cache, hash);
    let bytes = fs::read(&path).ok()?;
    if !matches(&bytes, hash) {
        return None;
    }
    fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(SystemTime::now()))
        .ok();
    Some(bytes.into())
}

pub fn save(cache: &Path, hash: Hash, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(cache.join(DIR))?;
    replace_file(&path(cache, hash), bytes)
}

/// Removes the pictures unused for longer than [`KEPT_UNUSED`], then the ones used longest
/// ago until the rest fit in [`MAX_CACHED_BYTES`]. Returns how many it removed.
pub fn prune(cache: &Path, now: SystemTime) -> Result<usize> {
    prune_to(cache, now, MAX_CACHED_BYTES)
}

fn prune_to(cache: &Path, now: SystemTime, max_bytes: u64) -> Result<usize> {
    let dir = cache.join(DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut kept = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let metadata = fs::metadata(&path)?;
        kept.push((metadata.modified()?, metadata.len(), path));
    }
    kept.sort();
    let mut total: u64 = kept.iter().map(|(_, len, _)| len).sum();
    let mut removed = 0;
    for (used, len, path) in kept {
        if now.duration_since(used).unwrap_or_default() <= KEPT_UNUSED && total <= max_bytes {
            break;
        }
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        total -= len;
        removed += 1;
    }
    Ok(removed)
}

fn path(cache: &Path, hash: Hash) -> PathBuf {
    cache.join(DIR).join(hash.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_picture_loads_back_only_while_it_matches() {
        let cache = tempfile::tempdir().unwrap();
        let hash = Hash::of(b"png");
        assert_eq!(load(cache.path(), hash), None);
        save(cache.path(), hash, b"png").unwrap();
        assert_eq!(load(cache.path(), hash).as_deref(), Some(&b"png"[..]));

        fs::write(path(cache.path(), hash), b"other").unwrap();
        assert_eq!(load(cache.path(), hash), None);
    }

    #[test]
    fn pruning_removes_only_pictures_unused_for_a_month() {
        let cache = tempfile::tempdir().unwrap();
        let (old, fresh) = (Hash::of(b"old"), Hash::of(b"fresh"));
        save(cache.path(), old, b"old").unwrap();
        save(cache.path(), fresh, b"fresh").unwrap();
        let month_ago = SystemTime::now() - KEPT_UNUSED - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(path(cache.path(), old))
            .unwrap()
            .set_modified(month_ago)
            .unwrap();

        assert_eq!(prune(cache.path(), SystemTime::now()).unwrap(), 1);
        assert_eq!(load(cache.path(), old), None);
        assert!(load(cache.path(), fresh).is_some());
    }

    fn last_used(cache: &Path, hash: Hash, at: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path(cache, hash))
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    #[test]
    fn pruning_removes_the_pictures_used_longest_ago_past_the_size_cap() {
        let cache = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let pictures: Vec<Hash> = (0..4u8).map(|i| Hash::of(&[i])).collect();
        for (age, hash) in (1..).zip(&pictures) {
            save(cache.path(), *hash, &[0; 100]).unwrap();
            last_used(cache.path(), *hash, now - Duration::from_secs(age * 60));
        }
        assert_eq!(prune_to(cache.path(), now, 250).unwrap(), 2);
        let left: Vec<bool> = pictures
            .iter()
            .map(|h| path(cache.path(), *h).exists())
            .collect();
        assert_eq!(left, [true, true, false, false]);
    }

    #[test]
    fn a_picture_over_the_size_limit_never_matches() {
        let big = vec![0; MAX_AVATAR_BYTES + 1];
        assert!(!matches(&big, Hash::of(&big)));
        assert!(matches(&big[1..], Hash::of(&big[1..])));
    }

    #[test]
    fn loading_marks_a_picture_used() {
        let cache = tempfile::tempdir().unwrap();
        let hash = Hash::of(b"png");
        save(cache.path(), hash, b"png").unwrap();
        let month_ago = SystemTime::now() - KEPT_UNUSED - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(path(cache.path(), hash))
            .unwrap()
            .set_modified(month_ago)
            .unwrap();

        assert!(load(cache.path(), hash).is_some());
        assert_eq!(prune(cache.path(), SystemTime::now()).unwrap(), 0);
    }
}
