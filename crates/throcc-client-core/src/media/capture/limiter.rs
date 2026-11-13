use throcc_proto::frame::VIDEO_CLOCK_HZ;

/// Lets captured frames through at no more than the share's frame rate on average,
/// judged by their 90 kHz timestamps.
pub(crate) struct FrameLimiter {
    interval: u32,
    next: Option<u32>,
}

impl FrameLimiter {
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            interval: VIDEO_CLOCK_HZ / fps.max(1),
            next: None,
        }
    }

    /// Whether to keep a frame stamped `timestamp`. Timestamps must never go back.
    pub(crate) fn admits(&mut self, timestamp: u32) -> bool {
        let afresh = timestamp.wrapping_add(self.interval);
        let Some(next) = self.next else {
            self.next = Some(afresh);
            return true;
        };
        let interval = i64::from(self.interval);
        // How far the frame is ahead of schedule, negative when it is late. Read as wrapping
        // timestamps, so a frame after a pause of 6.6 to 13.3 hours looks far ahead.
        let early = i64::from(next.wrapping_sub(timestamp) as i32);
        self.next = Some(match early {
            // Admitted frames leave the schedule at most 1¼ intervals ahead of the next
            // one, so only a pause long enough to wrap gets this far.
            early if early > 2 * interval => afresh,
            // A frame up to a quarter interval early still counts, so a source running at
            // exactly the share's rate loses nothing to capture jitter.
            early if early > interval / 4 => return false,
            early if early > -interval => next.wrapping_add(self.interval),
            _ => afresh,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn frames_after_a_pause_long_enough_to_wrap_are_admitted() {
        for hours in [7, 8, 13] {
            let mut limiter = FrameLimiter::new(60);
            admitted(&mut limiter, 0, 60, 60);
            let resumed = (hours * 3600 * u64::from(VIDEO_CLOCK_HZ)) as u32;
            let kept = admitted(&mut limiter, resumed, 60, 60);
            assert_eq!(kept, 60, "after {hours} h");
        }
    }

    #[test]
    fn the_limit_holds_across_timestamp_wrap() {
        let mut limiter = FrameLimiter::new(60);
        let kept = admitted(&mut limiter, u32::MAX - VIDEO_CLOCK_HZ, 180, 360);
        assert!((119..=121).contains(&kept), "kept {kept} in 2 s");
    }
}
