use std::collections::VecDeque;

use throcc_proto::frame::VIDEO_CLOCK_HZ;

use crate::media::audio::{FRAME, SAMPLE_RATE};
use crate::media::share_audio::STEREO_FRAME;

/// An audio buffer further than this from where the previous one ended starts afresh.
const SLIP: i64 = FRAME as i64 / 2;

/// Stamps both media of a capture from one zero: video on the 90 kHz clock, each picture
/// after the last, and audio in 20 ms frames on the 48 kHz grid.
pub(super) struct ShareClock {
    zero: ShareZero,
    chunker: AudioChunker,
    /// The last picture's time in 90 kHz ticks from the zero, kept whole so a long pause
    /// can't look like time going back once timestamps wrap.
    last_frame: Option<i64>,
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
        let ticks = self.last_frame.map_or(ticks, |last| ticks.max(last + 1));
        self.last_frame = Some(ticks);
        // Timestamps wrap after about 13 hours, so this truncates on purpose.
        Some(ticks as u32)
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
        let mut clock = ShareClock::new(i64::from(VIDEO_CLOCK_HZ));
        assert_eq!(clock.video(0), Some(0));
        assert_eq!(clock.video(0), Some(1));
        assert_eq!(clock.video(10), Some(10));
        assert_eq!(clock.video(4), Some(11));
        assert_eq!(clock.video(1500), Some(1500));
        assert_eq!(clock.video(1 << 32), Some(0));
        assert_eq!(clock.video(3), Some(1));
    }

    #[test]
    fn frame_timestamps_keep_pace_after_a_pause_long_enough_to_wrap() {
        let hz = i64::from(VIDEO_CLOCK_HZ);
        let mut clock = ShareClock::new(hz);
        assert_eq!(clock.video(0), Some(0));
        let resumed = 8 * 3600 * hz;
        assert_eq!(clock.video(resumed), Some(resumed as u32));
        assert_eq!(clock.video(resumed + 1500), Some((resumed + 1500) as u32));
    }

    #[test]
    fn video_before_the_zero_has_no_timestamp() {
        let mut clock = ShareClock::new(HNS);
        clock.push_audio(1_000_000, buffer(0.0));
        assert_eq!(clock.video(1_000_000 - HNS / 100), None);
        assert_eq!(clock.video(1_000_000 + HNS / 10), Some(9000));
        assert_eq!(clock.video(1_000_000 + HNS / 20), Some(9001));
    }
}
