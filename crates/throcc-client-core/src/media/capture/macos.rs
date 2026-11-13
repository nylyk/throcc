#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;
use screencapturekit::stream::ErrorHandler;
use throcc_video::frame::{VideoFrame, share_size};

use super::clock::ShareClock;
use super::{Capture, CaptureConfig, CaptureError, PixelBuffer, failed};
use crate::media::audio::SAMPLE_RATE;
use crate::media::share_audio::STEREO_FRAME;

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
    ended: Arc<AtomicBool>,
    /// Counts from host time, in nanoseconds.
    clock: ShareClock,
}

// SAFETY: SCStream is a reference to an Objective-C object that ScreenCaptureKit
// documents as usable from any thread; it is only stopped from here.
unsafe impl Send for MacCapture {}

pub fn start(config: CaptureConfig) -> Result<MacCapture, CaptureError> {
    let (filter, shape) = pick()?;
    capture(&filter, shape, config)
}

/// Captures `filter`, whose content is `shape` pixels, at that aspect ratio fitted in the
/// config's size and never larger than the content.
fn capture(
    filter: &SCContentFilter,
    shape: (usize, usize),
    config: CaptureConfig,
) -> Result<MacCapture, CaptureError> {
    let fps = config.fps.max(1);
    let (width, height) = share_size(shape, (config.width, config.height));
    let stream_config = SCStreamConfiguration::new()
        .with_width(width as u32)
        .with_height(height as u32)
        .with_pixel_format(PixelFormat::YCbCr_420f)
        .with_minimum_frame_interval(&CMTime::new(1, fps as i32))
        .with_queue_depth(8)
        .with_shows_cursor(true)
        .with_captures_audio(config.audio)
        .with_sample_rate(SAMPLE_RATE as i32)
        .with_channel_count(2)
        .with_excludes_current_process_audio(true);
    let ended = Arc::new(AtomicBool::new(false));
    let stopped = ended.clone();
    let on_stop = ErrorHandler::new(move |_| stopped.store(true, Ordering::Relaxed));
    let mut stream =
        SCStream::new_with_delegate(filter, &stream_config, on_stop).map_err(failed)?;

    let dropped = Arc::new(AtomicU64::new(0));
    let frames = queue_output(
        &mut stream,
        SCStreamOutputType::Screen,
        RING,
        &dropped,
        |sample: CMSampleBuffer| sample.pixel_buffer(),
    )?;
    let audio = if config.audio {
        Some(queue_output(
            &mut stream,
            SCStreamOutputType::Audio,
            AUDIO_RING,
            &dropped,
            Some,
        )?)
    } else {
        None
    };
    stream.start_capture().map_err(failed)?;
    Ok(MacCapture {
        stream,
        frames,
        audio,
        dropped,
        ended,
        clock: ShareClock::new(NANOS as i64),
    })
}

/// Queues what `take` makes of each sample of `kind`, with its host time, in a ring of
/// `size`; a full ring counts a drop. Handlers run on one serial dispatch queue per kind,
/// so `try_lock` never finds the producer taken: the lock only makes it shareable without
/// ever blocking.
fn queue_output<T: Send + 'static>(
    stream: &mut SCStream,
    kind: SCStreamOutputType,
    size: usize,
    dropped: &Arc<AtomicU64>,
    take: impl Fn(CMSampleBuffer) -> Option<T> + Send + Sync + 'static,
) -> Result<rtrb::Consumer<(T, i64)>, CaptureError> {
    let (producer, consumer) = rtrb::RingBuffer::new(size);
    let producer = std::sync::Mutex::new(producer);
    let dropped = dropped.clone();
    stream
        .add_output_handler(
            move |sample: CMSampleBuffer, _: SCStreamOutputType| {
                let at = host_nanos(&sample);
                let Some(item) = take(sample) else {
                    return;
                };
                let pushed = producer
                    .try_lock()
                    .is_ok_and(|mut p| p.push((item, at)).is_ok());
                if !pushed {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            kind,
        )
        .map_err(failed)?;
    Ok(consumer)
}

/// A sample's presentation time on the host clock, in nanoseconds.
fn host_nanos(sample: &CMSampleBuffer) -> i64 {
    let time = sample.presentation_timestamp();
    if time.timescale <= 0 {
        return 0;
    }
    (i128::from(time.value) * NANOS / i128::from(time.timescale)) as i64
}

/// Shows `SCContentSharingPicker` and waits for the person's choice; returns it with its
/// size in pixels.
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
            let (width, height) = result.pixel_size();
            Ok((result.filter(), (width as usize, height as usize)))
        }
        SCPickerOutcome::Cancelled => Err(CaptureError::Cancelled),
        SCPickerOutcome::Error(message) => Err(CaptureError::Failed(message)),
    }
}

impl MacCapture {
    fn pop(&mut self) -> Option<PixelBuffer> {
        let (buffer, at) = self.frames.pop().ok()?;
        let timestamp = self.clock.video(at)?;
        // SAFETY: `buffer` holds a live CVPixelBufferRef while the call takes its own
        // reference.
        unsafe { PixelBuffer::retaining(buffer.as_ptr(), timestamp) }
    }
}

impl Capture for MacCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        self.pop().is_some_and(|picture| picture.copy_into(into))
    }

    fn next_pixel_buffer(&mut self) -> Option<PixelBuffer> {
        self.pop()
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    fn captures_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        let audio = self.audio.as_mut()?;
        self.clock.next_audio(into, |clock| {
            let (sample, at) = audio.pop().ok()?;
            clock.push_audio(at, stereo(&sample));
            Some(())
        })
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
    let scale = f64::from(filter.point_pixel_scale());
    let shape = (
        (f64::from(display.width()) * scale) as usize,
        (f64::from(display.height()) * scale) as usize,
    );
    capture(&filter, shape, config).expect("capture to start")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::media::capture::{Capture, frames_within};

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
        let timestamps = frames_within(&mut capture, &mut frame, 30);
        assert!(
            timestamps.len() >= 10,
            "only {} frames in 5 s",
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
