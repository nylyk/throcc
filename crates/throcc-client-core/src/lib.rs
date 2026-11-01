pub mod account;
pub mod avatars;
pub mod call;
pub mod client;
pub mod connection;
pub mod contacts;
pub mod control;
mod json_file;
pub mod keys;
pub mod keystore;
pub mod known_servers;
pub mod media;
pub mod playback;
pub mod profile;
pub mod server_look;
mod servers;
pub mod session;
pub mod settings;
pub mod state;

/// Locks `mutex`, carrying on with the data a panicking holder left behind.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
