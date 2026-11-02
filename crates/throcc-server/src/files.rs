//! The data directory and the private files in it.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

/// Creates the data directory at mode 0700, and tightens an existing one this user owns to
/// 0700. One owned by another user is used as it is, with a warning if others may write to
/// it, since only its files matter and [`check_private_file`] checks those.
pub fn prepare_data_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    unix::tighten(path, 0o700)?;
    Ok(())
}

/// Refuses a file another user owns, and tightens one this user owns to mode 0600. A
/// missing file is fine.
pub fn check_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    unix::tighten(path, 0o600)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The owner of `path`, if it isn't the user this process runs as.
#[cfg(unix)]
pub fn other_owner(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    let owner = fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .uid();
    Ok((owner != unix::effective_uid()).then_some(owner))
}

#[cfg(not(unix))]
pub fn other_owner(_: &Path) -> Result<Option<u32>> {
    Ok(None)
}

/// Creates `path` empty at mode 0600 if it doesn't exist yet.
pub fn create_private_file(path: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use anyhow::{Context, Result, anyhow};
    use tracing::{debug, info, warn};

    const SETGID: u32 = 0o2000;

    /// Brings a path this user owns down to `mode`. A directory owned by another user is
    /// used as it is, with a warning unless only root and the pod's group can write to it; a
    /// file owned by another user is refused, since whoever owns it could have planted or
    /// could replace the key or the database.
    pub fn tighten(path: &Path, mode: u32) -> Result<()> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let (owner, me) = (metadata.uid(), effective_uid());
        let current = metadata.mode() & 0o7777;
        let pod_group = in_pod_group(path, &metadata);
        match owner_check(owner, me, metadata.is_dir()) {
            Owner::Me => {}
            Owner::OtherDir if only_root_or_pod_can_write(owner, current, pod_group) => {
                info!(
                    "{} belongs to uid {owner}, not this server's uid {me}, with mode {current:o}; \
                     only root and this server's group can write to it, so it is used as it is",
                    path.display()
                );
                return Ok(());
            }
            Owner::OtherDir => {
                warn!(
                    "{} belongs to uid {owner}, not this server's uid {me}, and has mode {current:o}; \
                     anyone who can write to it can replace the server's files. \
                     Give it to the server's user: chown {me} {}",
                    path.display(),
                    path.display()
                );
                return Ok(());
            }
            Owner::OtherFile => return Err(foreign_file(path, owner, me)),
        }
        let loose = current & 0o777 & !mode;
        if loose == 0 {
            return Ok(());
        }
        // Setgid on a directory only picks the group of new files; keeping it lets the next
        // start still recognise an fsGroup volume.
        let tightened = mode | (current & SETGID);
        fs::set_permissions(path, fs::Permissions::from_mode(tightened))
            .with_context(|| format!("restricting {} to mode {mode:o}", path.display()))?;
        let message = format!(
            "{} had mode {current:o}; restricted it to {tightened:o}",
            path.display()
        );
        match (only_group_was_open(loose, pod_group), metadata.is_dir()) {
            // Kubernetes re-opens the volume on every mount; one line for it is enough.
            (true, true) => info!("{message}, as on every start on a Kubernetes fsGroup volume"),
            (true, false) => debug!("{message}"),
            (false, _) => warn!("{message}"),
        }
        Ok(())
    }

    pub fn foreign_file(path: &Path, owner: u32, me: u32) -> anyhow::Error {
        let dir = path.parent().unwrap_or(path);
        anyhow!(
            "{} belongs to uid {owner}, but this server runs as uid {me}. Another user's file \
             could have been planted or swapped, so the server won't use it. If it is this \
             server's data, give the data directory to the server's user \
             (as root: chown -R {me} {}), or run the server as uid {owner}",
            path.display(),
            dir.display()
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Owner {
        Me,
        OtherDir,
        OtherFile,
    }

    pub fn owner_check(owner: u32, me: u32, is_dir: bool) -> Owner {
        match (owner == me, is_dir) {
            (true, _) => Owner::Me,
            (false, true) => Owner::OtherDir,
            (false, false) => Owner::OtherFile,
        }
    }

    /// Whether a directory another user owns is still safe to keep files in: root owns it,
    /// and nobody else but the pod's group may write to it.
    pub fn only_root_or_pod_can_write(owner: u32, mode: u32, pod_group: bool) -> bool {
        owner == 0 && mode & 0o002 == 0 && (mode & 0o020 == 0 || pod_group)
    }

    /// Whether the access `loose` grants beyond the wanted mode went only to the pod's group.
    pub fn only_group_was_open(loose: u32, pod_group: bool) -> bool {
        loose & 0o007 == 0 && pod_group
    }

    /// Whether `path`'s group is the pod's group of a Kubernetes `fsGroup` volume. Kubelet
    /// makes the volume's directories setgid to that group, which this server is in, and
    /// opens them and their files to it on every mount. Nobody outside the pod is in it.
    pub fn in_pod_group(path: &Path, metadata: &fs::Metadata) -> bool {
        let dir = if metadata.is_dir() {
            Some(metadata.clone())
        } else {
            path.parent().and_then(|dir| fs::metadata(dir).ok())
        };
        dir.is_some_and(|dir| {
            dir.mode() & SETGID != 0 && dir.gid() == metadata.gid() && in_group(dir.gid())
        })
    }

    fn in_group(gid: u32) -> bool {
        // SAFETY: getegid has no preconditions and cannot fail.
        if unsafe { libc::getegid() } == gid {
            return true;
        }
        // SAFETY: with a size of 0, getgroups only returns the count and writes nothing.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let Ok(len) = usize::try_from(count) else {
            return false;
        };
        let mut groups: Vec<libc::gid_t> = vec![0; len];
        // SAFETY: `groups` holds `count` writable entries.
        let written = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        usize::try_from(written).is_ok_and(|n| groups[..n.min(len)].contains(&gid))
    }

    pub fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::unix::{
        Owner, foreign_file, in_pod_group, only_group_was_open, only_root_or_pod_can_write,
        owner_check,
    };
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn an_existing_open_data_dir_and_its_files_are_made_private() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("data");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        prepare_data_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let file = dir.join("throcc.sqlite");
        fs::write(&file, b"").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        check_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o600);
        check_private_file(&dir.join("missing")).unwrap();
    }

    #[test]
    fn new_data_dirs_and_files_are_private() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("a/b");
        prepare_data_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        let file = dir.join("f");
        create_private_file(&file).unwrap();
        create_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o600);
    }

    #[test]
    fn only_files_of_another_user_are_refused() {
        assert_eq!(owner_check(1000, 1000, false), Owner::Me);
        assert_eq!(owner_check(1001, 1000, true), Owner::OtherDir);
        assert_eq!(owner_check(0, 1000, false), Owner::OtherFile);
    }

    #[test]
    fn a_kubernetes_fs_group_volume_is_recognised_and_still_made_private() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("data");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o2770)).unwrap();
        let file = dir.join("throcc.sqlite");
        fs::write(&file, b"").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(in_pod_group(&file, &fs::metadata(&file).unwrap()));

        prepare_data_dir(&dir).unwrap();
        check_private_file(&file).unwrap();
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(dir_mode, 0o2700);
        assert_eq!(mode(&file), 0o600);
        assert!(in_pod_group(&file, &fs::metadata(&file).unwrap()));
    }

    #[test]
    fn only_access_that_went_to_the_pod_group_is_quiet() {
        assert!(only_group_was_open(0o060, true));
        assert!(!only_group_was_open(0o060, false));
        assert!(!only_group_was_open(0o064, true));

        assert!(only_root_or_pod_can_write(0, 0o755, false));
        assert!(only_root_or_pod_can_write(0, 0o2775, true));
        assert!(!only_root_or_pod_can_write(0, 0o2775, false));
        assert!(!only_root_or_pod_can_write(0, 0o777, true));
        assert!(!only_root_or_pod_can_write(1000, 0o700, true));
    }

    #[test]
    fn a_refused_file_says_how_to_fix_it() {
        let refusal = foreign_file(Path::new("/data/server_key"), 0, 65532).to_string();
        assert!(refusal.contains("chown -R 65532 /data"), "{refusal}");
        assert!(refusal.contains("run the server as uid 0"), "{refusal}");
    }
}
