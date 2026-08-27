pub const WINDOW: u32 = 64;

/// A sliding window over one `media_id`'s sequence numbers. It refuses
/// duplicates and stragglers, and counts the gaps that slid past unfilled.
#[derive(Debug, Default)]
pub struct SequenceWindow {
    highest: Option<u32>,
    first: u32,
    /// Bit `i` marks `highest - i` as seen, so bit 0 is `highest` itself.
    seen: u64,
    missing: u64,
}

impl SequenceWindow {
    /// Whether this packet is new. A duplicate, or one that arrived after its
    /// place in the window slid away, is refused.
    pub fn accept(&mut self, seq: u32) -> bool {
        let Some(highest) = self.highest else {
            self.highest = Some(seq);
            self.first = seq;
            self.seen = 1;
            return true;
        };

        if seq > highest {
            self.advance(highest, seq);
            return true;
        }

        let behind = highest - seq;
        if behind >= WINDOW {
            return false;
        }
        let slot = 1u64 << behind;
        if self.seen & slot != 0 {
            return false;
        }
        self.seen |= slot;
        true
    }

    /// Sequence numbers that never arrived and can no longer be filled.
    pub fn missing(&self) -> u64 {
        self.missing
    }

    fn advance(&mut self, highest: u32, seq: u32) {
        let step = seq - highest;

        for slot in (WINDOW.saturating_sub(step)..WINDOW).rev() {
            let leaving = match highest.checked_sub(slot) {
                Some(leaving) if leaving >= self.first => leaving,
                _ => continue,
            };
            if self.seen & (1u64 << slot) == 0 {
                tracing::trace!(seq = leaving, "a gap slid out of the window unfilled");
                self.missing += 1;
            }
        }

        // Anything more than a window behind the new arrival never gets a slot.
        self.missing += u64::from(step.saturating_sub(WINDOW));

        self.seen = if step >= WINDOW { 0 } else { self.seen << step };
        self.seen |= 1;
        self.highest = Some(seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> SequenceWindow {
        SequenceWindow::default()
    }

    #[test]
    fn a_run_in_order_is_all_new() {
        let mut window = window();
        for seq in 0..1000 {
            assert!(window.accept(seq), "{seq} is new");
        }
        assert_eq!(window.missing(), 0);
    }

    #[test]
    fn a_duplicate_is_refused() {
        let mut window = window();
        assert!(window.accept(10));
        assert!(!window.accept(10));

        assert!(window.accept(12));
        assert!(window.accept(11));
        assert!(!window.accept(11));
        assert!(!window.accept(12));
        assert_eq!(window.missing(), 0, "a filled gap is not a loss");
    }

    #[test]
    fn reordering_inside_the_window_is_accepted() {
        let mut window = window();
        assert!(window.accept(100));
        assert!(window.accept(163), "the far edge of the window");
        assert!(window.accept(101), "still one slot inside the window");
        assert_eq!(window.missing(), 0);
    }

    #[test]
    fn arriving_below_the_window_is_refused() {
        let mut window = window();
        assert!(window.accept(100));
        assert!(window.accept(164));
        assert!(
            !window.accept(100),
            "exactly a window behind is already gone"
        );
        assert!(!window.accept(99));
        assert!(window.accept(101), "one slot short of the edge survives");
    }

    #[test]
    fn a_gap_that_slides_out_of_the_window_counts_as_loss() {
        let mut window = window();
        assert!(window.accept(0));
        assert!(window.accept(1));
        for seq in 3..=65 {
            assert!(window.accept(seq));
        }
        assert_eq!(window.missing(), 0, "2 is still fillable");

        assert!(window.accept(66));
        assert_eq!(window.missing(), 1, "2 has now slid out");

        assert!(!window.accept(2));
        assert_eq!(
            window.missing(),
            1,
            "a refused arrival is not a second loss"
        );
    }

    #[test]
    fn a_far_future_jump_counts_every_sequence_it_skipped() {
        let mut window = window();
        assert!(window.accept(0));
        assert!(window.accept(1_000));
        assert_eq!(
            window.missing(),
            1_000 - WINDOW as u64,
            "the 64 nearest are still in the window, the rest are gone"
        );

        for seq in 937..1_000 {
            assert!(window.accept(seq), "{seq} is inside the new window");
        }
        assert_eq!(window.missing(), 936);
    }

    #[test]
    fn a_first_packet_far_from_zero_is_not_a_thousand_losses() {
        let mut window = window();
        assert!(window.accept(5_000));
        for seq in 5_001..5_200 {
            assert!(window.accept(seq));
        }
        assert_eq!(
            window.missing(),
            0,
            "nothing before the first packet was ever expected"
        );
    }

    #[test]
    fn an_adversarial_sequence_holds_the_invariant() {
        let mut window = window();

        let arrivals = [
            (7, true),
            (9, true),
            (8, true),
            (7, false),
            (9, false),
            (10, true),
            (200, true),
            (199, true),
            (136, false),
            (135, false),
            (201, true),
            (8, false),
            (137, false),
            (4_000, true),
        ];
        for (seq, new) in arrivals {
            assert_eq!(window.accept(seq), new, "arrival {seq}");
        }

        assert!(!window.accept(4_000));
        assert!(window.accept(u32::MAX));
        assert!(!window.accept(u32::MAX));
    }

    #[test]
    fn every_sequence_is_delivered_at_most_once_under_duplication() {
        let mut window = window();
        let mut delivered = vec![0u32; 300];

        for round in 0..3 {
            for seq in 0..300u32 {
                let arrival = if round == 2 { 299 - seq } else { seq };
                if window.accept(arrival) {
                    delivered[arrival as usize] += 1;
                }
            }
        }
        assert!(
            delivered.iter().all(|&count| count == 1),
            "a sequence was delivered twice or not at all"
        );
    }
}
