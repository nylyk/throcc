#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

#[cfg(target_os = "android")]
mod android;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod desktop;
#[cfg(target_os = "ios")]
mod ios;

#[cfg(target_os = "android")]
use android as backend;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use desktop as backend;
#[cfg(target_os = "ios")]
use ios as backend;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use backend::list;
pub use backend::open;

/// Capture ring capacity in seconds of audio, at the device rate.
const RING_SECONDS: u32 = 1;
/// Playback ring capacity in milliseconds of audio, room for the few pulls kept queued.
const PLAYBACK_RING_MS: u32 = 200;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("no {0} device")]
    Missing(&'static str),
    #[error("no device called {0}")]
    NotFound(String),
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    #[error("the {0} device uses {1:?} samples, which throcc doesn't handle")]
    Format(&'static str, cpal::SampleFormat),
    #[error("audio device: {0}")]
    Backend(String),
}

/// Counts of samples the callbacks couldn't move, read by the audio worker.
#[derive(Debug, Default)]
pub struct DeviceCounters {
    /// Mic samples dropped because the capture ring was full.
    pub overruns: AtomicU64,
    /// Speaker samples played as silence because the playback ring was empty.
    pub underruns: AtomicU64,
    /// Errors either stream reported, such as an xrun or the device going away. Counted
    /// rather than logged, since the callback runs on the device's real-time thread.
    pub stream_errors: AtomicU64,
}

/// An open mic and speaker pair. Dropping it stops both streams.
pub struct Devices {
    _streams: backend::Streams,
    pub input_rate: u32,
    pub output_rate: u32,
    pub capture: rtrb::Consumer<f32>,
    /// Left and right samples, interleaved.
    pub playback: rtrb::Producer<f32>,
    pub counters: Arc<DeviceCounters>,
}
