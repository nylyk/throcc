use std::collections::VecDeque;

use crate::media::jitter::FRAME;
use crate::media::share_audio::STEREO_FRAME;

/// An audio buffer further than this from where the previous one ended starts afresh.
const SLIP: i64 = FRAME as i64 / 2;

/// The instant both media of a capture count from: the first frame or buffer stamped.
pub(super) struct ShareZero {
    source_hz: i64,
    zero: Option<i64>,
}

impl ShareZero {
    pub(super) fn new(source_hz: i64) -> Self {
        Self {
            source_hz,
            zero: None,
        }
    }

    /// Ticks of a `rate` Hz clock from the zero to `at`, a time on the source clock.
    pub(super) fn ticks(&mut self, at: i64, rate: u32) -> i64 {
        let zero = *self.zero.get_or_insert(at);
        (i128::from(at - zero) * i128::from(rate) / i128::from(self.source_hz)) as i64
    }
}

/// Cuts timestamped interleaved stereo into 20 ms frames on the 48 kHz share clock.
/// Every frame lies on one grid of 960 ticks, which receivers' jitter buffers rely on.
#[derive(Default)]
pub(super) struct AudioChunker {
    pending: VecDeque<f32>,
    /// Timestamp of the first pending sample, in 48 kHz ticks from the zero, once any came.
    pending_at: Option<i64>,
}

impl AudioChunker {
    pub(super) fn needs_more(&self) -> bool {
        self.pending.len() < STEREO_FRAME
    }

    /// Appends `samples`, whose first frame is at `ticks`. A buffer that slipped from where
    /// the pending audio ends restarts the chunking on the same grid, padded with silence.
    pub(super) fn push(&mut self, ticks: i64, samples: impl IntoIterator<Item = f32>) {
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
    pub(super) fn take(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
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
    const VIDEO_HZ: u32 = 90_000;
    const AUDIO_HZ: u32 = 48_000;

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
        assert_eq!(zero.ticks(start, VIDEO_HZ), 0);
        assert_eq!(zero.ticks(start + HNS / 50, AUDIO_HZ), 960);
        assert_eq!(zero.ticks(start + HNS / 10, VIDEO_HZ), 9000);
        assert_eq!(zero.ticks(start - HNS / 100, AUDIO_HZ), -480);
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
}
