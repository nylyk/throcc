use std::path::Path;

use anyhow::Result;
use keyring_core::{Entry, Error};
use zeroize::Zeroizing;

use super::SeedEntry;

const SERVICE: &str = "throcc";
const IDENTITY: &str = "identity";

/// The keychain entry for the identity key's seed, or `None` where there is no keychain.
#[cfg(not(target_os = "ios"))]
pub(super) fn identity_entry(_dir: &Path) -> Result<Option<Entry>> {
    match keyring::Entry::new(SERVICE, IDENTITY) {
        Ok(entry) => Ok(Some(entry.inner)),
        Err(Error::NoDefaultStore) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The keychain entry for the identity key's seed. The item never leaves this device and
/// is readable once the phone was unlocked after boot.
#[cfg(target_os = "ios")]
pub(super) fn identity_entry(_dir: &Path) -> Result<Option<Entry>> {
    use keyring_core::api::CredentialStoreApi;

    let store = apple_native_keyring_store::protected::Store::new()?;
    let policy = [("access-policy", "AfterFirstUnlockThisDeviceOnly")].into();
    Ok(Some(store.build(SERVICE, IDENTITY, Some(&policy))?))
}

impl SeedEntry for Entry {
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match self.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, seed: &[u8]) -> Result<()> {
        Ok(self.set_secret(seed)?)
    }
}
