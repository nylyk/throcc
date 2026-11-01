use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use throcc_proto::Fingerprint;
use throcc_proto::ids::Hash;

use crate::pictures;
use crate::servers::{Hex, Servers};

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
        let kept = Servers::load(dir)?.take(server);
        let avatar = kept.avatar.map(|Hex(hash)| Hash(hash));
        let picture = match avatar {
            Some(_) => pictures::load(dir, &picture_name(server))?,
            None => None,
        };
        Ok(Self {
            name: kept.name,
            avatar,
            picture,
        })
    }

    pub fn save(&self, dir: &Path, server: &Fingerprint) -> Result<()> {
        Servers::update(dir, |s| {
            let kept = s.server_mut(server);
            kept.name.clone_from(&self.name);
            kept.avatar = self.avatar.map(|Hash(hash)| Hex(hash));
        })?;
        pictures::save(dir, &picture_name(server), self.picture.as_deref())
    }
}

fn picture_name(server: &Fingerprint) -> String {
    let hex: String = server.0.iter().map(|b| format!("{b:02x}")).collect();
    format!("server-{hex}")
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
