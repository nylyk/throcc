#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use throcc_proto::frame::VIDEO_CLOCK_HZ;
use throcc_proto::messages::ShareConfig;

use super::video::frame::VideoFrame;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("screen sharing isn't available on this platform yet")]
    Unsupported,
    #[error("you didn't pick anything to share")]
    Cancelled,
    #[error("screen capture: {0}")]
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub audio: bool,
}

impl CaptureConfig {
    pub fn for_share(share: &ShareConfig) -> Self {
        Self {
            width: usize::from(share.width),
            height: usize::from(share.height),
            fps: u32::from(share.fps),
            audio: share.audio,
        }
    }
}

/// A running capture. Frames are taken with [`Capture::next_frame`]; dropping it stops.
pub trait Capture: Send {
    /// The oldest captured frame not yet taken, copied out as NV12. Never blocks.
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool;
    /// Frames the capture callback dropped because the ring was full.
    fn dropped(&self) -> Arc<AtomicU64>;
}

/// Asks the person what to share with the system's own picker, then starts capturing it.
/// Blocks until the picker closes.
pub fn start(config: CaptureConfig) -> Result<Box<dyn Capture>, CaptureError> {
    #[cfg(target_os = "macos")]
    {
        macos::start(config).map(|c| Box::new(c) as Box<dyn Capture>)
    }
    #[cfg(windows)]
    {
        windows::start(config).map(|c| Box::new(c) as Box<dyn Capture>)
    }
    #[cfg(target_os = "linux")]
    {
        linux::start(config).map(|c| Box::new(c) as Box<dyn Capture>)
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = config;
        Err(CaptureError::Unsupported)
    }
}

/// A synthetic capture whose pictures carry the milliseconds since `epoch` (mod 2¹⁶),
/// readable with [`VideoFrame::read_test_number`].
pub struct TestPattern {
    bases: Vec<VideoFrame>,
    shown: usize,
    epoch: Instant,
    interval: Duration,
    next: Instant,
}

impl TestPattern {
    pub fn new(config: CaptureConfig, epoch: Instant) -> Self {
        Self {
            bases: (0..8)
                .map(|n| VideoFrame::test_pattern(config.width, config.height, n, 0))
                .collect(),
            shown: 0,
            epoch,
            interval: Duration::from_secs(1) / config.fps.max(1),
            next: Instant::now(),
        }
    }
}

impl Capture for TestPattern {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        let now = Instant::now();
        if now < self.next {
            return false;
        }
        self.next = (self.next + self.interval).max(now);
        let elapsed = now - self.epoch;
        self.shown = (self.shown + 1) % self.bases.len();
        into.clone_from(&self.bases[self.shown]);
        into.stamp_number(elapsed.as_millis() as u16);
        into.timestamp = (elapsed.as_micros() * u128::from(VIDEO_CLOCK_HZ) / 1_000_000) as u32;
        true
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        Arc::default()
    }
}
