use throcc_client_core::keystore::Keystore;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum StoreError {
    #[error("{0:#}")]
    Failed(anyhow::Error),
}

/// Sets where the app keeps its data and what it can fetch again. The app calls this once,
/// before anything else.
#[cfg(any(target_os = "ios", target_os = "android"))]
#[uniffi::export]
pub fn set_app_dirs(data: String, cache: String) {
    throcc_client_core::keystore::set_app_dirs(data.into(), cache.into());
}

/// Our identity key, started on first use.
#[uniffi::export]
pub fn identity_key() -> Result<Vec<u8>, StoreError> {
    let store = Keystore::open_default().map_err(StoreError::Failed)?;
    Ok(store.identity().public().to_vec())
}
