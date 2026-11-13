use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_proto::frame::Flags;
use throcc_proto::messages::{Recovery, ShareConfig, ShareTracks};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use super::capture::{Capture, CaptureConfig, CaptureError, FrameLimiter};
use super::engine::{Kind, MediaEngine, Outgoing, Received};
use super::share_audio::{Playout, STEREO_FRAME, ShareAudioEncoder, due_frames};
use super::video::{Picture, PreviewDemand};
#[cfg(target_os = "macos")]
use throcc_video::PixelBuffer;
#[cfg(target_os = "linux")]
use throcc_video::Refusal;
use throcc_video::frame::{VideoFrame, share_size};
use throcc_video::{
    DecodeError, Decoder, EncodedUnit, Encoder, EncoderConfig, Frame, MaxSize, Output,
};

const IDLE: Duration = Duration::from_millis(2);
/// A share whose screen stands still sends its last picture again this often, so viewers
/// keep getting pictures and a keyframe asked for comes out with the next one.
const STILL_REPEAT: Duration = Duration::from_millis(500);
/// How long a share's decode thread sleeps between looks while it holds no picture.
const WAITING: Duration = Duration::from_millis(10);
/// Units waiting to be decoded past which decoding has fallen behind, half a second at
/// 60 fps. They are dropped, and decoding goes on from the newest keyframe among them or
/// the next one.
const BACKLOG: usize = 30;
/// Self-view gets every Nth frame.
const SELF_VIEW_EVERY: u32 = 2;
/// The widest a self-view picture held in memory is shrunk to. A macOS pixel buffer is shown
/// whole.
const SELF_VIEW_WIDTH: usize = 960;

/// Forced recoveries are at least this far apart, so a link too slow for whole keyframes
/// gets one a second rather than one every frame.
const RECOVERY_SPACING: Duration = Duration::from_secs(1);
const TALLY_EVERY: Duration = Duration::from_secs(5);
/// A keyframe request still unanswered by a decoded picture is repeated after this long.
const KEYFRAME_RETRY: Duration = Duration::from_secs(1);

/// What the share thread encoded since `since`, logged every [`TALLY_EVERY`].
struct Tally {
    since: Instant,
    frames: u32,
    keyframes: u32,
    bytes: usize,
    /// The capture's count of dropped frames and audio buffers at `since`.
    dropped: u64,
}

impl Tally {
    fn new(dropped: u64) -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            keyframes: 0,
            bytes: 0,
            dropped,
        }
    }

    fn add(&mut self, units: &[EncodedUnit]) {
        self.frames += 1;
        for unit in units {
            self.keyframes += u32::from(unit.keyframe);
            self.bytes += unit.data.len();
        }
    }

    /// Logs the tally if it is due. `dropped` is the capture's count of dropped frames and
    /// audio buffers so far.
    fn report_if_due(&mut self, dropped: u64) {
        let elapsed = self.since.elapsed();
        if elapsed < TALLY_EVERY {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let fps = f64::from(self.frames) / seconds;
        let kbps = self.bytes as f64 * 8.0 / seconds / 1000.0;
        info!(
            fps = format!("{fps:.1}"),
            kbps = kbps.round(),
            keyframes = self.keyframes,
            capture_dropped = dropped.saturating_sub(self.dropped),
            "share encoded"
        );
        *self = Self::new(dropped);
    }
}

/// Merges recovery requests into one per [`RECOVERY_SPACING`]; `Idr` wins over `Refresh`.
#[derive(Default)]
struct RecoveryPacer {
    wanted: Option<Recovery>,
    last: Option<Instant>,
}

impl RecoveryPacer {
    fn ask(&mut self, kind: Recovery) {
        self.wanted = self.wanted.max(Some(kind));
    }

    fn due(&mut self, now: Instant) -> Option<Recovery> {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < RECOVERY_SPACING)
        {
            return None;
        }
        let kind = self.wanted.take()?;
        self.last = Some(now);
        Some(kind)
    }
}

/// Which captured frames a share sends, and which of those also feed the self-view.
struct Admission {
    limiter: FrameLimiter,
    sent: u32,
    preview: Arc<PreviewDemand>,
}

impl Admission {
    /// `None` to skip a frame captured at `timestamp`, else whether it feeds the self-view
    /// too: every [`SELF_VIEW_EVERY`]th one sent, while the self-view is on screen.
    fn admit(&mut self, timestamp: u32) -> Option<bool> {
        if !self.limiter.admits(timestamp) {
            return None;
        }
        self.sent = self.sent.wrapping_add(1);
        Some(self.sent.is_multiple_of(SELF_VIEW_EVERY) && self.preview.wanted())
    }
}

/// Our running share: capture, encode, send.
pub struct ShareSender {
    stop: Arc<AtomicBool>,
    recover: std_mpsc::Sender<Recovery>,
    thread: Option<JoinHandle<()>>,
    pub config: ShareConfig,
    pub encoder: &'static str,
    /// The hardware API or software library encoding the share.
    pub engine: &'static str,
    pub self_view: Picture,
    pub preview: Arc<PreviewDemand>,
}

impl ShareSender {
    /// Encodes and sends what `capture` delivers, opened with [`CaptureConfig::for_share`].
    pub fn start(
        engine: Arc<MediaEngine>,
        tracks: ShareTracks,
        wanted: ShareConfig,
        mut capture: Box<dyn Capture>,
    ) -> Result<Self, ShareError> {
        let capture_config = CaptureConfig::for_share(&wanted);
        let bound = (capture_config.width, capture_config.height);
        let mut frame = VideoFrame::blank(bound.0, bound.1);
        let first = first_frame(capture.as_mut(), &mut frame);
        let (width, height) = match first {
            Some(First::Copied) => share_size((frame.width, frame.height), bound),
            #[cfg(target_os = "linux")]
            Some(First::InGpuMemory(size)) => share_size(size, bound),
            None => bound,
        };
        let encoder = Encoder::open(EncoderConfig {
            width,
            height,
            fps: capture_config.fps,
            kbps: wanted.kbps,
        })
        .map_err(|e| ShareError::Encoder(e.to_string()))?;
        let audio_encoder = if wanted.audio && capture.captures_audio() {
            Some(ShareAudioEncoder::new().map_err(|e| ShareError::Encoder(e.to_string()))?)
        } else {
            None
        };
        let config = ShareConfig {
            codec: encoder.codec(),
            audio: audio_encoder.is_some(),
            width: u16::try_from(width).unwrap_or(wanted.width),
            height: u16::try_from(height).unwrap_or(wanted.height),
            recovery: encoder.recovery(),
            ..wanted
        };
        let name = encoder.name();
        let encoded_by = encoder.engine();
        #[cfg(target_os = "macos")]
        let pixel_buffers = encoder.takes_pixel_buffers();
        let stop = Arc::new(AtomicBool::new(false));
        let (recover, recover_rx) = std_mpsc::channel();
        let (self_tx, self_view) = watch::channel(None);
        let preview = Arc::new(PreviewDemand::default());
        let sending = Sending {
            stop: stop.clone(),
            recover: recover_rx,
            capture,
            encoder,
            audio_encoder,
            engine,
            tracks,
            frame,
            unsent: first.is_some(),
            last: Last::Nothing,
            last_sent: Instant::now(),
            self_view: self_tx,
            admission: Admission {
                limiter: FrameLimiter::new(capture_config.fps),
                sent: 0,
                preview: preview.clone(),
            },
            tally: Tally::new(0),
            #[cfg(target_os = "macos")]
            pixel_buffers,
        };
        let thread = std::thread::Builder::new()
            .name("throcc-share".into())
            .spawn(move || sending.run())
            .map_err(|e| ShareError::Encoder(e.to_string()))?;
        Ok(Self {
            stop,
            recover,
            thread: Some(thread),
            config,
            encoder: name,
            engine: encoded_by,
            self_view,
            preview,
        })
    }

    pub fn recover(&self, kind: Recovery) {
        self.recover.send(kind).ok();
    }

    /// Whether sending stopped on its own, as when the shared window or screen went away.
    pub fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for ShareSender {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// The share thread: encodes what the capture delivers and queues it for sending.
struct Sending {
    stop: Arc<AtomicBool>,
    recover: std_mpsc::Receiver<Recovery>,
    capture: Box<dyn Capture>,
    encoder: Encoder,
    audio_encoder: Option<ShareAudioEncoder>,
    engine: Arc<MediaEngine>,
    tracks: ShareTracks,
    frame: VideoFrame,
    /// Whether the first captured picture, in `frame` or still held by the capture in GPU
    /// memory, has not been sent yet.
    unsent: bool,
    /// Where the last picture sent is, to send it again while the screen stands still.
    last: Last,
    /// When a picture was last given to the encoder.
    last_sent: Instant,
    self_view: watch::Sender<Option<Arc<Frame>>>,
    admission: Admission,
    tally: Tally,
    /// Whether captured pixel buffers go to the encoder uncopied.
    #[cfg(target_os = "macos")]
    pixel_buffers: bool,
}

/// Where the last picture sent is kept.
enum Last {
    Nothing,
    /// In [`Sending::frame`].
    Memory,
    /// In the encoder, which took it in GPU memory.
    #[cfg(target_os = "linux")]
    Gpu,
    #[cfg(target_os = "macos")]
    PixelBuffer(PixelBuffer),
}

impl Sending {
    fn run(mut self) {
        let mut sound = [0.0f32; STEREO_FRAME];
        let mut pacer = RecoveryPacer::default();
        while !self.stop.load(Ordering::Relaxed) && !self.capture.ended() {
            while let Ok(kind) = self.recover.try_recv() {
                pacer.ask(kind);
            }
            if let Some(kind) = pacer.due(Instant::now()) {
                self.encoder.recover(kind);
            }
            self.send_audio(&mut sound);
            self.tally.report_if_due(self.capture.dropped());
            #[cfg(target_os = "linux")]
            if self.send_dmabuf() {
                continue;
            }
            #[cfg(target_os = "macos")]
            if self.pixel_buffers && !self.unsent && self.send_pixel_buffer() {
                continue;
            }
            let fresh =
                std::mem::take(&mut self.unsent) || self.capture.next_frame(&mut self.frame);
            if !fresh {
                if self.last_sent.elapsed() >= STILL_REPEAT {
                    self.send_again();
                } else {
                    std::thread::sleep(IDLE);
                }
                continue;
            }
            let Some(self_view) = self.admission.admit(self.frame.timestamp) else {
                continue;
            };
            if self_view {
                let factor = self.frame.width.div_ceil(SELF_VIEW_WIDTH);
                let thumbnail = Frame::Planes(self.frame.shrunk(factor));
                self.self_view.send_replace(Some(Arc::new(thumbnail)));
            }
            self.send_video();
        }
    }

    /// Encodes the next frame if it is in GPU memory. On failure, asks the capture for
    /// no more like it, so frames arrive in shared memory instead. True if there was one.
    #[cfg(target_os = "linux")]
    fn send_dmabuf(&mut self) -> bool {
        let frame = if self.unsent {
            self.capture.current_dmabuf()
        } else {
            self.capture.next_dmabuf()
        };
        let Some(frame) = frame else {
            return false;
        };
        self.unsent = false;
        let modifier = frame.modifier;
        let Some(self_view) = self.admission.admit(frame.timestamp) else {
            return true;
        };
        let preview = self_view.then_some(SELF_VIEW_WIDTH);
        self.last_sent = Instant::now();
        match self.encoder.encode_dmabuf(frame, preview) {
            Ok(encoded) => {
                self.last = Last::Gpu;
                self.send_units(encoded.units);
                match encoded.preview {
                    Some(Ok(preview)) => {
                        self.self_view
                            .send_replace(Some(Arc::new(Frame::Planes(preview))));
                    }
                    Some(Err(e)) => warn!("no self-view picture: {e}"),
                    None => {}
                }
            }
            Err(e) => {
                // The encoder's last picture is older than the one that failed.
                if matches!(self.last, Last::Gpu) {
                    self.last = Last::Nothing;
                }
                match e.refusal(modifier) {
                    Some(refused) => {
                        warn!(?refused, "falling back to frames in shared memory: {e}");
                        self.capture.refuse_dmabuf(match refused {
                            Refusal::Modifier(modifier) => Some(modifier),
                            Refusal::All => None,
                        });
                    }
                    None => warn!("encoding failed: {e}"),
                }
            }
        }
        true
    }

    /// Encodes the next frame from the pixel buffer it was captured into, uncopied. True if
    /// there was one.
    #[cfg(target_os = "macos")]
    fn send_pixel_buffer(&mut self) -> bool {
        let Some(picture) = self.capture.next_pixel_buffer() else {
            return false;
        };
        let Some(self_view) = self.admission.admit(picture.timestamp) else {
            // Skipped for the frame rate, but the newest picture, to send if the screen stops.
            self.last = Last::PixelBuffer(picture);
            return true;
        };
        if self_view {
            let shown = Frame::PixelBuffer(picture.clone());
            self.self_view.send_replace(Some(Arc::new(shown)));
        }
        let encoded = self.encoder.encode_pixel_buffer(&picture);
        self.last = Last::PixelBuffer(picture);
        self.last_sent = Instant::now();
        match encoded {
            Ok(units) => self.send_units(units),
            Err(e) => warn!("encoding failed: {e}"),
        }
        true
    }

    fn send_audio(&mut self, sound: &mut [f32; STEREO_FRAME]) {
        let Some(audio_encoder) = &mut self.audio_encoder else {
            return;
        };
        while let Some(timestamp) = self.capture.next_audio(sound) {
            if let Some(packet) = audio_encoder.encode(sound) {
                let data = Bytes::copy_from_slice(packet);
                let unit = Outgoing::new(self.tracks.audio, data, timestamp, false);
                self.engine.send(Kind::Audio, unit);
            }
        }
    }

    fn send_video(&mut self) {
        self.last = Last::Memory;
        self.last_sent = Instant::now();
        match self.encoder.encode(&self.frame) {
            Ok(units) => self.send_units(units),
            Err(e) => warn!("encoding failed: {e}"),
        }
    }

    /// Encodes the last picture sent again, stamped now.
    fn send_again(&mut self) {
        self.last_sent = Instant::now();
        if matches!(self.last, Last::Nothing) {
            return;
        }
        let Some(timestamp) = self.capture.repeat_timestamp() else {
            return;
        };
        let encoded = match &self.last {
            Last::Nothing => return,
            Last::Memory => {
                self.frame.timestamp = timestamp;
                self.encoder.encode(&self.frame)
            }
            #[cfg(target_os = "linux")]
            Last::Gpu => self.encoder.encode_gpu_again(timestamp),
            #[cfg(target_os = "macos")]
            Last::PixelBuffer(picture) => {
                let again = PixelBuffer {
                    buffer: picture.buffer.clone(),
                    timestamp,
                };
                self.encoder.encode_pixel_buffer(&again)
            }
        };
        match encoded {
            Ok(units) => self.send_units(units),
            Err(e) => warn!("encoding the still picture again failed: {e}"),
        }
    }

    fn send_units(&mut self, units: Vec<EncodedUnit>) {
        self.tally.add(&units);
        for unit in units {
            let outgoing =
                Outgoing::new(self.tracks.video, unit.data, unit.timestamp, unit.keyframe);
            self.engine.send(Kind::Video, outgoing);
        }
    }
}

/// How long a share waits for its first frame to learn the screen's size.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(2);

/// The capture's first frame: copied out, or left in GPU memory with only its size kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum First {
    Copied,
    #[cfg(target_os = "linux")]
    InGpuMemory((usize, usize)),
}

/// Waits for the capture's first frame, which tells the size of what is shared.
fn first_frame(capture: &mut dyn Capture, into: &mut VideoFrame) -> Option<First> {
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    while Instant::now() < deadline {
        #[cfg(target_os = "linux")]
        if let Some(frame) = capture.next_dmabuf() {
            return Some(First::InGpuMemory((
                frame.width as usize,
                frame.height as usize,
            )));
        }
        if capture.next_frame(into) {
            return Some(First::Copied);
        }
        std::thread::sleep(IDLE);
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("video encoder: {0}")]
    Encoder(String),
}

/// Decoded frames held back for the share audio, at most.
const MAX_HELD: usize = 32;
/// Bytes of decoded frames held back for the share audio, at most.
const MAX_HELD_BYTES: usize = 256 << 20;

/// The largest decoded picture a share announced as `config` may have.
fn max_size(config: &ShareConfig) -> MaxSize {
    MaxSize::for_share(config.width.into(), config.height.into())
}

/// The bytes of an NV12 picture of `frame`'s size, wherever it is held.
fn frame_bytes(frame: &Frame) -> usize {
    let (width, height) = frame.size();
    width * height + 2 * width.div_ceil(2) * height.div_ceil(2)
}

/// Drops the oldest held frames until one of `incoming` bytes fits both bounds.
fn make_room(held: &mut VecDeque<Arc<Frame>>, incoming: usize) {
    let mut bytes: usize = held.iter().map(|f| frame_bytes(f)).sum();
    while held.len() >= MAX_HELD
        || (!held.is_empty() && bytes.saturating_add(incoming) > MAX_HELD_BYTES)
    {
        if let Some(oldest) = held.pop_front() {
            bytes -= frame_bytes(&oldest);
        }
    }
}

/// When to ask the sender for a keyframe while decoding its share.
#[derive(Default)]
struct KeyframeAsks {
    unanswered_since: Option<Instant>,
}

impl KeyframeAsks {
    /// Whether to ask at `now`, after a decoder `reset` or a unit that `failed`.
    fn ask(&mut self, now: Instant, reset: bool, failed: bool) -> bool {
        let waiting = self
            .unanswered_since
            .is_some_and(|at| now.duration_since(at) < KEYFRAME_RETRY);
        let ask = reset || (failed && !waiting);
        if ask {
            self.unanswered_since = Some(now);
        }
        ask
    }

    /// Notes a unit decoded with its references intact, which answers any request.
    fn answered(&mut self) {
        self.unanswered_since = None;
    }
}

/// What a share's decode thread reports.
pub struct Receiving {
    pub picture: Picture,
    /// What decodes the share, as people see it named.
    pub engine: watch::Receiver<&'static str>,
    /// How many times decoding fell so far behind that units waiting for it were dropped.
    pub fell_behind: Arc<AtomicU64>,
}

/// Decodes a share's `units` on its own thread into its newest picture due by `playout`,
/// shown in `output` on the phones. `ask_idr` runs after a reset, and on failures at most
/// once per [`KEYFRAME_RETRY`] until a picture decodes. `first_picture` runs once the first
/// picture is out.
pub fn receive(
    mut units: mpsc::Receiver<Received>,
    config: ShareConfig,
    playout: Option<Arc<Playout>>,
    output: Output,
    ask_idr: impl Fn() + Send + 'static,
    first_picture: impl FnOnce() + Send + 'static,
) -> Result<Receiving, String> {
    let mut decoder =
        Decoder::new(config.codec, max_size(&config), output).map_err(|e| e.to_string())?;
    let (tx, picture) = watch::channel(None);
    let (engine_tx, engine) = watch::channel(decoder.engine());
    let fell_behind = Arc::new(AtomicU64::new(0));
    let falls = fell_behind.clone();
    std::thread::Builder::new()
        .name("throcc-decode".into())
        .spawn(move || {
            let mut first_picture = Some(first_picture);
            let mut held: VecDeque<Arc<Frame>> = VecDeque::new();
            let mut replaced: Option<Arc<Frame>> = None;
            let mut asks = KeyframeAsks::default();
            // Set while the decoder lacks the references anything but a keyframe needs.
            let mut awaiting_keyframe = false;
            // Units taken from the channel to be decoded before any still in it.
            let mut queued: VecDeque<Received> = VecDeque::new();
            let mut oversized: u64 = 0;
            loop {
                // A sender sends nothing while its screen stands still, so the loop keeps
                // turning without units to follow the app's view and show the picture again
                // in a new one.
                if decoder.follow_view() {
                    let shown: Option<Arc<Frame>> = tx.borrow().clone();
                    let flushed = decoder.take_flushed();
                    awaiting_keyframe |= flushed;
                    let blank = shown.as_ref().is_some_and(|frame| !frame.shows_again());
                    if (flushed || blank) && asks.ask(Instant::now(), true, false) {
                        ask_idr();
                    }
                    if let Some(shown) = shown {
                        decoder.present(&shown);
                    }
                }
                if units.len() >= BACKLOG {
                    let mut backlog: Vec<Received> = queued
                        .drain(..)
                        .chain(std::iter::from_fn(|| units.try_recv().ok()))
                        .collect();
                    let newest_keyframe = backlog
                        .iter()
                        .rposition(|unit| unit.flags.contains(Flags::KEYFRAME));
                    let dropped = newest_keyframe.unwrap_or(backlog.len());
                    match newest_keyframe {
                        Some(at) => queued.extend(backlog.drain(at..)),
                        None => {
                            awaiting_keyframe = true;
                            if asks.ask(Instant::now(), false, true) {
                                ask_idr();
                            }
                        }
                    }
                    if dropped > 0 {
                        falls.fetch_add(1, Ordering::Relaxed);
                        warn!(
                            dropped,
                            "decoding fell behind; dropped the units waiting for it"
                        );
                    }
                }
                let unit = match queued.pop_front().map_or_else(|| units.try_recv(), Ok) {
                    Ok(unit) => Some(unit),
                    Err(mpsc::error::TryRecvError::Empty) => {
                        if tx.is_closed() {
                            return;
                        }
                        // Units have stopped coming, so pictures still being decoded come
                        // out now.
                        loop {
                            match decoder.drain() {
                                Ok(Some(frame)) => {
                                    if !awaiting_keyframe {
                                        asks.answered();
                                    }
                                    make_room(&mut held, frame_bytes(&frame));
                                    held.push_back(Arc::new(frame));
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    warn!("decoding failed: {e}");
                                    break;
                                }
                            }
                        }
                        std::thread::sleep(if held.is_empty() { WAITING } else { IDLE });
                        None
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => return,
                };
                let unit = unit.filter(|unit| {
                    let skipped = awaiting_keyframe && !unit.flags.contains(Flags::KEYFRAME);
                    if skipped && asks.ask(Instant::now(), false, true) {
                        ask_idr();
                    }
                    !skipped
                });
                if let Some(unit) = &unit {
                    let decoded = decoder.decode(&unit.data, unit.timestamp);
                    let flushed = decoder.take_flushed();
                    // Only a keyframe that came out as a picture shows the references whole.
                    let keyframe_shown =
                        unit.flags.contains(Flags::KEYFRAME) && matches!(decoded, Ok(Some(_)));
                    awaiting_keyframe = flushed && !keyframe_shown;
                    let engine_changed = *engine_tx.borrow() != decoder.engine();
                    if engine_changed {
                        engine_tx.send_replace(decoder.engine());
                    }
                    // A keyframe wouldn't help a picture too large: the sender chose the size.
                    let failed =
                        flushed || !matches!(decoded, Ok(_) | Err(DecodeError::TooLarge(..)));
                    if asks.ask(Instant::now(), engine_changed, failed) {
                        ask_idr();
                    }
                    match decoded {
                        Ok(Some(frame)) => {
                            if !flushed {
                                asks.answered();
                            }
                            make_room(&mut held, frame_bytes(&frame));
                            held.push_back(Arc::new(frame));
                        }
                        Ok(None) => {}
                        Err(DecodeError::TooLarge(width, height)) => {
                            oversized += 1;
                            if oversized.is_power_of_two() {
                                warn!(
                                    width,
                                    height,
                                    oversized,
                                    "dropped pictures larger than the share announced"
                                );
                            }
                        }
                        Err(e) => warn!("decoding failed: {e}"),
                    }
                }
                let heard = playout.as_ref().and_then(|p| p.heard());
                let due = due_frames(held.iter().map(|f| f.timestamp()), heard);
                let mut drained = held.drain(..due);
                let Some(newest) = drained.next_back() else {
                    continue;
                };
                let superseded = drained.next();
                drop(drained);
                if tx.is_closed() {
                    return;
                }
                let unused = superseded.or(replaced.take());
                let unused = unused.and_then(|frame| Arc::try_unwrap(frame).ok());
                if let Some(frame) = unused.and_then(Frame::into_planes) {
                    decoder.recycle(frame);
                }
                decoder.present(&newest);
                replaced = tx.send_replace(Some(newest));
                if let Some(first_picture) = first_picture.take() {
                    first_picture();
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Receiving {
        picture,
        engine,
        fell_behind,
    })
}

#[cfg(test)]
mod pacer_tests {
    use super::*;

    #[test]
    fn recoveries_merge_until_a_second_has_passed() {
        let start = Instant::now();
        let mut pacer = RecoveryPacer::default();
        assert_eq!(pacer.due(start), None);
        pacer.ask(Recovery::Refresh);
        assert_eq!(pacer.due(start), Some(Recovery::Refresh));
        pacer.ask(Recovery::Idr);
        pacer.ask(Recovery::Refresh);
        assert_eq!(pacer.due(start + Duration::from_millis(500)), None);
        assert_eq!(pacer.due(start + RECOVERY_SPACING), Some(Recovery::Idr));
        assert_eq!(pacer.due(start + RECOVERY_SPACING * 3), None);
    }
}

#[cfg(test)]
mod receive_tests {
    use throcc_proto::messages::Codec;

    use super::*;

    #[test]
    fn failures_ask_again_only_once_a_keyframe_is_overdue() {
        let start = Instant::now();
        let after = |millis| start + Duration::from_millis(millis);
        let mut asks = KeyframeAsks::default();
        assert!(!asks.ask(start, false, false));
        assert!(asks.ask(start, false, true));
        assert!(!asks.ask(after(500), false, true));
        assert!(asks.ask(after(500), true, false));
        assert!(!asks.ask(after(1_400), false, true));
        assert!(asks.ask(after(1_500), false, true));
        asks.answered();
        assert!(asks.ask(after(1_600), false, true));
    }

    fn config(width: u16, height: u16) -> ShareConfig {
        ShareConfig {
            audio: false,
            kbps: 4000,
            codec: Codec::Vp9,
            width,
            height,
            fps: 30,
            recovery: Recovery::Idr,
        }
    }

    #[test]
    fn decoded_pictures_are_bounded_by_the_announced_size() {
        let sides = |width, height| max_size(&config(width, height)).sides();
        assert_eq!(sides(1280, 720), (1280 + 64, 720 + 64));
        assert_eq!(
            sides(u16::MAX, 16),
            MaxSize::new(usize::MAX, 16 + 64).sides()
        );
        assert_eq!(sides(u16::MAX, u16::MAX), MaxSize::ANY.sides());
        assert_eq!(sides(0, 0), (64, 64));
        assert_eq!(max_size(&config(1920, 1080)).announced(), (1920, 1080));
    }

    #[test]
    fn held_frames_stay_within_count_and_bytes() {
        let mut held = VecDeque::new();
        let small = Arc::new(Frame::Planes(VideoFrame::blank(16, 16)));
        for _ in 0..MAX_HELD + 5 {
            make_room(&mut held, frame_bytes(&small));
            held.push_back(small.clone());
        }
        assert_eq!(held.len(), MAX_HELD);

        let big = Arc::new(Frame::Planes(VideoFrame::blank(4096, 4096)));
        for _ in 0..MAX_HELD {
            make_room(&mut held, frame_bytes(&big));
            held.push_back(big.clone());
        }
        let bytes: usize = held.iter().map(|f| frame_bytes(f)).sum();
        assert!(bytes <= MAX_HELD_BYTES, "{bytes} bytes held");
        assert_eq!(held.len(), MAX_HELD_BYTES / frame_bytes(&big));

        make_room(&mut held, usize::MAX);
        assert!(held.is_empty());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::media::capture::macos::main_display;

    fn cpu_time() -> Duration {
        // SAFETY: getrusage only writes the struct it is given.
        let usage = unsafe {
            let mut usage = std::mem::zeroed::<libc::rusage>();
            libc::getrusage(libc::RUSAGE_SELF, &mut usage);
            usage
        };
        let seconds = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
        seconds(usage.ru_utime) + seconds(usage.ru_stime)
    }

    #[test]
    #[ignore = "needs a display, the screen recording permission and moving content on it"]
    fn sending_1440p60_takes_under_one_core() {
        let config = CaptureConfig {
            width: 2560,
            height: 1440,
            fps: 60,
            audio: false,
        };
        let mut capture = main_display(config);
        let mut encoder = Encoder::open(EncoderConfig {
            width: config.width,
            height: config.height,
            fps: config.fps,
            kbps: 10_000,
        })
        .unwrap();
        let mut frame = VideoFrame::blank(config.width, config.height);
        let (mut frames, mut bytes) = (0u32, 0usize);
        std::thread::sleep(Duration::from_millis(500));
        while capture.next_frame(&mut frame) {}
        let (started, cpu_before) = (Instant::now(), cpu_time());
        while started.elapsed() < Duration::from_secs(5) {
            if !capture.next_frame(&mut frame) {
                std::thread::sleep(IDLE);
                continue;
            }
            frames += 1;
            bytes += encoder
                .encode(&frame)
                .unwrap()
                .iter()
                .map(|u| u.data.len())
                .sum::<usize>();
        }
        let wall = started.elapsed();
        let cores = (cpu_time() - cpu_before).as_secs_f64() / wall.as_secs_f64();
        let fps = f64::from(frames) / wall.as_secs_f64();
        let mbps = bytes as f64 * 8.0 / wall.as_secs_f64() / 1e6;
        eprintln!(
            "{} at {fps:.1} fps, {mbps:.1} Mbps, {cores:.2} cores",
            encoder.name()
        );
        assert!(fps > 50.0, "only {fps:.1} fps; is the screen moving?");
        assert!(cores < 1.0, "{cores:.2} cores");
    }
}
