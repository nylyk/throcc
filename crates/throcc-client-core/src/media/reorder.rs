use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::fragment::Unit;

pub const REORDER_WINDOW: Duration = Duration::from_millis(50);
const MAX_WAITING: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    Unit(Unit),
    /// Units starting at `from` never arrived in time; the receiver should ask for recovery.
    Abandoned {
        from: u32,
    },
}

/// Emits whole units in `seq` order, waiting at most [`REORDER_WINDOW`] for a missing one.
#[derive(Default)]
pub struct Reorder {
    next: Option<u32>,
    waiting: BTreeMap<u32, Unit>,
    gap_since: Option<Instant>,
    /// Units that arrived after the ones before them were abandoned.
    pub late: u64,
}

impl Reorder {
    pub fn push(&mut self, unit: Unit, now: Instant) -> Vec<Output> {
        match self.next {
            Some(next) if unit.last_seq < next => {
                self.late += 1;
                return Vec::new();
            }
            None => self.next = Some(unit.first_seq),
            _ => {}
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_first();
        }
        self.waiting.insert(unit.first_seq, unit);
        self.drain(now)
    }

    /// Abandons a gap whose wait has run out.
    pub fn poll(&mut self, now: Instant) -> Vec<Output> {
        self.drain(now)
    }

    /// When [`Reorder::poll`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        self.gap_since.map(|since| since + REORDER_WINDOW)
    }

    fn drain(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        while let Some((&first, _)) = self.waiting.first_key_value() {
            let next = self.next.unwrap_or(first);
            if first > next {
                let since = *self.gap_since.get_or_insert(now);
                if now < since + REORDER_WINDOW {
                    break;
                }
                out.push(Output::Abandoned { from: next });
            }
            self.gap_since = None;
            if let Some((_, unit)) = self.waiting.pop_first() {
                self.next = unit.last_seq.checked_add(1);
                out.push(Output::Unit(unit));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use throcc_proto::frame::Flags;

    use super::*;

    fn unit(first: u32, last: u32) -> Unit {
        Unit {
            first_seq: first,
            last_seq: last,
            timestamp: first,
            flags: Flags::default(),
            data: vec![],
        }
    }

    fn firsts(out: &[Output]) -> Vec<Option<u32>> {
        out.iter()
            .map(|o| match o {
                Output::Unit(u) => Some(u.first_seq),
                Output::Abandoned { .. } => None,
            })
            .collect()
    }

    #[test]
    fn reorders_within_the_window() {
        let t = Instant::now();
        let mut reorder = Reorder::default();
        assert_eq!(firsts(&reorder.push(unit(0, 2), t)), vec![Some(0)]);
        assert!(reorder.push(unit(6, 6), t).is_empty());
        assert_eq!(
            firsts(&reorder.push(unit(3, 5), t + Duration::from_millis(10))),
            vec![Some(3), Some(6)]
        );
    }

    #[test]
    fn abandons_after_the_window_and_counts_late_units() {
        let t = Instant::now();
        let mut reorder = Reorder::default();
        reorder.push(unit(0, 0), t);
        assert!(reorder.push(unit(5, 5), t).is_empty());
        assert_eq!(reorder.deadline(), Some(t + REORDER_WINDOW));
        assert!(reorder.poll(t + Duration::from_millis(49)).is_empty());
        let out = reorder.poll(t + REORDER_WINDOW);
        assert_eq!(out[0], Output::Abandoned { from: 1 });
        assert_eq!(firsts(&out[1..]), vec![Some(5)]);
        assert!(reorder.push(unit(1, 4), t + REORDER_WINDOW).is_empty());
        assert_eq!(reorder.late, 1);
    }
}
