use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_proto::messages::{Recovery, Share, ShareConfig};
use tokio::sync::{mpsc, watch};
use tracing::warn;

use super::capture::{Capture, CaptureConfig, CaptureError};
use super::engine::{Kind, MediaEngine, Outgoing, Received};
use super::share_audio::{Playout, STEREO_FRAME, ShareAudioEncoder, due_frames};
use super::video::decode::{Decoder, MAX_PIXELS};
use super::video::encode::{EncodeError, EncodedUnit, Encoder, EncoderConfig};
use super::video::frame::{VideoFrame, share_size};
use super::video::{Picture, PreviewDemand};

const IDLE: Duration = Duration::from_millis(2);
/// Self-view gets every Nth frame, shrunk to about this wide; it is only a thumbnail of
/// what is sent.
const SELF_VIEW_EVERY: u32 = 2;
const SELF_VIEW_WIDTH: usize = 960;

/// Forced recoveries are at least this far apart, so a link too slow for whole keyframes
/// gets one a second rather than one every frame.
const RECOVERY_SPACING: Duration = Duration::from_secs(1);

/// Merges recovery requests into one per [`RECOVERY_SPACING`]; `Idr` wins over `Refresh`.
#[derive(Default)]
struct RecoveryPacer {
    wanted: Option<Recovery>,
    last: Option<Instant>,
}

impl RecoveryPacer {
    fn ask(&mut self, kind: Recovery) {
        let idr = kind == Recovery::Idr || self.wanted == Some(Recovery::Idr);
        self.wanted = Some(if idr { Recovery::Idr } else { kind });
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

/// Our running share: capture, encode, send.
pub struct ShareSender {
    stop: Arc<AtomicBool>,
    recover: std_mpsc::Sender<Recovery>,
    thread: Option<JoinHandle<()>>,
    pub config: ShareConfig,
    pub encoder: &'static str,
    pub self_view: Picture,
    pub preview: Arc<PreviewDemand>,
}

impl ShareSender {
    /// Encodes and sends what `capture` delivers, opened with [`CaptureConfig::for_share`].
    pub fn start(
        engine: Arc<MediaEngine>,
        tracks: Share,
        wanted: ShareConfig,
        mut capture: Box<dyn Capture>,
    ) -> Result<Self, ShareError> {
        let capture_config = CaptureConfig::for_share(&wanted);
        let bound = (capture_config.width, capture_config.height);
        let mut frame = VideoFrame::blank(bound.0, bound.1);
        let first = first_frame(capture.as_mut(), &mut frame);
        let (width, height) = match first {
            Some(First::Copied) => share_size((frame.width, frame.height), bound),
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
            ..wanted
        };
        let name = encoder.name();
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
            unsent: first == Some(First::Copied),
            self_view: self_tx,
            preview: preview.clone(),
            refused: Vec::new(),
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
    tracks: Share,
    frame: VideoFrame,
    /// Whether `frame` holds a captured picture that has not been sent yet.
    unsent: bool,
    self_view: watch::Sender<Option<Arc<VideoFrame>>>,
    preview: Arc<PreviewDemand>,
    /// Modifiers of frames in GPU memory that failed, `None` for all of them.
    refused: Vec<Option<u64>>,
}

impl Sending {
    fn run(mut self) {
        let mut sound = [0.0f32; STEREO_FRAME];
        let mut count = 0u32;
        let mut pacer = RecoveryPacer::default();
        while !self.stop.load(Ordering::Relaxed) && !self.capture.ended() {
            while let Ok(kind) = self.recover.try_recv() {
                pacer.ask(kind);
            }
            if let Some(kind) = pacer.due(Instant::now()) {
                self.encoder.recover(kind);
            }
            self.send_audio(&mut sound);
            #[cfg(target_os = "linux")]
            if self.send_dmabuf(&mut count) {
                continue;
            }
            let fresh =
                std::mem::take(&mut self.unsent) || self.capture.next_frame(&mut self.frame);
            if !fresh {
                std::thread::sleep(IDLE);
                continue;
            }
            count = count.wrapping_add(1);
            if count.is_multiple_of(SELF_VIEW_EVERY) && self.preview.wanted() {
                let factor = self.frame.width / SELF_VIEW_WIDTH;
                let thumbnail = Arc::new(self.frame.shrunk(factor));
                self.self_view.send_replace(Some(thumbnail));
            }
            self.send_video();
        }
    }

    /// Encodes the next frame if it is in GPU memory. On failure, asks the capture for
    /// no more like it, so frames arrive in shared memory instead. True if there was one.
    #[cfg(target_os = "linux")]
    fn send_dmabuf(&mut self, count: &mut u32) -> bool {
        let Some(frame) = self.capture.next_dmabuf() else {
            return false;
        };
        let modifier = frame.modifier;
        if self.refused.contains(&None) || self.refused.contains(&Some(modifier)) {
            return true;
        }
        *count = count.wrapping_add(1);
        match self.encoder.encode_dmabuf(frame) {
            Ok(units) => {
                send_units(&self.engine, &self.tracks, units);
                if count.is_multiple_of(SELF_VIEW_EVERY) && self.preview.wanted() {
                    match self.encoder.preview_dmabuf(frame, SELF_VIEW_WIDTH) {
                        Ok(preview) => {
                            self.self_view.send_replace(Some(Arc::new(preview)));
                        }
                        Err(e) => warn!("no self-view picture: {e}"),
                    }
                }
            }
            Err(e) => {
                let refused = refusal(&e, modifier);
                warn!(?refused, "falling back to frames in shared memory: {e}");
                self.refused.push(refused);
                self.capture.refuse_dmabuf(refused);
            }
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
        match self.encoder.encode(&self.frame) {
            Ok(units) => send_units(&self.engine, &self.tracks, units),
            Err(e) => warn!("encoding failed: {e}"),
        }
    }
}

fn send_units(engine: &MediaEngine, tracks: &Share, units: Vec<EncodedUnit>) {
    for unit in units {
        let outgoing = Outgoing::new(tracks.video, unit.data, unit.timestamp, unit.keyframe);
        engine.send(Kind::Video, outgoing);
    }
}

/// Which frames in GPU memory to refuse after one failed with `error`: those with its
/// `modifier` when only importing that layout failed, else all of them.
#[cfg(target_os = "linux")]
fn refusal(error: &EncodeError, modifier: u64) -> Option<u64> {
    match error {
        EncodeError::Import(_) => Some(modifier),
        _ => None,
    }
}

/// How long a share waits for its first frame to learn the screen's size.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(2);

/// The capture's first frame: copied out, or left in GPU memory with only its size kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum First {
    Copied,
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
/// Room above each announced side for decoders that pad pictures to their alignment.
const DECODER_PADDING: usize = 64;

/// The most pixels a decoded picture of a share announced as `config` may have.
fn max_pixels(config: &ShareConfig) -> usize {
    let side = |announced: u16| usize::from(announced) + DECODER_PADDING;
    (side(config.width) * side(config.height)).min(MAX_PIXELS)
}

fn frame_bytes(frame: &VideoFrame) -> usize {
    frame.y.len() + frame.uv.len()
}

/// Drops the oldest held frames until one of `incoming` bytes fits both bounds.
fn make_room(held: &mut VecDeque<Arc<VideoFrame>>, incoming: usize) {
    let mut bytes: usize = held.iter().map(|f| frame_bytes(f)).sum();
    while held.len() >= MAX_HELD
        || (!held.is_empty() && bytes.saturating_add(incoming) > MAX_HELD_BYTES)
    {
        if let Some(oldest) = held.pop_front() {
            bytes -= frame_bytes(&oldest);
        }
    }
}

/// Decodes one remote share and keeps its newest due picture. With `playout`, a frame is
/// due once the share audio reaching the speakers has caught up with it.
/// Decodes `units` on a thread of its own. After a unit fails to decode, `ask_idr` is called
/// once, and again only after a unit has decoded since.
/// Pictures larger than `config` announces are dropped.
pub fn receive(
    units: mpsc::Receiver<Received>,
    config: ShareConfig,
    playout: Option<Arc<Playout>>,
    ask_idr: impl Fn() + Send + 'static,
) -> Result<Picture, String> {
    let mut decoder = Decoder::new(config.codec, max_pixels(&config)).map_err(|e| e.to_string())?;
    let (tx, picture) = watch::channel(None);
    std::thread::Builder::new()
        .name("throcc-decode".into())
        .spawn(move || {
            let mut units = units;
            let mut held: VecDeque<Arc<VideoFrame>> = VecDeque::new();
            let mut asked = false;
            loop {
                let unit = if held.is_empty() {
                    units.blocking_recv()
                } else {
                    match units.try_recv() {
                        Ok(unit) => Some(unit),
                        Err(mpsc::error::TryRecvError::Empty) => {
                            std::thread::sleep(IDLE);
                            None
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => return,
                    }
                };
                if let Some(unit) = &unit {
                    match decoder.decode(&unit.data, unit.timestamp) {
                        Ok(Some(frame)) => {
                            asked = false;
                            make_room(&mut held, frame_bytes(&frame));
                            held.push_back(Arc::new(frame));
                        }
                        Ok(None) => asked = false,
                        Err(e) => {
                            warn!("decoding failed: {e}");
                            if !asked {
                                ask_idr();
                                asked = true;
                            }
                        }
                    }
                } else if held.is_empty() {
                    return;
                }
                let heard = playout.as_ref().and_then(|p| p.heard());
                let due = due_frames(held.iter().map(|f| f.timestamp), heard);
                if let Some(newest) = held.drain(..due).next_back()
                    && tx.send(Some(newest)).is_err()
                {
                    return;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(picture)
}

#[cfg(all(test, target_os = "linux"))]
mod refusal_tests {
    use super::super::video::vaapi::VaError;
    use super::*;

    #[test]
    fn a_layout_that_fails_to_import_is_refused_alone() {
        let import = EncodeError::Import(VaError::Format(0));
        assert_eq!(refusal(&import, 7), Some(7));
        let processing = EncodeError::Gpu(VaError::Call {
            call: "vaEndPicture",
            status: 1,
        });
        assert_eq!(refusal(&processing, 7), None);
        assert_eq!(refusal(&EncodeError::NoGpuInput, 7), None);
    }
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

    fn config(width: u16, height: u16) -> ShareConfig {
        ShareConfig {
            audio: false,
            kbps: 4000,
            codec: Codec::Vp9,
            width,
            height,
            fps: 30,
        }
    }

    #[test]
    fn decoded_pictures_are_bounded_by_the_announced_size() {
        assert_eq!(max_pixels(&config(1280, 720)), (1280 + 64) * (720 + 64));
        assert_eq!(max_pixels(&config(u16::MAX, u16::MAX)), MAX_PIXELS);
        assert_eq!(max_pixels(&config(0, 0)), 64 * 64);
    }

    #[test]
    fn held_frames_stay_within_count_and_bytes() {
        let mut held = VecDeque::new();
        let small = Arc::new(VideoFrame::blank(16, 16));
        for _ in 0..MAX_HELD + 5 {
            make_room(&mut held, frame_bytes(&small));
            held.push_back(small.clone());
        }
        assert_eq!(held.len(), MAX_HELD);

        let big = Arc::new(VideoFrame::blank(4096, 4096));
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
