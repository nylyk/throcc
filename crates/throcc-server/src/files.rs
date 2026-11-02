//! The data directory and the private files in it.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

/// Creates the data directory at mode 0700, and tightens an existing one this user owns to
/// 0700. One owned by another user is used as it is, with a warning, since only its files
/// matter and [`check_private_file`] checks those.
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

    use anyhow::{Context, Result, bail};
    use tracing::warn;

    /// Brings a path this user owns down to `mode`. A directory owned by another user is
    /// only warned about; a file owned by another user is refused, since whoever owns it
    /// could have planted or could replace the key or the database.
    pub fn tighten(path: &Path, mode: u32) -> Result<()> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let (owner, me) = (metadata.uid(), effective_uid());
        let current = metadata.permissions().mode() & 0o777;
        match owner_check(owner, me, metadata.is_dir()) {
            Owner::Me => {}
            Owner::OtherDir => {
                warn!(
                    "{} belongs to uid {owner}, not this server's uid {me}, and has mode {current:o}; \
                     anyone who can write to it can replace the server's files",
                    path.display()
                );
                return Ok(());
            }
            Owner::OtherFile => bail!(
                "{} belongs to uid {owner}, not this server's uid {me}; refusing to use it",
                path.display()
            ),
        }
        if current & !mode != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .with_context(|| format!("restricting {} to mode {mode:o}", path.display()))?;
            warn!(
                "{} had mode {current:o}; restricted it to {mode:o}",
                path.display()
            );
        }
        Ok(())
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

    fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::unix::{Owner, owner_check};
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
}
