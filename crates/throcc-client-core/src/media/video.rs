//! Pictures of shares as the UI gets them. The codecs and frames are in `throcc-video`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub use throcc_video::{Frame, Output, View, frame};

/// The newest picture of a share, shared with the renderer.
pub type Picture = tokio::sync::watch::Receiver<Option<Arc<Frame>>>;

/// How long our share's preview counts as on screen after the UI last drew it.
const PREVIEW_LINGER: Duration = Duration::from_secs(1);

/// When the UI last drew our own share's preview, so the share builds preview pictures
/// only while someone looks at them.
pub struct PreviewDemand {
    since: Instant,
    /// Milliseconds from `since` to the last draw, plus one; zero before the first.
    drawn: AtomicU64,
}

impl Default for PreviewDemand {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            drawn: AtomicU64::new(0),
        }
    }
}

impl PreviewDemand {
    pub fn drawn(&self) {
        self.drawn.store(self.millis() + 1, Ordering::Relaxed);
    }

    pub fn wanted(&self) -> bool {
        let drawn = self.drawn.load(Ordering::Relaxed);
        drawn != 0 && (self.millis() + 1).saturating_sub(drawn) < PREVIEW_LINGER.as_millis() as u64
    }

    fn millis(&self) -> u64 {
        self.since.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn the_preview_is_wanted_only_shortly_after_a_draw() {
        let demand = PreviewDemand::default();
        assert!(!demand.wanted());
        demand.drawn();
        assert!(demand.wanted());
        let stale = PreviewDemand {
            since: Instant::now() - PREVIEW_LINGER * 2,
            drawn: AtomicU64::new(1),
        };
        assert!(!stale.wanted());
    }
}
