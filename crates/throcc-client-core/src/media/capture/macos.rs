#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;

use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;

use super::{Capture, CaptureConfig, CaptureError};
use crate::media::video::frame::VideoFrame;
use throcc_proto::frame::VIDEO_CLOCK_HZ;

/// Captured frames waiting for the share thread. A full ring drops the newest frame.
const RING: usize = 4;

pub struct MacCapture {
    stream: SCStream,
    frames: rtrb::Consumer<(CVPixelBuffer, u32)>,
    dropped: Arc<AtomicU64>,
}

// SAFETY: SCStream is a reference to an Objective-C object that ScreenCaptureKit
// documents as usable from any thread; it is only stopped from here.
unsafe impl Send for MacCapture {}

pub fn start(config: CaptureConfig) -> Result<MacCapture, CaptureError> {
    capture(&pick()?, config)
}

fn capture(filter: &SCContentFilter, config: CaptureConfig) -> Result<MacCapture, CaptureError> {
    let fps = config.fps.max(1);
    let stream_config = SCStreamConfiguration::new()
        .with_width(config.width as u32)
        .with_height(config.height as u32)
        .with_pixel_format(PixelFormat::YCbCr_420f)
        .with_minimum_frame_interval(&CMTime::new(1, fps as i32))
        .with_queue_depth(5)
        .with_shows_cursor(true)
        .with_captures_audio(config.audio)
        .with_excludes_current_process_audio(true);
    let mut stream = SCStream::new(filter, &stream_config).map_err(failed)?;

    let (producer, frames) = rtrb::RingBuffer::new(RING);
    // The handler runs on one serial dispatch queue, so `try_lock` never finds it taken;
    // it is there only to make the producer shareable without ever blocking.
    let producer = std::sync::Mutex::new(producer);
    let dropped = Arc::new(AtomicU64::new(0));
    let callback_dropped = dropped.clone();
    let zero = AtomicI64::new(i64::MIN);
    stream
        .add_output_handler(
            move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                if kind != SCStreamOutputType::Screen {
                    return;
                }
                let Some(buffer) = sample.pixel_buffer() else {
                    return;
                };
                let time = sample.presentation_timestamp();
                let ticks = if time.timescale > 0 {
                    i128::from(time.value) * i128::from(VIDEO_CLOCK_HZ) / i128::from(time.timescale)
                } else {
                    0
                } as i64;
                let start = match zero.compare_exchange(
                    i64::MIN,
                    ticks,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => ticks,
                    Err(start) => start,
                };
                let timestamp = ticks.wrapping_sub(start) as u32;
                let pushed = producer
                    .try_lock()
                    .is_ok_and(|mut p| p.push((buffer, timestamp)).is_ok());
                if !pushed {
                    callback_dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            SCStreamOutputType::Screen,
        )
        .map_err(failed)?;
    stream.start_capture().map_err(failed)?;
    Ok(MacCapture {
        stream,
        frames,
        dropped,
    })
}

/// Shows `SCContentSharingPicker` and waits for the person's choice.
fn pick() -> Result<SCContentFilter, CaptureError> {
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
        SCPickerOutcome::Picked(result) => Ok(result.filter()),
        SCPickerOutcome::Cancelled => Err(CaptureError::Cancelled),
        SCPickerOutcome::Error(message) => Err(CaptureError::Failed(message)),
    }
}

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

impl Capture for MacCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        let Ok((buffer, timestamp)) = self.frames.pop() else {
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
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()
        .expect("a display filter");
    capture(&filter, config).expect("capture to start")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

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
}
