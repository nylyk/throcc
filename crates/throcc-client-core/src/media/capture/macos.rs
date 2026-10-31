#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;

use super::clock::{AudioChunker, ShareZero};
use super::{Capture, CaptureConfig, CaptureError};
use crate::media::audio::SAMPLE_RATE;
use crate::media::share_audio::STEREO_FRAME;
use crate::media::video::frame::{VideoFrame, fit_within};
use throcc_proto::frame::VIDEO_CLOCK_HZ;

/// Captured frames waiting for the share thread. A full ring drops the newest frame.
const RING: usize = 4;
/// Captured audio buffers waiting for the share thread, about 20 ms each.
const AUDIO_RING: usize = 32;
const NANOS: i128 = 1_000_000_000;

pub struct MacCapture {
    stream: SCStream,
    frames: rtrb::Consumer<(CVPixelBuffer, i64)>,
    audio: Option<rtrb::Consumer<(CMSampleBuffer, i64)>>,
    dropped: Arc<AtomicU64>,
    /// Host time, in nanoseconds, that both media clocks count from.
    zero: ShareZero,
    chunker: AudioChunker,
}

// SAFETY: SCStream is a reference to an Objective-C object that ScreenCaptureKit
// documents as usable from any thread; it is only stopped from here.
unsafe impl Send for MacCapture {}

pub fn start(config: CaptureConfig) -> Result<MacCapture, CaptureError> {
    let (filter, shape) = pick()?;
    capture(&filter, shape, config)
}

/// Captures `filter`, whose content has the aspect ratio of `shape`, at that aspect ratio
/// fitted in the config's size.
fn capture(
    filter: &SCContentFilter,
    shape: (usize, usize),
    config: CaptureConfig,
) -> Result<MacCapture, CaptureError> {
    let fps = config.fps.max(1);
    let (width, height) = fit_within(shape, (config.width, config.height));
    let stream_config = SCStreamConfiguration::new()
        .with_width(width as u32)
        .with_height(height as u32)
        .with_pixel_format(PixelFormat::YCbCr_420f)
        .with_minimum_frame_interval(&CMTime::new(1, fps as i32))
        .with_queue_depth(5)
        .with_shows_cursor(true)
        .with_captures_audio(config.audio)
        .with_sample_rate(SAMPLE_RATE as i32)
        .with_channel_count(2)
        .with_excludes_current_process_audio(true);
    let mut stream = SCStream::new(filter, &stream_config).map_err(failed)?;

    let (producer, frames) = rtrb::RingBuffer::new(RING);
    // The handler runs on one serial dispatch queue, so `try_lock` never finds it taken;
    // it is there only to make the producer shareable without ever blocking.
    let producer = std::sync::Mutex::new(producer);
    let dropped = Arc::new(AtomicU64::new(0));
    let callback_dropped = dropped.clone();
    stream
        .add_output_handler(
            move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                if kind != SCStreamOutputType::Screen {
                    return;
                }
                let Some(buffer) = sample.pixel_buffer() else {
                    return;
                };
                let at = host_nanos(&sample);
                let pushed = producer
                    .try_lock()
                    .is_ok_and(|mut p| p.push((buffer, at)).is_ok());
                if !pushed {
                    callback_dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            SCStreamOutputType::Screen,
        )
        .map_err(failed)?;
    let audio = if config.audio {
        let (producer, audio) = rtrb::RingBuffer::new(AUDIO_RING);
        let producer = std::sync::Mutex::new(producer);
        let callback_dropped = dropped.clone();
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                    if kind != SCStreamOutputType::Audio {
                        return;
                    }
                    let at = host_nanos(&sample);
                    let pushed = producer
                        .try_lock()
                        .is_ok_and(|mut p| p.push((sample, at)).is_ok());
                    if !pushed {
                        callback_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                },
                SCStreamOutputType::Audio,
            )
            .map_err(failed)?;
        Some(audio)
    } else {
        None
    };
    stream.start_capture().map_err(failed)?;
    Ok(MacCapture {
        stream,
        frames,
        audio,
        dropped,
        zero: ShareZero::new(NANOS as i64),
        chunker: AudioChunker::default(),
    })
}

/// A sample's presentation time on the host clock, in nanoseconds.
fn host_nanos(sample: &CMSampleBuffer) -> i64 {
    let time = sample.presentation_timestamp();
    if time.timescale <= 0 {
        return 0;
    }
    (i128::from(time.value) * NANOS / i128::from(time.timescale)) as i64
}

/// Shows `SCContentSharingPicker` and waits for the person's choice.
/// Asks what to share; returns it with its size in points.
fn pick() -> Result<(SCContentFilter, (usize, usize)), CaptureError> {
    let mut picker = SCContentSharingPickerConfiguration::new().map_err(failed)?;
    picker.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
        SCContentSharingPickerMode::SingleApplication,
    ]);
    let (tx, rx) = mpsc::sync_channel(1);
    SCContentSharingPicker::show(&picker, move |outcome| {
        tx.send(outcome).ok();
    });
    match rx.recv().map_err(|_| CaptureError::Cancelled)? {
        SCPickerOutcome::Picked(result) => {
            let (width, height) = result.size();
            Ok((result.filter(), (width as usize, height as usize)))
        }
        SCPickerOutcome::Cancelled => Err(CaptureError::Cancelled),
        SCPickerOutcome::Error(message) => Err(CaptureError::Failed(message)),
    }
}

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

impl Capture for MacCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        let Ok((buffer, at)) = self.frames.pop() else {
            return false;
        };
        let Ok(timestamp) = u32::try_from(self.zero.ticks(at, VIDEO_CLOCK_HZ)) else {
            return false;
        };
        let Ok(guard) = buffer.lock_read_only() else {
            return false;
        };
        let (width, height) = (guard.width(), guard.height());
        if into.width != width || into.height != height {
            *into = VideoFrame::blank(width, height);
        }
        into.timestamp = timestamp;
        copy_plane(&guard, 0, width, height, &mut into.y);
        copy_plane(&guard, 1, width, height.div_ceil(2), &mut into.uv);
        true
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }

    fn captures_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        let audio = self.audio.as_mut()?;
        loop {
            while self.chunker.needs_more() {
                let (sample, at) = audio.pop().ok()?;
                let ticks = self.zero.ticks(at, SAMPLE_RATE);
                self.chunker.push(ticks, stereo(&sample));
            }
            if let Some(timestamp) = self.chunker.take(into) {
                return Some(timestamp);
            }
        }
    }
}

/// A buffer of 32-bit float audio, planar or interleaved, as interleaved stereo.
fn stereo(sample: &CMSampleBuffer) -> Vec<f32> {
    let mut out = Vec::new();
    let Ok(list) = sample.audio_buffer_list() else {
        return out;
    };
    let floats = |buffer: &screencapturekit::cm::AudioBuffer| -> Vec<f32> {
        buffer
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b))
            .collect()
    };
    match (list.get(0), list.get(1)) {
        (Some(left), Some(right)) => {
            for (l, r) in floats(left).into_iter().zip(floats(right)) {
                out.extend([l, r]);
            }
        }
        (Some(only), None) if only.number_channels >= 2 => {
            let channels = only.number_channels as usize;
            for frame in floats(only).chunks_exact(channels) {
                if let [l, r, ..] = frame {
                    out.extend([*l, *r]);
                }
            }
        }
        (Some(mono), None) => {
            for s in floats(mono) {
                out.extend([s, s]);
            }
        }
        _ => {}
    }
    out
}

/// Copies `rows` rows of `row_bytes` from plane `index` into a tightly packed buffer.
fn copy_plane(
    guard: &screencapturekit::cv::CVPixelBufferLockGuard<'_>,
    index: usize,
    row_bytes: usize,
    rows: usize,
    out: &mut [u8],
) {
    let Some(base) = guard.base_address_of_plane(index) else {
        return;
    };
    let stride = guard.bytes_per_row_of_plane(index);
    let rows = rows.min(guard.height_of_plane(index));
    for row in 0..rows {
        let Some(dst) = out.get_mut(row * row_bytes..(row + 1) * row_bytes) else {
            return;
        };
        // SAFETY: the buffer is locked, and plane `index` has `height_of_plane` rows of
        // `stride` bytes, each at least `row_bytes` long.
        let src =
            unsafe { std::slice::from_raw_parts(base.add(row * stride), row_bytes.min(stride)) };
        if let Some(d) = dst.get_mut(..src.len()) {
            d.copy_from_slice(src);
        }
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stream.stop_capture().ok();
    }
}

/// Captures the main display without asking, for tests on real hardware.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn main_display(config: CaptureConfig) -> MacCapture {
    let content = SCShareableContent::get().expect("screen recording permission");
    let display = content.displays().into_iter().next().expect("a display");
    let shape = (display.width() as usize, display.height() as usize);
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()
        .expect("a display filter");
    capture(&filter, shape, config).expect("capture to start")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::media::capture::Capture;

    #[test]
    #[ignore = "needs a display and the screen recording permission"]
    fn main_display_delivers_frames() {
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 30,
            audio: false,
        };
        let mut capture = main_display(config);
        let mut frame = VideoFrame::blank(1280, 720);
        let mut timestamps = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && timestamps.len() < 30 {
            if capture.next_frame(&mut frame) {
                timestamps.push(frame.timestamp);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(
            timestamps.len() >= 10,
            "only {} frames in 3 s",
            timestamps.len()
        );
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]));
        assert!(
            frame.y.iter().any(|&v| v != frame.y[0]),
            "the picture is uniform"
        );
    }

    #[test]
    #[ignore = "needs a display, the screen recording permission and speakers"]
    fn share_audio_arrives_in_order_with_sound() {
        let config = CaptureConfig {
            width: 640,
            height: 360,
            fps: 30,
            audio: true,
        };
        let mut capture = main_display(config);
        assert!(capture.captures_audio());
        let mut player = std::process::Command::new("afplay")
            .args(["-t", "2", "/System/Library/Sounds/Submarine.aiff"])
            .spawn()
            .unwrap();
        let mut frame = VideoFrame::blank(640, 360);
        let mut sound = [0.0f32; STEREO_FRAME];
        let (mut timestamps, mut loudest) = (Vec::new(), 0.0f32);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            capture.next_frame(&mut frame);
            match capture.next_audio(&mut sound) {
                Some(timestamp) => {
                    timestamps.push(timestamp);
                    loudest = sound.iter().fold(loudest, |m, s| m.max(s.abs()));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        player.wait().ok();
        assert!(
            timestamps.len() > 100,
            "{} audio frames in 3 s",
            timestamps.len()
        );
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]));
        assert!(loudest > 0.01, "loudest sample {loudest}");
    }
}
