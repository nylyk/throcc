use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use throcc_proto::ids::Hash;

use crate::keystore::replace_file;

/// A server's name and picture as last seen, kept on this computer to show while not
/// connected to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerLook {
    pub name: Option<String>,
    pub avatar: Option<Hash>,
    /// The picture behind `avatar`, once it was fetched.
    pub picture: Option<Arc<[u8]>>,
}

impl ServerLook {
    /// The look kept for `server` in `dir`, empty when none was kept.
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let (text_path, picture_path) = paths(dir, server);
        let Some(text) = read(&text_path)? else {
            return Ok(Self::default());
        };
        let text = String::from_utf8_lossy(&text);
        let mut lines = text.lines();
        let avatar = lines.next().and_then(parse_hash);
        let name = lines
            .next()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let picture = match avatar {
            Some(_) => read(&picture_path)?.map(Arc::from),
            None => None,
        };
        Ok(Self {
            name,
            avatar,
            picture,
        })
    }

    pub fn save(&self, dir: &Path, server: &Fingerprint) -> Result<()> {
        let (text_path, picture_path) = paths(dir, server);
        let avatar = self.avatar.map(|h| hex(&h.0)).unwrap_or_default();
        let name = self.name.as_deref().unwrap_or_default();
        replace_file(&text_path, format!("{avatar}\n{name}\n").as_bytes())?;
        match &self.picture {
            Some(bytes) => replace_file(&picture_path, bytes),
            None => match fs::remove_file(&picture_path) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", picture_path.display()))
                }
                _ => Ok(()),
            },
        }
    }
}

fn paths(dir: &Path, server: &Fingerprint) -> (PathBuf, PathBuf) {
    let hex = hex(&server.0);
    (
        dir.join(format!("server-{hex}")),
        dir.join(format!("server-{hex}-picture")),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hash(text: &str) -> Option<Hash> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(Hash(hash))
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_look_loads_back_per_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = Fingerprint([7; 32]);
        assert_eq!(
            ServerLook::load(dir.path(), &server).unwrap(),
            ServerLook::default()
        );
        let look = ServerLook {
            name: Some("Moss Co-op".into()),
            avatar: Some(Hash([0xab; 32])),
            picture: Some(Arc::from(&b"png"[..])),
        };
        look.save(dir.path(), &server).unwrap();
        assert_eq!(ServerLook::load(dir.path(), &server).unwrap(), look);
        let other = Fingerprint([8; 32]);
        assert_eq!(
            ServerLook::load(dir.path(), &other).unwrap(),
            ServerLook::default()
        );
        ServerLook::default().save(dir.path(), &server).unwrap();
        assert_eq!(
            ServerLook::load(dir.path(), &server).unwrap(),
            ServerLook::default()
        );
    }
}
