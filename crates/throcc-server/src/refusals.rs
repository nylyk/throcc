use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use tracing::warn;

use crate::lock;

/// How often the counts are logged.
pub const REFUSAL_REPORT: Duration = Duration::from_secs(10);

/// Counts connections refused or failed before sign-in, by reason, and logs the counts
/// every [`REFUSAL_REPORT`]. One source can refuse itself as fast as it can send packets,
/// so these are never logged one line each; the per-connection detail goes to `debug`.
#[derive(Default)]
pub struct RefusalLog(Mutex<BTreeMap<&'static str, u64>>);

impl RefusalLog {
    pub fn count(&self, reason: &'static str) {
        *lock(&self.0).entry(reason).or_default() += 1;
    }

    /// Logs the counts since the last report and starts again from zero.
    pub fn report(&self) {
        let counts = std::mem::take(&mut *lock(&self.0));
        let seconds = REFUSAL_REPORT.as_secs();
        for (reason, count) in counts {
            warn!(
                count,
                "{count} connections in the last {seconds} s: {reason}"
            );
        }
    }

    #[cfg(test)]
    fn counts(&self) -> BTreeMap<&'static str, u64> {
        lock(&self.0).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_reason_until_reported() {
        let log = RefusalLog::default();
        for _ in 0..3 {
            log.count("a");
        }
        log.count("b");
        assert_eq!(log.counts(), BTreeMap::from([("a", 3), ("b", 1)]));
        log.report();
        assert!(log.counts().is_empty());
    }
}
