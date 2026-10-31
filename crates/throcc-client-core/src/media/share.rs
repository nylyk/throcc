use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_proto::messages::{Codec, Recovery, Share, ShareConfig};
use tokio::sync::{mpsc, watch};
use tracing::warn;

use super::capture::{Capture, CaptureConfig, CaptureError};
use super::engine::{Kind, MediaEngine, Outgoing, Received};
use super::share_audio::{Playout, STEREO_FRAME, ShareAudioEncoder, due_frames};
use super::video::Picture;
use super::video::decode::Decoder;
use super::video::encode::{Encoder, EncoderConfig};
use super::video::frame::{VideoFrame, share_size};

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
        let mut first = first_frame(capture.as_mut(), &mut frame);
        let (width, height) = if first {
            share_size((frame.width, frame.height), bound)
        } else {
            bound
        };
        let mut encoder = Encoder::open(EncoderConfig {
            width,
            height,
            fps: capture_config.fps,
            kbps: wanted.kbps,
        })
        .map_err(|e| ShareError::Encoder(e.to_string()))?;
        let mut audio_encoder = if wanted.audio && capture.captures_audio() {
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
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("throcc-share".into())
            .spawn(move || {
                let mut sound = [0.0f32; STEREO_FRAME];
                let mut count = 0u32;
                let mut pacer = RecoveryPacer::default();
                while !thread_stop.load(Ordering::Relaxed) {
                    while let Ok(kind) = recover_rx.try_recv() {
                        pacer.ask(kind);
                    }
                    if let Some(kind) = pacer.due(Instant::now()) {
                        encoder.recover(kind);
                    }
                    if let Some(audio_encoder) = &mut audio_encoder {
                        while let Some(timestamp) = capture.next_audio(&mut sound) {
                            let Some(packet) = audio_encoder.encode(&sound) else {
                                continue;
                            };
                            let unit = Outgoing {
                                media_id: tracks.audio,
                                data: Bytes::copy_from_slice(packet),
                                timestamp,
                                keyframe: false,
                                discardable: false,
                                enqueued: Instant::now(),
                            };
                            engine.send(Kind::Audio, unit);
                        }
                    }
                    let fresh = std::mem::take(&mut first) || capture.next_frame(&mut frame);
                    if !fresh {
                        std::thread::sleep(IDLE);
                        continue;
                    }
                    count = count.wrapping_add(1);
                    if count.is_multiple_of(SELF_VIEW_EVERY) {
                        let factor = frame.width / SELF_VIEW_WIDTH;
                        self_tx.send_replace(Some(Arc::new(frame.shrunk(factor))));
                    }
                    match encoder.encode(&frame) {
                        Ok(units) => {
                            for unit in units {
                                engine.send(
                                    Kind::Video,
                                    Outgoing {
                                        media_id: tracks.video,
                                        data: unit.data,
                                        timestamp: unit.timestamp,
                                        keyframe: unit.keyframe,
                                        discardable: false,
                                        enqueued: Instant::now(),
                                    },
                                );
                            }
                        }
                        Err(e) => warn!("encoding failed: {e}"),
                    }
                }
            })
            .map_err(|e| ShareError::Encoder(e.to_string()))?;
        Ok(Self {
            stop,
            recover,
            thread: Some(thread),
            config,
            encoder: name,
            self_view,
        })
    }

    pub fn recover(&self, kind: Recovery) {
        self.recover.send(kind).ok();
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

/// How long a share waits for its first frame to learn the screen's size.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(2);

/// Waits for the capture's first frame, which tells the size of what is shared.
fn first_frame(capture: &mut dyn Capture, into: &mut VideoFrame) -> bool {
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    while Instant::now() < deadline {
        if capture.next_frame(into) {
            return true;
        }
        std::thread::sleep(IDLE);
    }
    false
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

/// Decodes one remote share and keeps its newest due picture. With `playout`, a frame is
/// due once the share audio reaching the speakers has caught up with it.
pub fn receive(
    units: mpsc::Receiver<Received>,
    codec: Codec,
    playout: Option<Arc<Playout>>,
) -> Result<Picture, String> {
    let mut decoder = Decoder::new(codec).map_err(|e| e.to_string())?;
    let (tx, picture) = watch::channel(None);
    std::thread::Builder::new()
        .name("throcc-decode".into())
        .spawn(move || {
            let mut units = units;
            let mut held: VecDeque<Arc<VideoFrame>> = VecDeque::new();
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
                            if held.len() == MAX_HELD {
                                held.pop_front();
                            }
                            held.push_back(Arc::new(frame));
                        }
                        Ok(None) => {}
                        Err(e) => warn!("decoding failed: {e}"),
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
