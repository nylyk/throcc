#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod screen_capture_kit;

use std::cell::RefCell;
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::NSError;
#[cfg(target_os = "macos")]
use objc2_foundation::NSString;
use throcc_video::frame::{VideoFrame, share_size};

use super::clock::{ShareClock, clock_nanos};
use super::{Capture, CaptureConfig, CaptureError, PixelBuffer};
#[cfg(target_os = "macos")]
use super::{pack_shape, unpack_shape};
use crate::media::audio::SAMPLE_RATE;
use crate::media::share_audio::STEREO_FRAME;
use screen_capture_kit::{
    OUTPUT_AUDIO, OUTPUT_SCREEN, SCContentFilter, SCContentSharingPicker,
    SCContentSharingPickerObserver, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput,
};

/// Captured frames waiting for the share thread. A full ring drops the newest frame.
const RING: usize = 4;
/// Captured audio buffers waiting for the share thread, about 20 ms each.
const AUDIO_RING: usize = 32;
const NANOS: i128 = 1_000_000_000;

/// A captured picture with its host time in nanoseconds.
type Picture = (CFRetained<CVPixelBuffer>, i64);
/// A captured audio buffer with its host time in nanoseconds.
type Sound = (CFRetained<CMSampleBuffer>, i64);

pub struct AppleCapture {
    stream: Retained<SCStream>,
    /// The stream's configuration, which a turn changes the size of.
    #[cfg(target_os = "macos")]
    config: Retained<SCStreamConfiguration>,
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "held for as long as the stream sends to it")
    )]
    output: Retained<Output>,
    _queues: Vec<DispatchRetained<DispatchQueue>>,
    frames: rtrb::Consumer<Picture>,
    audio: Option<rtrb::Consumer<Sound>>,
    dropped: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
    /// Counts from host time, in nanoseconds.
    clock: ShareClock,
}

// SAFETY: the stream, its output and their queues are Objective-C objects ScreenCaptureKit
// documents as usable from any thread; from here they are only stopped and released. The
// pixel and sample buffers are reference counted thread-safely.
unsafe impl Send for AppleCapture {}

pub fn start(config: CaptureConfig) -> Result<AppleCapture, CaptureError> {
    let (filter, shape) = pick()?;
    capture(&filter, shape, config)
}

/// Captures `filter`, whose content is `shape` pixels, at that aspect ratio fitted in the
/// config's size and never larger than the content.
fn capture(
    filter: &SCContentFilter,
    shape: (usize, usize),
    config: CaptureConfig,
) -> Result<AppleCapture, CaptureError> {
    let (width, height) = share_size(shape, (config.width, config.height));
    let stream_config = SCStreamConfiguration::new();
    stream_config.set_width(width);
    stream_config.set_height(height);
    stream_config.set_captures_audio(config.audio);
    stream_config.set_sample_rate(SAMPLE_RATE as isize);
    stream_config.set_channel_count(2);
    stream_config.set_excludes_current_process_audio(true);
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a plain CMTime constructor.
        let interval = unsafe { objc2_core_media::CMTime::new(1, config.fps.max(1) as i32) };
        stream_config.set_pixel_format(throcc_video::FULL_RANGE_NV12);
        stream_config.set_minimum_frame_interval(interval);
        stream_config.set_queue_depth(8);
        stream_config.set_shows_cursor(true);
    }

    let (frames_in, frames) = rtrb::RingBuffer::new(RING);
    let (audio_in, audio) = if config.audio {
        let (producer, consumer) = rtrb::RingBuffer::new(AUDIO_RING);
        (Some(Mutex::new(producer)), Some(consumer))
    } else {
        (None, None)
    };
    let dropped = Arc::new(AtomicU64::new(0));
    let ended = Arc::new(AtomicBool::new(false));
    let output = Output::new(Outputs {
        frames: Mutex::new(frames_in),
        audio: audio_in,
        dropped: dropped.clone(),
        ended: ended.clone(),
        #[cfg(target_os = "macos")]
        shape: AtomicU64::new(0),
    });
    let stream = SCStream::init_with_filter(
        SCStream::alloc(),
        filter,
        &stream_config,
        Some(ProtocolObject::from_ref(&*output)),
    );
    let mut queues = vec![add_output(&stream, &output, OUTPUT_SCREEN)?];
    if config.audio {
        queues.push(add_output(&stream, &output, OUTPUT_AUDIO)?);
    }
    start_capture(&stream)?;
    Ok(AppleCapture {
        stream,
        #[cfg(target_os = "macos")]
        config: stream_config,
        output,
        _queues: queues,
        frames,
        audio,
        dropped,
        ended,
        // CLOCK_UPTIME_RAW is the clock mach_absolute_time counts, which the host clock is.
        clock: ShareClock::new(NANOS as i64, || clock_nanos(libc::CLOCK_UPTIME_RAW)),
    })
}

/// Has `output` take the stream's samples of `kind` on a serial queue of their own.
fn add_output(
    stream: &SCStream,
    output: &Output,
    kind: isize,
) -> Result<DispatchRetained<DispatchQueue>, CaptureError> {
    let queue = DispatchQueue::new("cc.thro.throcc.capture", DispatchQueueAttr::SERIAL);
    // SAFETY: the queue is serial.
    unsafe { stream.add_stream_output(ProtocolObject::from_ref(output), kind, Some(&queue)) }
        .map_err(|e| ns_failed(&e))?;
    Ok(queue)
}

/// Starts `stream` and waits until it runs or fails.
fn start_capture(stream: &SCStream) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: ScreenCaptureKit passes a live error or none.
        tx.send(unsafe { error.as_ref() }.map(ns_failed)).ok();
    });
    stream.start_capture(Some(&handler));
    match rx.recv() {
        Ok(None) => Ok(()),
        Ok(Some(e)) => Err(e),
        Err(_) => Err(CaptureError::Failed("the capture never started".into())),
    }
}

fn ns_failed(error: &NSError) -> CaptureError {
    CaptureError::Failed(error.localizedDescription().to_string())
}

struct Outputs {
    frames: Mutex<rtrb::Producer<Picture>>,
    audio: Option<Mutex<rtrb::Producer<Sound>>>,
    dropped: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
    /// The pixel size of what the last frame shows, packed by [`pack_shape`].
    #[cfg(target_os = "macos")]
    shape: AtomicU64,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Output` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "ThroccCaptureOutput"]
    #[ivars = Outputs]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: isize) {
            self.queue(sample, kind);
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, _error: &NSError) {
            self.ivars().ended.store(true, Ordering::Relaxed);
        }
    }
);

impl Output {
    fn new(outputs: Outputs) -> Retained<Self> {
        let this = Self::alloc().set_ivars(outputs);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Queues a sample with its host time; a full ring counts a drop. Each kind arrives on
    /// one serial queue of its own, so `try_lock` never finds its ring taken: the lock only
    /// makes it shareable without ever blocking.
    fn queue(&self, sample: &CMSampleBuffer, kind: isize) {
        let outputs = self.ivars();
        let at = host_nanos(sample);
        let pushed = match kind {
            OUTPUT_SCREEN => {
                // SAFETY: `sample` is a live sample buffer.
                let Some(picture) = (unsafe { sample.image_buffer() }) else {
                    return;
                };
                #[cfg(target_os = "macos")]
                if let Some(shape) = content_shape(sample) {
                    outputs.shape.store(pack_shape(shape), Ordering::Relaxed);
                }
                outputs
                    .frames
                    .try_lock()
                    .is_ok_and(|mut frames| frames.push((picture, at)).is_ok())
            }
            OUTPUT_AUDIO => {
                let Some(audio) = &outputs.audio else {
                    return;
                };
                // SAFETY: `sample` is a live sample buffer.
                let sound = unsafe { CFRetained::retain(NonNull::from(sample)) };
                audio
                    .try_lock()
                    .is_ok_and(|mut audio| audio.push((sound, at)).is_ok())
            }
            _ => return,
        };
        if !pushed {
            outputs.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The pixel size of what `sample` shows, as on screen.
#[cfg(target_os = "macos")]
fn content_shape(sample: &CMSampleBuffer) -> Option<(usize, usize)> {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CGRect};
    use screen_capture_kit::{
        CGRectMakeWithDictionaryRepresentation, SCStreamFrameInfoScaleFactor,
        SCStreamFrameInfoScreenRect,
    };
    // SAFETY: `sample` is live; its attachments are a dictionary per sample, whose screen
    // rect is a rect's dictionary and whose scale factor is a number, as ScreenCaptureKit
    // documents them.
    unsafe {
        let attachments = sample.sample_attachments_array(false)?;
        if attachments.count() < 1 {
            return None;
        }
        let info = attachments
            .value_at_index(0)
            .cast::<CFDictionary>()
            .as_ref()?;
        let key = |name: &NSString| (name as *const NSString).cast();
        let rect = info.value(key(SCStreamFrameInfoScreenRect));
        let mut screen = CGRect::default();
        if rect.is_null() || !CGRectMakeWithDictionaryRepresentation(rect, &mut screen) {
            return None;
        }
        let scale = info
            .value(key(SCStreamFrameInfoScaleFactor))
            .cast::<CFNumber>()
            .as_ref()?;
        let mut factor = 0.0f64;
        if !scale.value(CFNumberType::Float64Type, (&raw mut factor).cast()) {
            return None;
        }
        let side = |points: f64| (points * factor).round().max(0.0) as usize;
        Some((side(screen.size.width), side(screen.size.height)))
    }
}

/// A sample's presentation time on the host clock, in nanoseconds.
fn host_nanos(sample: &CMSampleBuffer) -> i64 {
    // SAFETY: `sample` is a live sample buffer.
    let time = unsafe { sample.presentation_time_stamp() };
    if time.timescale <= 0 {
        return 0;
    }
    (i128::from(time.value) * NANOS / i128::from(time.timescale)) as i64
}

enum Picked {
    Content(Retained<SCContentFilter>),
    Cancelled,
    Failed(String),
}

struct Choice {
    chosen: Mutex<Option<mpsc::SyncSender<Picked>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `PickerObserver` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "ThroccPickerObserver"]
    #[ivars = Choice]
    struct PickerObserver;

    unsafe impl NSObjectProtocol for PickerObserver {}

    unsafe impl SCContentSharingPickerObserver for PickerObserver {
        #[unsafe(method(contentSharingPicker:didCancelForStream:))]
        fn did_cancel(&self, _picker: &SCContentSharingPicker, _stream: Option<&SCStream>) {
            self.finish(Picked::Cancelled);
        }

        #[unsafe(method(contentSharingPicker:didUpdateWithFilter:forStream:))]
        fn did_update(
            &self,
            _picker: &SCContentSharingPicker,
            filter: &SCContentFilter,
            _stream: Option<&SCStream>,
        ) {
            self.finish(Picked::Content(filter.retain()));
        }

        #[unsafe(method(contentSharingPickerStartDidFailWithError:))]
        fn did_fail(&self, error: &NSError) {
            self.finish(Picked::Failed(error.localizedDescription().to_string()));
        }
    }
);

thread_local! {
    /// The observer of the picker on screen. Only the main thread uses it.
    static PICKING: RefCell<Option<Retained<PickerObserver>>> = const { RefCell::new(None) };
}

impl PickerObserver {
    fn new(chosen: mpsc::SyncSender<Picked>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Choice {
            chosen: Mutex::new(Some(chosen)),
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Hands the person's choice to whoever waits for it, the first time only, and puts the
    /// picker away.
    fn finish(&self, picked: Picked) {
        let chosen = self.ivars().chosen.lock().ok().and_then(|mut c| c.take());
        if let Some(chosen) = chosen {
            chosen.send(picked).ok();
        }
        let this = self as *const Self as usize;
        DispatchQueue::main().exec_async(move || put_away(this));
    }
}

/// Shows `SCContentSharingPicker` and waits for the person's choice; returns it with its
/// size in pixels.
fn pick() -> Result<(Retained<SCContentFilter>, (usize, usize)), CaptureError> {
    let (tx, rx) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || present(tx));
    match rx.recv() {
        Ok(Picked::Content(filter)) => {
            let rect = filter.content_rect();
            let scale = f64::from(filter.point_pixel_scale());
            let shape = (
                (rect.size.width * scale).round() as usize,
                (rect.size.height * scale).round() as usize,
            );
            Ok((filter, shape))
        }
        Ok(Picked::Cancelled) | Err(_) => Err(CaptureError::Cancelled),
        Ok(Picked::Failed(message)) => Err(CaptureError::Failed(message)),
    }
}

/// Runs on the main thread: replaces any picker still open with a new one.
fn present(chosen: mpsc::SyncSender<Picked>) {
    let picker = SCContentSharingPicker::shared();
    let observer = PickerObserver::new(chosen);
    if let Some(old) = PICKING.replace(Some(observer.clone())) {
        picker.remove_observer(ProtocolObject::from_ref(&*old));
        old.finish(Picked::Cancelled);
    }
    picker.add_observer(ProtocolObject::from_ref(&*observer));
    #[cfg(target_os = "macos")]
    {
        use screen_capture_kit::{SCContentSharingPickerConfiguration, picker_mode};
        let config = SCContentSharingPickerConfiguration::new();
        config.set_allowed_picker_modes(
            picker_mode::SINGLE_DISPLAY
                | picker_mode::SINGLE_WINDOW
                | picker_mode::SINGLE_APPLICATION,
        );
        picker.set_default_configuration(&config);
    }
    picker.set_active(true);
    picker.present();
}

/// Runs on the main thread: stops observing the picker once `observer` has its answer,
/// unless a newer picker took over.
fn put_away(observer: usize) {
    let current = PICKING.with_borrow(|picking| {
        picking
            .as_ref()
            .is_some_and(|p| &**p as *const PickerObserver as usize == observer)
    });
    if !current {
        return;
    }
    if let Some(done) = PICKING.take() {
        let picker = SCContentSharingPicker::shared();
        picker.remove_observer(ProtocolObject::from_ref(&*done));
        picker.set_active(false);
    }
}

impl AppleCapture {
    fn pop(&mut self) -> Option<PixelBuffer> {
        let (buffer, at) = self.frames.pop().ok()?;
        let timestamp = self.clock.video(at)?;
        Some(PixelBuffer { buffer, timestamp })
    }
}

impl Capture for AppleCapture {
    fn repeat_timestamp(&mut self) -> Option<u32> {
        self.clock.repeat()
    }

    #[cfg(target_os = "macos")]
    fn shape(&self) -> Option<(usize, usize)> {
        unpack_shape(self.output.ivars().shape.load(Ordering::Relaxed))
    }

    #[cfg(target_os = "macos")]
    fn reshape(&mut self, (width, height): (usize, usize)) -> Result<(), CaptureError> {
        self.config.set_width(width);
        self.config.set_height(height);
        self.stream.update_configuration(&self.config, None);
        Ok(())
    }

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

/// Room for the two buffers of planar stereo, as `AudioBufferList` lays them out.
#[repr(C)]
struct TwoBuffers {
    list: AudioBufferList,
    second: AudioBuffer,
}

/// A buffer of 32-bit float audio, planar or interleaved, as interleaved stereo.
fn stereo(sample: &CMSampleBuffer) -> Vec<f32> {
    let mut out = Vec::new();
    let empty = AudioBuffer {
        mNumberChannels: 0,
        mDataByteSize: 0,
        mData: null_mut(),
    };
    let mut buffers = TwoBuffers {
        list: AudioBufferList {
            mNumberBuffers: 0,
            mBuffers: [empty],
        },
        second: empty,
    };
    let mut block: *mut CMBlockBuffer = null_mut();
    // SAFETY: the list has room for the two buffers its size says, and the block buffer
    // comes back retained.
    let status = unsafe {
        sample.audio_buffer_list_with_retained_block_buffer(
            null_mut(),
            &mut buffers.list,
            size_of::<TwoBuffers>(),
            None,
            None,
            0,
            &mut block,
        )
    };
    // SAFETY: the block buffer came retained, and holds the audio the list points into
    // until it is released at the end of this function.
    let _block = NonNull::new(block).map(|b| unsafe { CFRetained::from_raw(b) });
    if status != 0 {
        return out;
    }
    let [first] = buffers.list.mBuffers;
    // SAFETY: the buffers point into the block buffer, alive until the end of this function.
    let (left, right) = unsafe { (floats(&first), floats(&buffers.second)) };
    match buffers.list.mNumberBuffers {
        2 => {
            for (l, r) in left.into_iter().zip(right) {
                out.extend([l, r]);
            }
        }
        1 if first.mNumberChannels >= 2 => {
            for frame in left.chunks_exact(first.mNumberChannels as usize) {
                if let [l, r, ..] = frame {
                    out.extend([*l, *r]);
                }
            }
        }
        1 => {
            for s in left {
                out.extend([s, s]);
            }
        }
        _ => {}
    }
    out
}

/// The 32-bit floats `buffer` holds.
///
/// # Safety
///
/// `buffer` must point at `mDataByteSize` live bytes, or at none.
unsafe fn floats(buffer: &AudioBuffer) -> Vec<f32> {
    if buffer.mData.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller promises the bytes are live.
    let bytes = unsafe {
        std::slice::from_raw_parts(buffer.mData.cast::<u8>(), buffer.mDataByteSize as usize)
    };
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_ne_bytes(*b))
        .collect()
}

impl Drop for AppleCapture {
    fn drop(&mut self) {
        self.stream.stop_capture(None);
    }
}

/// Captures the main display without asking, for tests on real hardware.
#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::expect_used)]
pub(crate) fn main_display(config: CaptureConfig) -> AppleCapture {
    use objc2_foundation::NSArray;
    use screen_capture_kit::SCShareableContent;

    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |content: *mut SCShareableContent, _: *mut NSError| {
        // SAFETY: ScreenCaptureKit passes live content or none.
        let display = unsafe { content.as_ref() }.and_then(|c| c.displays().firstObject());
        let found = display.map(|display| {
            let filter = SCContentFilter::init_with_display(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let scale = f64::from(filter.point_pixel_scale());
            let shape = (
                (display.width() as f64 * scale) as usize,
                (display.height() as f64 * scale) as usize,
            );
            (filter, shape)
        });
        tx.send(found).ok();
    });
    SCShareableContent::get(&handler);
    let (filter, shape) = rx
        .recv()
        .ok()
        .flatten()
        .expect("a display and the screen recording permission");
    capture(&filter, shape, config).expect("capture to start")
}

#[cfg(all(test, target_os = "macos"))]
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
