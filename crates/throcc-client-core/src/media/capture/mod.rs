#[cfg(any(windows, target_os = "linux", test))]
mod audio_ring;
#[cfg(any(windows, target_os = "macos", target_os = "linux", test))]
mod clock;
mod limiter;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
mod windows;

use std::time::{Duration, Instant};

use throcc_proto::frame::VIDEO_CLOCK_HZ;
use throcc_proto::messages::ShareConfig;
#[cfg(target_os = "macos")]
pub use throcc_video::PixelBuffer;
use throcc_video::frame::VideoFrame;
#[cfg(any(windows, target_os = "linux"))]
use throcc_video::frame::copy_nv12;
#[cfg(target_os = "linux")]
use throcc_video::frame::resize;
#[cfg(target_os = "linux")]
pub use throcc_video::{DmaBuf, DmaBufPlane};

use super::audio::{FRAME, SAMPLE_RATE};
use super::share_audio::STEREO_FRAME;
pub(crate) use limiter::FrameLimiter;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("screen sharing isn't available on this platform yet")]
    Unsupported,
    #[error("you didn't pick anything to share")]
    Cancelled,
    #[error("screen capture: {0}")]
    Failed(String),
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
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
    /// The oldest captured frame not yet taken, when it is still in GPU memory. It stays
    /// valid until the next call to this or [`Capture::next_frame`]. Never blocks.
    #[cfg(target_os = "linux")]
    fn next_dmabuf(&mut self) -> Option<&DmaBuf> {
        None
    }
    /// Asks for no more frames in GPU memory with `modifier`, or none at all without one.
    /// Those already captured are skipped.
    #[cfg(target_os = "linux")]
    fn refuse_dmabuf(&mut self, _modifier: Option<u64>) {}
    /// The oldest captured frame not yet taken, in the pixel buffer the system captured it
    /// into. Never blocks.
    #[cfg(target_os = "macos")]
    fn next_pixel_buffer(&mut self) -> Option<PixelBuffer> {
        None
    }
    /// Frames and audio buffers the capture callbacks dropped because a ring was full.
    fn dropped(&self) -> u64 {
        0
    }
    /// Whether capturing stopped for good, as when the shared window closed or the system
    /// ended the share.
    fn ended(&self) -> bool {
        false
    }
    fn captures_audio(&self) -> bool {
        false
    }
    /// The oldest 20 ms of share audio not yet taken, 48 kHz stereo interleaved. Returns its
    /// timestamp in 48 kHz ticks from the same zero as the video's. Never blocks.
    fn next_audio(&mut self, _into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        None
    }
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
    audio: bool,
    /// Timestamp of the next audio frame, in 48 kHz ticks since `epoch`.
    audio_next: u64,
    bases: Vec<VideoFrame>,
    shown: usize,
    epoch: Instant,
    interval: Duration,
    next: Instant,
}

impl TestPattern {
    pub fn new(config: CaptureConfig, epoch: Instant) -> Self {
        let since_epoch = ticks(epoch.elapsed(), SAMPLE_RATE);
        Self {
            audio: config.audio,
            audio_next: since_epoch - since_epoch % FRAME as u64,
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
        into.timestamp = ticks(elapsed, VIDEO_CLOCK_HZ) as u32;
        true
    }

    fn captures_audio(&self) -> bool {
        self.audio
    }

    /// A 440 Hz tone on the left and 660 Hz on the right, as real time allows.
    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        let now = ticks(self.epoch.elapsed(), SAMPLE_RATE);
        if !self.audio || now < self.audio_next + FRAME as u64 {
            return None;
        }
        let start = self.audio_next;
        for (i, pair) in into.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let t = (start + i as u64) as f32 / SAMPLE_RATE as f32;
            let tone = |hz: f32| 0.2 * (std::f32::consts::TAU * hz * t).sin();
            *pair = [tone(440.0), tone(660.0)];
        }
        self.audio_next += FRAME as u64;
        Some(start as u32)
    }
}

/// `elapsed` in ticks of a `hz` clock.
fn ticks(elapsed: Duration, hz: u32) -> u64 {
    (elapsed.as_micros() * u128::from(hz) / 1_000_000) as u64
}

/// The timestamps of up to `want` frames captured within five seconds, the last copied
/// one in `frame`. Frames in GPU memory count but aren't copied.
#[cfg(test)]
fn frames_within(capture: &mut dyn Capture, frame: &mut VideoFrame, want: usize) -> Vec<u32> {
    let mut timestamps = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && timestamps.len() < want {
        #[cfg(target_os = "linux")]
        if let Some(dmabuf) = capture.next_dmabuf() {
            timestamps.push(dmabuf.timestamp);
            continue;
        }
        if capture.next_frame(frame) {
            timestamps.push(frame.timestamp);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    timestamps
}
