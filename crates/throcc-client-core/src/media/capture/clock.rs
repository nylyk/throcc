use std::collections::VecDeque;

use throcc_proto::frame::{VIDEO_CLOCK_HZ, timestamp_before};

use crate::media::audio::{FRAME, SAMPLE_RATE};
use crate::media::share_audio::STEREO_FRAME;

/// An audio buffer further than this from where the previous one ended starts afresh.
const SLIP: i64 = FRAME as i64 / 2;

/// Stamps both media of a capture from one zero: video on the 90 kHz clock, each picture
/// after the last, and audio in 20 ms frames on the 48 kHz grid.
pub(super) struct ShareClock {
    zero: ShareZero,
    chunker: AudioChunker,
    last_frame: Option<u32>,
}

impl ShareClock {
    /// A clock for capture times counted at `source_hz`.
    pub(super) fn new(source_hz: i64) -> Self {
        Self {
            zero: ShareZero::new(source_hz),
            chunker: AudioChunker::default(),
            last_frame: None,
        }
    }

    /// The timestamp of a picture taken at `at`, or `None` before the zero.
    pub(super) fn video(&mut self, at: i64) -> Option<u32> {
        let ticks = self.zero.ticks(at, VIDEO_CLOCK_HZ);
        if ticks < 0 {
            return None;
        }
        // Timestamps wrap after about 13 hours, so this truncates on purpose.
        let timestamp = strictly_after(self.last_frame, ticks as u32);
        self.last_frame = Some(timestamp);
        Some(timestamp)
    }

    /// Adds interleaved stereo whose first frame was taken at `at`.
    pub(super) fn push_audio(&mut self, at: i64, samples: impl IntoIterator<Item = f32>) {
        let ticks = self.zero.ticks(at, SAMPLE_RATE);
        self.chunker.push(ticks, samples);
    }

    /// The next 20 ms of audio into `into`, with its timestamp. While more is needed, calls
    /// `pull`, which adds one captured buffer with [`ShareClock::push_audio`] or returns
    /// `None` when there is none.
    pub(super) fn next_audio(
        &mut self,
        into: &mut [f32; STEREO_FRAME],
        mut pull: impl FnMut(&mut Self) -> Option<()>,
    ) -> Option<u32> {
        loop {
            while self.chunker.needs_more() {
                pull(self)?;
            }
            if let Some(timestamp) = self.chunker.take(into) {
                return Some(timestamp);
            }
        }
    }
}

/// `timestamp`, or the tick after `last` when it isn't later.
fn strictly_after(last: Option<u32>, timestamp: u32) -> u32 {
    match last {
        Some(last) if !timestamp_before(last, timestamp) => last.wrapping_add(1),
        _ => timestamp,
    }
}

/// Lets captured frames through at no more than the share's frame rate on average,
/// judged by their 90 kHz timestamps.
#[cfg(any(windows, feature = "ffmpeg", test))]
pub(crate) struct FrameLimiter {
    interval: u32,
    next: Option<u32>,
}

#[cfg(any(windows, feature = "ffmpeg", test))]
impl FrameLimiter {
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            interval: VIDEO_CLOCK_HZ / fps.max(1),
            next: None,
        }
    }

    pub(crate) fn admits(&mut self, timestamp: u32) -> bool {
        let Some(next) = self.next else {
            self.next = Some(timestamp.wrapping_add(self.interval));
            return true;
        };
        // A frame up to a quarter interval early still counts, so a source running at
        // exactly the share's rate loses nothing to capture jitter.
        if timestamp_before(timestamp, next.wrapping_sub(self.interval / 4)) {
            return false;
        }
        let on_schedule = next.wrapping_add(self.interval);
        self.next = Some(if timestamp_before(timestamp, on_schedule) {
            on_schedule
        } else {
            timestamp.wrapping_add(self.interval)
        });
        true
    }
}

/// The instant both media of a capture count from: the first frame or buffer stamped.
struct ShareZero {
    source_hz: i64,
    zero: Option<i64>,
}

impl ShareZero {
    fn new(source_hz: i64) -> Self {
        Self {
            source_hz,
            zero: None,
        }
    }

    /// Ticks of a `rate` Hz clock from the zero to `at`, a time on the source clock.
    fn ticks(&mut self, at: i64, rate: u32) -> i64 {
        let zero = *self.zero.get_or_insert(at);
        (i128::from(at - zero) * i128::from(rate) / i128::from(self.source_hz)) as i64
    }
}

/// Cuts timestamped interleaved stereo into 20 ms frames on the 48 kHz share clock.
/// Every frame lies on one grid of 960 ticks, which receivers' jitter buffers rely on.
#[derive(Default)]
struct AudioChunker {
    pending: VecDeque<f32>,
    /// Timestamp of the first pending sample, in 48 kHz ticks from the zero, once any came.
    pending_at: Option<i64>,
}

impl AudioChunker {
    fn needs_more(&self) -> bool {
        self.pending.len() < STEREO_FRAME
    }

    /// Appends `samples`, whose first frame is at `ticks`. A buffer that slipped from where
    /// the pending audio ends restarts the chunking on the same grid, padded with silence.
    fn push(&mut self, ticks: i64, samples: impl IntoIterator<Item = f32>) {
        let Some(at) = self.pending_at else {
            self.pending_at = Some(ticks);
            self.pending.extend(samples);
            return;
        };
        let expected = at + (self.pending.len() / 2) as i64;
        if (ticks - expected).abs() <= SLIP {
            self.pending.extend(samples);
            return;
        }
        self.pending.clear();
        let grid = at + (ticks - at).div_euclid(FRAME as i64) * FRAME as i64;
        if grid >= at {
            self.pending_at = Some(grid);
            let silence = 2 * (ticks - grid) as usize;
            self.pending.extend(std::iter::repeat_n(0.0, silence));
            self.pending.extend(samples);
        } else {
            let overlap = 2 * (at - ticks) as usize;
            self.pending.extend(samples.into_iter().skip(overlap));
        }
    }

    /// Moves the oldest 20 ms into `into` and returns its timestamp. A frame that starts
    /// before the zero is discarded and gives `None`, as does too little audio.
    fn take(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        if self.needs_more() {
            return None;
        }
        let at = self.pending_at?;
        self.pending_at = Some(at + FRAME as i64);
        if at < 0 {
            self.pending.drain(..STEREO_FRAME);
            return None;
        }
        for (slot, sample) in into.iter_mut().zip(self.pending.drain(..STEREO_FRAME)) {
            *slot = sample;
        }
        Some(at as u32)
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    const HNS: i64 = 10_000_000;

    /// A 10 ms buffer whose samples count up from `first`.
    fn buffer(first: f32) -> Vec<f32> {
        (0..FRAME).map(|i| first + i as f32).collect()
    }

    fn take_all(chunker: &mut AudioChunker) -> Vec<(u32, f32)> {
        let mut out = [0.0; STEREO_FRAME];
        let mut frames = Vec::new();
        while !chunker.needs_more() {
            if let Some(at) = chunker.take(&mut out) {
                frames.push((at, out[0]));
            }
        }
        frames
    }

    #[test]
    fn both_media_count_from_the_first_time_stamped() {
        let mut zero = ShareZero::new(HNS);
        let start = 123_456_789_000;
        assert_eq!(zero.ticks(start, VIDEO_CLOCK_HZ), 0);
        assert_eq!(zero.ticks(start + HNS / 50, SAMPLE_RATE), 960);
        assert_eq!(zero.ticks(start + HNS / 10, VIDEO_CLOCK_HZ), 9000);
        assert_eq!(zero.ticks(start - HNS / 100, SAMPLE_RATE), -480);
    }

    #[test]
    fn contiguous_buffers_make_frames_960_ticks_apart() {
        let mut chunker = AudioChunker::default();
        for n in 0..10 {
            chunker.push(480 * n, buffer(n as f32 * 1000.0));
        }
        let frames = take_all(&mut chunker);
        let stamps: Vec<u32> = frames.iter().map(|f| f.0).collect();
        assert_eq!(stamps, [0, 960, 1920, 2880, 3840]);
        assert_eq!(frames[1].1, 2000.0);
    }

    #[test]
    fn small_jitter_keeps_the_chunking() {
        let mut chunker = AudioChunker::default();
        chunker.push(0, buffer(0.0));
        chunker.push(480 + 200, buffer(1.0));
        chunker.push(960 - 150, buffer(2.0));
        chunker.push(1440, buffer(3.0));
        let stamps: Vec<u32> = take_all(&mut chunker).iter().map(|f| f.0).collect();
        assert_eq!(stamps, [0, 960]);
    }

    #[test]
    fn a_gap_restarts_the_chunking_on_the_same_grid() {
        let mut chunker = AudioChunker::default();
        chunker.push(0, buffer(0.0));
        chunker.push(10_000, buffer(1.0));
        chunker.push(10_480, buffer(2.0));
        let mut out = [0.0; STEREO_FRAME];
        assert_eq!(chunker.take(&mut out), Some(9600));
        assert_eq!(out[2 * 400 - 1], 0.0);
        assert_eq!(out[2 * 400], 1.0);
    }

    #[test]
    fn a_buffer_that_slipped_back_skips_what_was_already_sent() {
        let mut chunker = AudioChunker::default();
        chunker.push(0, buffer(0.0));
        chunker.push(480, buffer(1.0));
        let mut out = [0.0; STEREO_FRAME];
        assert_eq!(chunker.take(&mut out), Some(0));
        chunker.push(400, (0..2 * FRAME).map(|i| (i / 2) as f32));
        chunker.push(1360, buffer(2.0));
        chunker.push(1840, buffer(3.0));
        assert_eq!(chunker.take(&mut out), Some(960));
        assert_eq!(out[0], 560.0);
    }

    #[test]
    fn frames_stay_on_the_grid_when_each_buffer_fills_one_exactly() {
        let mut chunker = AudioChunker::default();
        let mut out = [0.0; STEREO_FRAME];
        for n in 0..5i64 {
            chunker.push(960 * n + n * 40, (0..STEREO_FRAME).map(|i| i as f32));
            assert_eq!(chunker.take(&mut out), Some(960 * n as u32));
        }
    }

    #[test]
    fn frames_before_the_zero_are_discarded() {
        let mut chunker = AudioChunker::default();
        for n in 0..6 {
            chunker.push(-1000 + 480 * n, buffer(n as f32));
        }
        let frames = take_all(&mut chunker);
        assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), [920]);
    }

    #[test]
    fn frame_timestamps_only_move_forward() {
        assert_eq!(strictly_after(None, 0), 0);
        assert_eq!(strictly_after(Some(0), 0), 1);
        assert_eq!(strictly_after(Some(10), 4), 11);
        assert_eq!(strictly_after(Some(10), 1500), 1500);
        assert_eq!(strictly_after(Some(u32::MAX), 3), 3);
    }

    #[test]
    fn video_before_the_zero_has_no_timestamp() {
        let mut clock = ShareClock::new(HNS);
        clock.push_audio(1_000_000, buffer(0.0));
        assert_eq!(clock.video(1_000_000 - HNS / 100), None);
        assert_eq!(clock.video(1_000_000 + HNS / 10), Some(9000));
        assert_eq!(clock.video(1_000_000 + HNS / 20), Some(9001));
    }

    /// How many of `count` frames at `source_fps` from `start` a 60 fps limiter admits.
    fn admitted(limiter: &mut FrameLimiter, start: u32, source_fps: u32, count: u32) -> u32 {
        (0..count)
            .filter(|n| {
                let offset = u64::from(*n) * u64::from(VIDEO_CLOCK_HZ) / u64::from(source_fps);
                limiter.admits(start.wrapping_add(offset as u32))
            })
            .count() as u32
    }

    #[test]
    fn a_faster_source_is_cut_to_the_share_rate() {
        for source_fps in [75, 90, 120, 144, 165, 180, 240] {
            let mut limiter = FrameLimiter::new(60);
            let kept = admitted(&mut limiter, 0, source_fps, source_fps * 10);
            assert!(
                (599..=601).contains(&kept),
                "{source_fps} fps kept {kept} in 10 s"
            );
        }
    }

    #[test]
    fn a_source_at_or_below_the_share_rate_loses_nothing() {
        for source_fps in [24, 30, 59, 60] {
            let mut limiter = FrameLimiter::new(60);
            let kept = admitted(&mut limiter, 0, source_fps, source_fps * 10);
            assert_eq!(kept, source_fps * 10, "{source_fps} fps");
        }
        let mut limiter = FrameLimiter::new(60);
        let jitter = [0, 300, -300, 200, -200, 0];
        let kept = (0..600i64)
            .filter(|n| {
                let at = n * 1500 + jitter[*n as usize % jitter.len()] + 1000;
                limiter.admits(at as u32)
            })
            .count();
        assert_eq!(kept, 600);
    }

    #[test]
    fn a_pause_does_not_let_a_burst_through_afterwards() {
        let mut limiter = FrameLimiter::new(60);
        admitted(&mut limiter, 0, 60, 60);
        let kept = admitted(&mut limiter, 5 * VIDEO_CLOCK_HZ, 240, 240);
        assert!(
            (60..=61).contains(&kept),
            "kept {kept} in the second after a pause"
        );
    }

    #[test]
    fn the_limit_holds_across_timestamp_wrap() {
        let mut limiter = FrameLimiter::new(60);
        let kept = admitted(&mut limiter, u32::MAX - VIDEO_CLOCK_HZ, 180, 360);
        assert!((119..=121).contains(&kept), "kept {kept} in 2 s");
    }
}
