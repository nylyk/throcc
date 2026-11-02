pub mod account;
pub mod avatars;
pub mod call;
pub mod client;
pub mod connection;
pub mod control;
mod files;
mod floors;
mod json_file;
mod keys;
pub mod keystore;
mod known_servers;
pub mod media;
mod playback;
mod profile;
pub mod server_look;
mod servers;
mod session;
pub mod settings;
pub mod state;
mod verified;

use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

/// Locks `mutex`, carrying on with the data a panicking holder left behind.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Seconds since the Unix epoch, by the system clock.
pub(crate) fn unix_now() -> Result<u64, SystemTimeError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
