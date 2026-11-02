use std::collections::{BTreeMap, BTreeSet};

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
pub fn fragment(unit: &Bytes, timestamp: u32, keyframe: bool) -> Vec<Fragment> {
    let base = Flags::default().with(Flags::KEYFRAME, keyframe);
    let first_len = unit.len().min(BODY_BYTES - TIMESTAMP_BYTES);
    let mut fragments = vec![Fragment {
        flags: base | Flags::FIRST,
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
        last.flags = last.flags | Flags::LAST;
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
#[derive(Clone, Debug, Default)]
pub struct ReplayWindow {
    highest: Option<u32>,
    bits: [u64; (WINDOW_BITS / 64) as usize],
    first: u32,
    received: u64,
    skipped_by_restarts: u64,
}

impl ReplayWindow {
    /// Whether `seq` is new, and records it if so.
    pub fn check(&mut self, seq: u32) -> Seen {
        let seen = self.peek(seq);
        if seen == Seen::New {
            self.record(seq);
        }
        seen
    }

    /// What [`ReplayWindow::check`] would say about `seq`, without recording it.
    pub fn peek(&self, seq: u32) -> Seen {
        let Some(behind) = self.highest.and_then(|highest| highest.checked_sub(seq)) else {
            return Seen::New;
        };
        if behind >= WINDOW_BITS {
            Seen::TooOld
        } else if self.get(behind) {
            Seen::Duplicate
        } else {
            Seen::New
        }
    }

    fn record(&mut self, seq: u32) {
        match self.highest {
            Some(highest) if seq <= highest => {
                self.set(highest - seq);
                self.first = self.first.min(seq);
            }
            Some(highest) => {
                if seq - highest >= WINDOW_BITS {
                    self.skipped_by_restarts += u64::from(seq - highest - 1);
                }
                self.shift(seq - highest);
                self.highest = Some(seq);
                self.set(0);
            }
            None => {
                self.highest = Some(seq);
                self.first = seq;
                self.set(0);
            }
        }
        self.received += 1;
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    /// Datagrams between the first and the highest `seq` that never arrived, not counting
    /// forward jumps of a whole window.
    pub fn lost(&self) -> u64 {
        self.highest.map_or(0, |h| {
            (u64::from(h - self.first) + 1).saturating_sub(self.received + self.skipped_by_restarts)
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
            self.bits[i] = lo << bits | carry;
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

/// The longest video unit, in fragments, the SFU forwards.
pub const MAX_VIDEO_UNIT_FRAGMENTS: usize = 4096;
/// The longest audio unit, in fragments: an Opus packet from either audio track fits.
pub const MAX_AUDIO_UNIT_FRAGMENTS: usize = 4;
/// The most fragments buffered, and for a track whose units are short, how many units'
/// worth of them.
const MAX_BUFFERED: usize = 4096;
const BUFFERED_UNITS: usize = 64;

/// The longest unit, in bytes, that fits in `max_fragments` fragments.
pub const fn max_unit_bytes(max_fragments: usize) -> usize {
    max_fragments * BODY_BYTES - TIMESTAMP_BYTES
}

/// Rebuilds access units from decrypted fragments that may arrive out of order.
/// A unit is emitted only for an unbroken run from a first to a last fragment that is no
/// longer than the track's limit.
///
/// Each push costs O(log n) whatever the flags: a sender must not be able to make its
/// receivers rescan everything they hold for each datagram.
pub struct Reassembler {
    max_fragments: u32,
    max_buffered: usize,
    fragments: BTreeMap<u32, (Flags, Vec<u8>)>,
    firsts: BTreeSet<u32>,
    lasts: BTreeSet<u32>,
    /// Each maximal run of consecutive buffered seqs, from its first seq to its last.
    spans: BTreeMap<u32, u32>,
}

impl Reassembler {
    pub fn new(max_fragments: usize) -> Self {
        Self {
            max_fragments: u32::try_from(max_fragments.max(1)).unwrap_or(u32::MAX),
            max_buffered: MAX_BUFFERED.min(max_fragments.max(1).saturating_mul(BUFFERED_UNITS)),
            fragments: BTreeMap::new(),
            firsts: BTreeSet::new(),
            lasts: BTreeSet::new(),
            spans: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, seq: u32, flags: Flags, plaintext: &[u8]) -> Option<Unit> {
        if self.fragments.len() >= self.max_buffered
            && !self.fragments.contains_key(&seq)
            && let Some(&oldest) = self.fragments.keys().next()
        {
            self.drop_run(oldest, oldest);
        }
        self.insert(seq, flags, plaintext);
        let (lo, hi) = self.span_of(seq)?;
        let Some(&last) = self.lasts.range(seq..=hi).next() else {
            self.drop_unfinishable(lo, hi);
            return None;
        };
        let first = self.firsts.range(lo..=seq).next_back().copied();
        match first {
            Some(first) if last - first < self.max_fragments => self.take_unit(first, last),
            Some(first) => {
                self.drop_run(first, last);
                None
            }
            // A unit through `last` would have to start before `lo`, which makes it too long.
            None if last - lo >= self.max_fragments - 1 => {
                self.drop_run(lo, last);
                None
            }
            None => None,
        }
    }

    /// Drops buffered fragments older than `seq`, which can no longer complete in order.
    pub fn forget_before(&mut self, seq: u32) {
        self.fragments = self.fragments.split_off(&seq);
        self.firsts = self.firsts.split_off(&seq);
        self.lasts = self.lasts.split_off(&seq);
        let straddling = self.span_of(seq).filter(|&(lo, _)| lo < seq);
        self.spans = self.spans.split_off(&seq);
        if let Some((_, hi)) = straddling {
            self.spans.insert(seq, hi);
        }
    }

    fn insert(&mut self, seq: u32, flags: Flags, plaintext: &[u8]) {
        if self
            .fragments
            .insert(seq, (flags, plaintext.to_vec()))
            .is_none()
        {
            self.join_spans(seq);
        }
        for (set, flag) in [
            (&mut self.firsts, Flags::FIRST),
            (&mut self.lasts, Flags::LAST),
        ] {
            if flags.contains(flag) {
                set.insert(seq);
            } else {
                set.remove(&seq);
            }
        }
    }

    /// Adds the newly buffered `seq` to the spans, merging the ones it touches.
    fn join_spans(&mut self, seq: u32) {
        let lo = seq
            .checked_sub(1)
            .and_then(|before| self.span_of(before))
            .map_or(seq, |(lo, _)| lo);
        let hi = seq
            .checked_add(1)
            .and_then(|after| self.spans.remove(&after))
            .unwrap_or(seq);
        self.spans.insert(lo, hi);
    }

    /// The span that holds `seq`.
    fn span_of(&self, seq: u32) -> Option<(u32, u32)> {
        let (&lo, &hi) = self.spans.range(..=seq).next_back()?;
        (hi >= seq).then_some((lo, hi))
    }

    /// Removes `first..=last`, which lie in one span, from the spans and flag sets.
    fn unlink(&mut self, first: u32, last: u32) {
        if let Some((lo, hi)) = self.span_of(first) {
            self.spans.remove(&lo);
            if lo < first {
                self.spans.insert(lo, first - 1);
            }
            if last < hi {
                self.spans.insert(last + 1, hi);
            }
        }
        for seq in first..=last {
            self.firsts.remove(&seq);
            self.lasts.remove(&seq);
        }
    }

    /// Drops the fragments of the span `lo..=hi` that no unit can hold any more: those after
    /// its last LAST and too far before its end to reach a LAST that comes after it.
    fn drop_unfinishable(&mut self, lo: u32, hi: u32) {
        let after_last = self.lasts.range(lo..=hi).next_back().map_or(lo, |l| l + 1);
        if hi - after_last >= self.max_fragments - 1 {
            self.drop_run(after_last, hi - (self.max_fragments - 1));
        }
    }

    fn drop_run(&mut self, first: u32, last: u32) {
        self.unlink(first, last);
        for seq in first..=last {
            self.fragments.remove(&seq);
        }
    }

    fn take_unit(&mut self, first: u32, last: u32) -> Option<Unit> {
        self.unlink(first, last);
        let run: Vec<(Flags, Vec<u8>)> = (first..=last)
            .filter_map(|seq| self.fragments.remove(&seq))
            .collect();
        let ((flags, head), rest) = run.split_first()?;
        let (timestamp, head) = head.split_first_chunk::<TIMESTAMP_BYTES>()?;
        let len = head.len() + rest.iter().map(|(_, body)| body.len()).sum::<usize>();
        let mut data = Vec::with_capacity(len);
        data.extend_from_slice(head);
        for (_, body) in rest {
            data.extend_from_slice(body);
        }
        Some(Unit {
            first_seq: first,
            last_seq: last,
            timestamp: u32::from_be_bytes(*timestamp),
            flags: *flags | Flags::LAST,
            data,
        })
    }

    #[cfg(test)]
    fn buffered(&self) -> usize {
        self.fragments.len()
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
        let mut reassembler = Reassembler::new(MAX_VIDEO_UNIT_FRAGMENTS);
        fragments
            .iter()
            .filter_map(|(seq, f)| reassembler.push(*seq, f.flags, &f.plaintext_parts().concat()))
            .collect()
    }

    #[test]
    fn fragments_fit_the_body_and_slice_the_unit() {
        for len in [0, 1, BODY_BYTES - TIMESTAMP_BYTES, BODY_BYTES, 5000] {
            let data = unit(len);
            let fragments = fragment(&data, 42, true);
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
        let fragments: Vec<(u32, Fragment)> = fragment(&data, 7, false)
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
    fn a_unit_carries_its_first_fragments_flags_and_ends_last() {
        let fragments: Vec<(u32, Fragment)> = fragment(&unit(3000), 7, true)
            .into_iter()
            .enumerate()
            .map(|(i, f)| (10 + i as u32, f))
            .collect();
        let units = reassemble(&fragments);
        assert_eq!(units[0].flags, Flags::KEYFRAME | Flags::FIRST | Flags::LAST);
        let delta = reassemble(&[(0, fragment(&unit(10), 7, false).remove(0))]);
        assert_eq!(delta[0].flags, Flags::FIRST | Flags::LAST);
    }

    #[test]
    fn forget_before_drops_only_older_fragments() {
        let fragments: Vec<(u32, Fragment)> = [
            fragment(&unit(3000), 1, false),
            fragment(&unit(3000), 2, false),
        ]
        .concat()
        .into_iter()
        .enumerate()
        .map(|(i, f)| (i as u32, f))
        .collect();
        let (first, second) = fragments.split_at(3);
        let mut reassembler = Reassembler::new(MAX_VIDEO_UNIT_FRAGMENTS);
        for (seq, f) in [&first[..2], &second[..2]].concat() {
            assert_eq!(
                reassembler.push(seq, f.flags, &f.plaintext_parts().concat()),
                None
            );
        }
        reassembler.forget_before(second[0].0);
        let (seq, f) = &first[2];
        assert_eq!(
            reassembler.push(*seq, f.flags, &f.plaintext_parts().concat()),
            None
        );
        let (seq, f) = &second[2];
        let unit = reassembler.push(*seq, f.flags, &f.plaintext_parts().concat());
        assert_eq!(unit.map(|u| (u.first_seq, u.timestamp)), Some((3, 2)));
    }

    fn push_all(reassembler: &mut Reassembler, fragments: &[(u32, Fragment)]) -> Vec<Unit> {
        fragments
            .iter()
            .filter_map(|(seq, f)| reassembler.push(*seq, f.flags, &f.plaintext_parts().concat()))
            .collect()
    }

    fn numbered(fragments: Vec<Fragment>, from: u32) -> Vec<(u32, Fragment)> {
        (from..).zip(fragments).collect()
    }

    #[test]
    fn opus_packets_fit_an_audio_unit() {
        for len in [
            super::super::audio::MAX_PACKET,
            super::super::share_audio::MAX_PACKET,
        ] {
            assert!(fragment(&unit(len), 0, false).len() <= MAX_AUDIO_UNIT_FRAGMENTS);
        }
    }

    #[test]
    fn the_longest_unit_of_a_limit_fills_exactly_that_many_fragments() {
        for max in [1, MAX_AUDIO_UNIT_FRAGMENTS, 100] {
            let longest = max_unit_bytes(max);
            assert_eq!(fragment(&unit(longest), 0, false).len(), max);
            assert_eq!(fragment(&unit(longest + 1), 0, false).len(), max + 1);
        }
    }

    #[test]
    fn units_longer_than_the_limit_are_dropped() {
        let fits = numbered(fragment(&unit(3000), 1, false), 0);
        assert_eq!(fits.len(), 3);
        let mut reassembler = Reassembler::new(3);
        let mut reversed = fits.clone();
        reversed.reverse();
        assert_eq!(push_all(&mut reassembler, &reversed).len(), 1);

        let too_long = numbered(fragment(&unit(5000), 1, false), 10);
        for order in [too_long.clone(), too_long.iter().rev().cloned().collect()] {
            let mut reassembler = Reassembler::new(3);
            assert!(push_all(&mut reassembler, &order).is_empty());
            assert!(reassembler.buffered() < 3);
        }
    }

    #[test]
    fn runs_without_a_first_fragment_do_not_pile_up() {
        let last_only: Vec<(u32, Fragment)> = (0..20_000)
            .map(|seq| {
                (
                    seq,
                    Fragment {
                        flags: Flags::LAST,
                        timestamp: None,
                        body: Bytes::from_static(b"x"),
                    },
                )
            })
            .collect();
        let mut reassembler = Reassembler::new(MAX_AUDIO_UNIT_FRAGMENTS);
        assert!(push_all(&mut reassembler, &last_only).is_empty());
        assert!(reassembler.buffered() < MAX_AUDIO_UNIT_FRAGMENTS);
    }

    #[test]
    fn audio_fragments_that_cannot_complete_do_not_pile_up() {
        let first_only = |seq| {
            (
                seq,
                Fragment {
                    flags: Flags::FIRST,
                    timestamp: Some([0; TIMESTAMP_BYTES]),
                    body: Bytes::from_static(b"x"),
                },
            )
        };
        let mut reassembler = Reassembler::new(MAX_AUDIO_UNIT_FRAGMENTS);
        let unbroken: Vec<_> = (0..20_000).map(first_only).collect();
        assert!(push_all(&mut reassembler, &unbroken).is_empty());
        assert!(reassembler.buffered() < MAX_AUDIO_UNIT_FRAGMENTS);

        let mut reassembler = Reassembler::new(MAX_AUDIO_UNIT_FRAGMENTS);
        let gapped: Vec<_> = (0..20_000).map(|i| first_only(i * 2)).collect();
        assert!(push_all(&mut reassembler, &gapped).is_empty());
        assert!(reassembler.buffered() <= MAX_AUDIO_UNIT_FRAGMENTS * BUFFERED_UNITS);

        let mut then_a_unit = unbroken[..6].to_vec();
        then_a_unit.extend(numbered(fragment(&unit(1500), 1, false), 6));
        let mut reassembler = Reassembler::new(MAX_AUDIO_UNIT_FRAGMENTS);
        assert_eq!(push_all(&mut reassembler, &then_a_unit).len(), 1);
    }

    /// The scan this replaced took ~160 µs per datagram for these streams once the buffer
    /// filled; the bound is loose enough for a slow debug build.
    #[test]
    fn crafted_streams_cost_little_per_datagram() {
        let fragment = |flags| Fragment {
            flags,
            timestamp: None,
            body: Bytes::from_static(b"x"),
        };
        let streams: [Vec<(u32, Fragment)>; 3] = [
            (0..20_000)
                .map(|seq| (seq, fragment(Flags::LAST)))
                .collect(),
            (1..20_000)
                .map(|seq| {
                    (
                        seq,
                        fragment(if seq % 4000 == 0 {
                            Flags::LAST
                        } else {
                            Flags::default()
                        }),
                    )
                })
                .collect(),
            (0..20_000)
                .map(|seq| {
                    (
                        seq,
                        fragment(if seq % 2 == 0 {
                            Flags::LAST
                        } else {
                            Flags::default()
                        }),
                    )
                })
                .collect(),
        ];
        let started = std::time::Instant::now();
        for stream in &streams {
            let mut reassembler = Reassembler::new(MAX_VIDEO_UNIT_FRAGMENTS);
            push_all(&mut reassembler, stream);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
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

    #[test]
    fn a_jump_of_a_whole_window_is_not_loss() {
        let mut window = ReplayWindow::default();
        for seq in 1..=10 {
            window.check(seq);
        }
        window.check(10 + WINDOW_BITS + 100);
        assert_eq!(window.lost(), 0);
        window.check(12 + WINDOW_BITS + 100);
        assert_eq!(window.lost(), 1);
        window.check(12 + 2 * WINDOW_BITS + 100);
        assert_eq!(window.lost(), 1);
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
                prop_assert_eq!(window.peek(seq), expected, "seq {}", seq);
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
                fragment(&data, 9, false).into_iter().enumerate().map(|(i, f)| (i as u32, f)).collect();
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
        fn shuffled_units_within_the_limit_all_reassemble(lens in proptest::collection::vec(0usize..4000, 1..8), seed in any::<u64>()) {
            let units: Vec<Bytes> = lens.iter().map(|&len| unit(len)).collect();
            let fragments: Vec<Fragment> = units
                .iter()
                .enumerate()
                .flat_map(|(i, data)| fragment(data, i as u32, false))
                .collect();
            let mut fragments = numbered(fragments, 1000);
            let mut state = seed | 1;
            for i in (1..fragments.len()).rev() {
                state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                fragments.swap(i, (state % (i as u64 + 1)) as usize);
            }
            let mut reassembler = Reassembler::new(MAX_AUDIO_UNIT_FRAGMENTS);
            let mut got = push_all(&mut reassembler, &fragments);
            got.sort_by_key(|u| u.timestamp);
            let got: Vec<&[u8]> = got.iter().map(|u| u.data.as_slice()).collect();
            let want: Vec<&[u8]> = units.iter().map(|u| &u[..]).collect();
            prop_assert_eq!(got, want);
            prop_assert_eq!(reassembler.buffered(), 0);
        }

        /// Bodies shorter than the timestamp and the video limit too: no input may panic.
        #[test]
        fn emitted_units_are_whole_runs_within_the_limit(max in prop_oneof![1usize..6, Just(MAX_VIDEO_UNIT_FRAGMENTS)], input in proptest::collection::vec((0u32..64, 0u8..16, proptest::collection::vec(any::<u8>(), 0..8)), 0..128)) {
            let mut reassembler = Reassembler::new(max);
            let mut pushed = std::collections::HashMap::new();
            for (seq, flags, body) in input {
                pushed.insert(seq, (Flags(flags), body.len()));
                if let Some(unit) = reassembler.push(seq, Flags(flags), &body) {
                    prop_assert!(unit.last_seq - unit.first_seq < max as u32);
                    prop_assert!(pushed[&unit.first_seq].0.contains(Flags::FIRST));
                    prop_assert!(pushed[&unit.last_seq].0.contains(Flags::LAST));
                    let len: usize = (unit.first_seq..=unit.last_seq).map(|s| pushed[&s].1).sum();
                    prop_assert_eq!(unit.data.len() + TIMESTAMP_BYTES, len);
                }
            }
        }
    }
}
