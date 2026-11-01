use std::path::Path;

use anyhow::Result;
use throcc_proto::Fingerprint;
use throcc_proto::ids::Hash;

use crate::servers::{Hex, Servers};

/// A server's name and picture hash as last seen, kept on this computer to show while not
/// connected to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerLook {
    pub name: Option<String>,
    pub avatar: Option<Hash>,
}

impl ServerLook {
    /// The look kept for `server` in `dir`, empty when none was kept.
    pub fn load(dir: &Path, server: &Fingerprint) -> Result<Self> {
        let kept = Servers::load(dir)?.take(server);
        Ok(Self {
            name: kept.name,
            avatar: kept.avatar.map(|Hex(hash)| Hash(hash)),
        })
    }

    pub fn save(&self, dir: &Path, server: &Fingerprint) -> Result<()> {
        Servers::update(dir, |s| {
            let kept = s.server_mut(server);
            kept.name.clone_from(&self.name);
            kept.avatar = self.avatar.map(|Hash(hash)| Hex(hash));
        })
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
