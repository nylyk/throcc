use std::collections::BTreeMap;

use bytes::Bytes;
use throcc_proto::frame::{BODY_BYTES, Flags, TIMESTAMP_BYTES};

/// One datagram's worth of an access unit, before a `seq` is assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub flags: Flags,
    /// The media timestamp, carried only by the first fragment.
    pub timestamp: Option<[u8; TIMESTAMP_BYTES]>,
    pub body: Bytes,
}

impl Fragment {
    pub fn plaintext_parts(&self) -> [&[u8]; 2] {
        [
            self.timestamp.as_ref().map_or(&[][..], |t| &t[..]),
            &self.body,
        ]
    }
}

/// Cuts an access unit into fragments that are slices of `unit`.
pub fn fragment(unit: &Bytes, timestamp: u32, keyframe: bool, discardable: bool) -> Vec<Fragment> {
    let base = Flags::default()
        .with(Flags::KEYFRAME, keyframe)
        .with(Flags::DISCARDABLE, discardable);
    let first_len = unit.len().min(BODY_BYTES - TIMESTAMP_BYTES);
    let mut fragments = vec![Fragment {
        flags: base.with(Flags::FIRST, true),
        timestamp: Some(timestamp.to_be_bytes()),
        body: unit.slice(..first_len),
    }];
    let mut at = first_len;
    while at < unit.len() {
        let end = (at + BODY_BYTES).min(unit.len());
        fragments.push(Fragment {
            flags: base,
            timestamp: None,
            body: unit.slice(at..end),
        });
        at = end;
    }
    if let Some(last) = fragments.last_mut() {
        last.flags = last.flags.with(Flags::LAST, true);
    }
    fragments
}

pub const WINDOW_BITS: u32 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    New,
    Duplicate,
    TooOld,
}

/// A 2048-bit window behind the highest `seq` seen on one track. Drops repeats and
/// anything older than the window, and counts what never arrived.
#[derive(Clone, Debug)]
pub struct ReplayWindow {
    highest: Option<u32>,
    bits: [u64; (WINDOW_BITS / 64) as usize],
    first: u32,
    received: u64,
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self {
            highest: None,
            bits: [0; (WINDOW_BITS / 64) as usize],
            first: 0,
            received: 0,
        }
    }
}

impl ReplayWindow {
    pub fn check(&mut self, seq: u32) -> Seen {
        let Some(highest) = self.highest else {
            self.highest = Some(seq);
            self.first = seq;
            self.set(0);
            self.received = 1;
            return Seen::New;
        };
        if seq > highest {
            self.shift(seq - highest);
            self.highest = Some(seq);
            self.set(0);
        } else {
            let behind = highest - seq;
            if behind >= WINDOW_BITS {
                return Seen::TooOld;
            }
            if self.get(behind) {
                return Seen::Duplicate;
            }
            self.set(behind);
            self.first = self.first.min(seq);
        }
        self.received += 1;
        Seen::New
    }

    /// Datagrams between the first and the highest `seq` that never arrived.
    pub fn lost(&self) -> u64 {
        self.highest.map_or(0, |h| {
            (u64::from(h - self.first) + 1).saturating_sub(self.received)
        })
    }

    fn get(&self, behind: u32) -> bool {
        self.bits[(behind / 64) as usize] >> (behind % 64) & 1 == 1
    }

    fn set(&mut self, behind: u32) {
        self.bits[(behind / 64) as usize] |= 1 << (behind % 64);
    }

    /// Moves the window forward by `by` positions.
    fn shift(&mut self, by: u32) {
        if by >= WINDOW_BITS {
            self.bits = [0; (WINDOW_BITS / 64) as usize];
            return;
        }
        let (words, bits) = ((by / 64) as usize, by % 64);
        let len = self.bits.len();
        for i in (0..len).rev() {
            let lo = i.checked_sub(words).map_or(0, |j| self.bits[j]);
            let carry = i
                .checked_sub(words + 1)
                .filter(|_| bits > 0)
                .map_or(0, |j| self.bits[j] >> (64 - bits));
            self.bits[i] = if bits == 0 { lo } else { lo << bits | carry };
        }
    }
}

/// A reassembled access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub first_seq: u32,
    pub last_seq: u32,
    pub timestamp: u32,
    pub flags: Flags,
    pub data: Vec<u8>,
}

const MAX_BUFFERED: usize = 4096;

/// Rebuilds access units from decrypted fragments that may arrive out of order.
/// A unit is emitted only for an unbroken run from a first to a last fragment.
#[derive(Default)]
pub struct Reassembler {
    fragments: BTreeMap<u32, (Flags, Vec<u8>)>,
}

impl Reassembler {
    pub fn push(&mut self, seq: u32, flags: Flags, plaintext: &[u8]) -> Option<Unit> {
        if self.fragments.len() >= MAX_BUFFERED {
            self.fragments.pop_first();
        }
        self.fragments.insert(seq, (flags, plaintext.to_vec()));
        let first = self.run_start(seq)?;
        let last = self.run_end(seq)?;
        let mut data = Vec::new();
        let mut timestamp = None;
        let mut unit_flags = Flags::default();
        for s in first..=last {
            let (flags, body) = self.fragments.remove(&s)?;
            let body = if s == first {
                let (ts, rest) = body.split_first_chunk::<TIMESTAMP_BYTES>()?;
                timestamp = Some(u32::from_be_bytes(*ts));
                unit_flags = flags.defined().with(Flags::LAST, true);
                rest.to_vec()
            } else {
                body
            };
            data.extend_from_slice(&body);
        }
        self.fragments.retain(|s, _| *s > last || *s < first);
        Some(Unit {
            first_seq: first,
            last_seq: last,
            timestamp: timestamp?,
            flags: unit_flags,
            data,
        })
    }

    /// Drops buffered fragments older than `seq`, which can no longer complete in order.
    pub fn forget_before(&mut self, seq: u32) {
        self.fragments.retain(|s, _| *s >= seq);
    }

    fn run_start(&self, mut seq: u32) -> Option<u32> {
        loop {
            let (flags, _) = self.fragments.get(&seq)?;
            if flags.contains(Flags::FIRST) {
                return Some(seq);
            }
            seq = seq.checked_sub(1)?;
        }
    }

    fn run_end(&self, mut seq: u32) -> Option<u32> {
        loop {
            let (flags, _) = self.fragments.get(&seq)?;
            if flags.contains(Flags::LAST) {
                return Some(seq);
            }
            seq = seq.checked_add(1)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn unit(len: usize) -> Bytes {
        Bytes::from((0..len).map(|i| i as u8).collect::<Vec<_>>())
    }

    fn reassemble(fragments: &[(u32, Fragment)]) -> Vec<Unit> {
        let mut reassembler = Reassembler::default();
        fragments
            .iter()
            .filter_map(|(seq, f)| reassembler.push(*seq, f.flags, &f.plaintext_parts().concat()))
            .collect()
    }

    #[test]
    fn fragments_fit_the_body_and_slice_the_unit() {
        for len in [0, 1, BODY_BYTES - TIMESTAMP_BYTES, BODY_BYTES, 5000] {
            let data = unit(len);
            let fragments = fragment(&data, 42, true, false);
            assert!(fragments[0].flags.contains(Flags::FIRST));
            assert!(fragments.last().unwrap().flags.contains(Flags::LAST));
            for f in &fragments {
                assert!(f.plaintext_parts().iter().map(|p| p.len()).sum::<usize>() <= BODY_BYTES);
                assert!(f.flags.contains(Flags::KEYFRAME));
            }
            let total: usize = fragments.iter().map(|f| f.body.len()).sum();
            assert_eq!(total, len);
        }
    }

    #[test]
    fn reassembles_in_any_order_and_drops_gapped_units() {
        let data = unit(4000);
        let fragments: Vec<(u32, Fragment)> = fragment(&data, 7, false, false)
            .into_iter()
            .enumerate()
            .map(|(i, f)| (100 + i as u32, f))
            .collect();
        let mut reversed = fragments.clone();
        reversed.reverse();
        for order in [fragments.clone(), reversed] {
            let units = reassemble(&order);
            assert_eq!(units.len(), 1);
            assert_eq!(
                (units[0].timestamp, units[0].data.as_slice()),
                (7, &data[..])
            );
            assert_eq!(
                (units[0].first_seq, units[0].last_seq),
                (100, 100 + fragments.len() as u32 - 1)
            );
        }
        let mut gapped = fragments.clone();
        gapped.remove(1);
        assert!(reassemble(&gapped).is_empty());
    }

    #[test]
    fn window_drops_duplicates_and_old_seqs() {
        let mut window = ReplayWindow::default();
        assert_eq!(window.check(5000), Seen::New);
        assert_eq!(window.check(5000), Seen::Duplicate);
        assert_eq!(window.check(4999), Seen::New);
        assert_eq!(window.check(5000 - WINDOW_BITS + 1), Seen::New);
        assert_eq!(window.check(5000 - WINDOW_BITS), Seen::TooOld);
        assert_eq!(window.check(6000), Seen::New);
        assert_eq!(window.check(4999), Seen::Duplicate);
        assert_eq!(window.check(6000 - WINDOW_BITS), Seen::TooOld);
        assert_eq!(window.check(100_000), Seen::New);
        assert_eq!(window.check(6000), Seen::TooOld);
    }

    #[test]
    fn window_counts_loss() {
        let mut window = ReplayWindow::default();
        for seq in [1, 2, 4, 5, 9] {
            window.check(seq);
        }
        assert_eq!(window.lost(), 4);
        window.check(3);
        assert_eq!(window.lost(), 3);
    }

    #[test]
    fn a_seq_before_the_first_one_seen_extends_the_count() {
        let mut window = ReplayWindow::default();
        window.check(10);
        window.check(8);
        assert_eq!(window.lost(), 1);
        window.check(9);
        assert_eq!(window.lost(), 0);
    }

    proptest! {
        #[test]
        fn window_matches_a_set(seqs in proptest::collection::vec(0u32..5000, 1..400)) {
            let mut window = ReplayWindow::default();
            let mut seen = std::collections::BTreeSet::new();
            let mut highest = 0u32;
            for seq in seqs {
                let expected = if !seen.is_empty() && seq + WINDOW_BITS <= highest {
                    Seen::TooOld
                } else if seen.contains(&seq) {
                    Seen::Duplicate
                } else {
                    Seen::New
                };
                prop_assert_eq!(window.check(seq), expected, "seq {}", seq);
                if expected == Seen::New {
                    seen.insert(seq);
                    highest = highest.max(seq);
                }
            }
        }

        #[test]
        fn shuffled_and_duplicated_fragments_reassemble_once(len in 0usize..6000, seed in any::<u64>()) {
            let data = unit(len);
            let mut fragments: Vec<(u32, Fragment)> =
                fragment(&data, 9, false, false).into_iter().enumerate().map(|(i, f)| (i as u32, f)).collect();
            let copies = fragments.clone();
            fragments.extend(copies);
            let mut state = seed | 1;
            for i in (1..fragments.len()).rev() {
                state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                fragments.swap(i, (state % (i as u64 + 1)) as usize);
            }
            let mut window = ReplayWindow::default();
            let deduped: Vec<_> = fragments.into_iter().filter(|(seq, _)| window.check(*seq) == Seen::New).collect();
            let units = reassemble(&deduped);
            prop_assert_eq!(units.len(), 1);
            prop_assert_eq!(&units[0].data, &data.to_vec());
        }

        #[test]
        fn reassembler_never_panics(input in proptest::collection::vec((0u32..64, 0u8..16, proptest::collection::vec(any::<u8>(), 0..8)), 0..64)) {
            let mut reassembler = Reassembler::default();
            for (seq, flags, body) in input {
                let _ = reassembler.push(seq, Flags(flags), &body);
            }
        }
    }
}
