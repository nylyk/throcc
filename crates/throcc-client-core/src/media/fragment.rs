use std::collections::BTreeMap;

use throcc_proto::PAYLOAD_BUDGET;

pub const FIRST: u8 = 1 << 0;
pub const LAST: u8 = 1 << 1;
pub const TIMESTAMP: u8 = 1 << 2;
const KNOWN_MARKS: u8 = FIRST | LAST | TIMESTAMP;

const MARKS_BYTES: usize = 1;
const TIMESTAMP_BYTES: usize = 4;

/// Codec bytes per datagram, once the fragment byte is taken out of the payload
/// budget. A first fragment carrying a timestamp gives up four more.
pub const CODEC_BUDGET: usize = PAYLOAD_BUDGET - MARKS_BYTES;

/// Holding this many fragments for one track means a peer stopped marking its
/// last fragments, not that the network is slow.
const MAX_PENDING_FRAGMENTS: usize = 512;

/// One access unit, split into datagram payloads. The timestamp rides on the
/// first fragment, and only on tracks whose sound has to match their picture.
pub fn fragment(unit: &[u8], timestamp: Option<u32>) -> Vec<Vec<u8>> {
    let first_budget = CODEC_BUDGET - timestamp.map_or(0, |_| TIMESTAMP_BYTES);
    let (head, rest) = unit.split_at(unit.len().min(first_budget));

    let mut payloads = Vec::with_capacity(1 + rest.len() / CODEC_BUDGET);
    payloads.push(payload(FIRST, timestamp, head));
    for chunk in rest.chunks(CODEC_BUDGET) {
        payloads.push(payload(0, None, chunk));
    }

    let marks = &mut payloads
        .last_mut()
        .expect("the first fragment is always pushed")[0];
    *marks |= LAST;
    payloads
}

fn payload(marks: u8, timestamp: Option<u32>, bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(MARKS_BYTES + TIMESTAMP_BYTES + bytes.len());
    match timestamp {
        None => payload.push(marks),
        Some(ticks) => {
            payload.push(marks | TIMESTAMP);
            payload.extend_from_slice(&ticks.to_be_bytes());
        }
    }
    payload.extend_from_slice(bytes);
    payload
}

pub struct AccessUnit {
    pub bytes: Vec<u8>,
    pub timestamp: Option<u32>,
    pub last_seq: u32,
}

struct Fragment {
    marks: u8,
    timestamp: Option<u32>,
    bytes: Vec<u8>,
}

/// One track's fragments, assembled into access units and emitted in `seq`
/// order, since a decoder fed out of order produces garbage.
#[derive(Default)]
pub struct Reassembler {
    pending: BTreeMap<u32, Fragment>,
    emitted_through: Option<u32>,
    abandoned: u64,
    late: u64,
}

impl Reassembler {
    /// Whatever became emittable, in `seq` order. Usually nothing or one unit.
    pub fn push(&mut self, seq: u32, payload: &[u8]) -> Vec<AccessUnit> {
        let Some(fragment) = parse(payload) else {
            tracing::debug!(seq, "dropping a malformed fragment");
            return Vec::new();
        };
        if self.emitted_through.is_some_and(|through| seq <= through) {
            self.late += 1;
            tracing::trace!(seq, "dropping a fragment of a unit already resolved");
            return Vec::new();
        }

        self.pending.insert(seq, fragment);
        while self.pending.len() > MAX_PENDING_FRAGMENTS {
            self.abandon_lowest();
        }
        self.drain_complete()
    }

    /// Units given up on because a peer buried them under later fragments.
    pub fn abandoned(&self) -> u64 {
        self.abandoned
    }

    /// Fragments that arrived after their unit was resolved. Nonzero says the
    /// receiver gave up too early, rather than that the network lost anything.
    pub fn late_fragments(&self) -> u64 {
        self.late
    }

    fn drain_complete(&mut self) -> Vec<AccessUnit> {
        let mut units = Vec::new();
        while let Some(unit) = self.take_lowest_complete() {
            units.push(unit);
        }
        units
    }

    fn take_lowest_complete(&mut self) -> Option<AccessUnit> {
        let (&start, head) = self.pending.iter().next()?;
        if head.marks & FIRST == 0 {
            return None;
        }

        let mut end = start;
        while self.pending.get(&end)?.marks & LAST == 0 {
            end = end.checked_add(1)?;
            if !self.pending.contains_key(&end) {
                return None;
            }
        }

        let mut bytes = Vec::new();
        let mut timestamp = None;
        for seq in start..=end {
            let fragment = self
                .pending
                .remove(&seq)
                .expect("the whole run was just walked");
            timestamp = timestamp.or(fragment.timestamp);
            bytes.extend_from_slice(&fragment.bytes);
        }
        self.emitted_through = Some(end);

        Some(AccessUnit {
            bytes,
            timestamp,
            last_seq: end,
        })
    }

    fn abandon_lowest(&mut self) {
        let Some((&start, _)) = self.pending.iter().next() else {
            return;
        };

        let mut end = start;
        for (&seq, fragment) in self.pending.range(start..) {
            if seq != start && fragment.marks & FIRST != 0 {
                break;
            }
            end = seq;
            if fragment.marks & LAST != 0 {
                break;
            }
        }

        self.pending.retain(|&seq, _| seq > end);
        self.emitted_through = Some(end);
        self.abandoned += 1;
        tracing::debug!(start, end, "abandoning a unit that never completed");
    }
}

fn parse(payload: &[u8]) -> Option<Fragment> {
    let (&marks, rest) = payload.split_first()?;
    if marks & !KNOWN_MARKS != 0 {
        return None;
    }

    if marks & TIMESTAMP == 0 {
        return Some(Fragment {
            marks,
            timestamp: None,
            bytes: rest.to_vec(),
        });
    }
    let (ticks, bytes) = rest.split_at_checked(TIMESTAMP_BYTES)?;
    Some(Fragment {
        marks,
        timestamp: Some(u32::from_be_bytes(ticks.try_into().ok()?)),
        bytes: bytes.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(len: usize) -> Vec<u8> {
        (0..len).map(|byte| (byte % 251) as u8).collect()
    }

    fn reassemble(fragments: &[(u32, Vec<u8>)]) -> Vec<AccessUnit> {
        let mut reassembler = Reassembler::default();
        let mut units = Vec::new();
        for (seq, payload) in fragments {
            units.extend(reassembler.push(*seq, payload));
        }
        units
    }

    fn numbered(payloads: Vec<Vec<u8>>, from: u32) -> Vec<(u32, Vec<u8>)> {
        payloads
            .into_iter()
            .enumerate()
            .map(|(offset, payload)| (from + offset as u32, payload))
            .collect()
    }

    #[test]
    fn no_fragment_exceeds_the_datagram_budget() {
        for payload in fragment(&unit(400_000), Some(90_000)) {
            assert!(
                payload.len() <= PAYLOAD_BUDGET,
                "a fragment of {} bytes does not fit",
                payload.len()
            );
        }
    }

    #[test]
    fn one_datagram_carries_a_whole_audio_frame() {
        let payloads = fragment(&unit(80), None);
        assert_eq!(payloads.len(), 1, "audio never fragments");
        assert_eq!(payloads[0][0], FIRST | LAST, "both marks are set");

        let units = reassemble(&numbered(payloads, 0));
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].bytes, unit(80));
        assert_eq!(units[0].timestamp, None);
    }

    #[test]
    fn an_empty_unit_is_still_one_fragment() {
        let payloads = fragment(&[], None);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0], vec![FIRST | LAST]);
        assert!(reassemble(&numbered(payloads, 0))[0].bytes.is_empty());
    }

    #[test]
    fn a_large_unit_round_trips_with_its_fragments_reordered() {
        let original = unit(400_000);
        let mut fragments = numbered(fragment(&original, Some(1_234)), 100);
        assert!(fragments.len() > 300, "the unit really is many datagrams");

        let mut draw: u64 = 0x9E37_79B9_7F4A_7C15;
        for index in (1..fragments.len()).rev() {
            draw = draw.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            fragments.swap(index, (draw >> 33) as usize % (index + 1));
        }

        let units = reassemble(&fragments);
        assert_eq!(units.len(), 1, "one unit, however its fragments arrived");
        assert_eq!(units[0].bytes, original);
        assert_eq!(units[0].timestamp, Some(1_234));
    }

    #[test]
    fn a_duplicate_fragment_changes_nothing() {
        let mut fragments = numbered(fragment(&unit(3_000), None), 0);
        fragments.push(fragments[1].clone());
        fragments.push(fragments[0].clone());

        let units = reassemble(&fragments);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].bytes, unit(3_000));
    }

    #[test]
    fn a_missing_middle_fragment_discards_the_unit() {
        let mut fragments = numbered(fragment(&unit(3_000), None), 0);
        fragments.remove(1);
        assert!(reassemble(&fragments).is_empty());
    }

    #[test]
    fn a_missing_last_fragment_discards_the_unit() {
        let mut fragments = numbered(fragment(&unit(3_000), None), 0);
        fragments.pop();
        assert!(reassemble(&fragments).is_empty());
    }

    #[test]
    fn two_units_interleaved_come_out_in_sequence_order() {
        let first = numbered(fragment(&unit(2_500), None), 0);
        let second = numbered(fragment(&unit(2_400), Some(90)), first.len() as u32);

        let mut arrivals = Vec::new();
        for (early, late) in first.iter().zip(second.iter()) {
            arrivals.push(late.clone());
            arrivals.push(early.clone());
        }

        let units = reassemble(&arrivals);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].bytes, unit(2_500));
        assert_eq!(units[1].bytes, unit(2_400));
        assert!(
            units[0].last_seq < units[1].last_seq,
            "a later unit must never be emitted first"
        );
    }

    #[test]
    fn a_completed_unit_waits_for_the_unfinished_one_before_it() {
        let first = numbered(fragment(&unit(2_500), None), 0);
        let second = numbered(fragment(&unit(1_000), None), first.len() as u32);
        let mut reassembler = Reassembler::default();

        let (opening, rest) = first.split_first().expect("the unit has fragments");
        assert!(reassembler.push(opening.0, &opening.1).is_empty());
        for (seq, payload) in &second {
            assert!(
                reassembler.push(*seq, payload).is_empty(),
                "the later unit is complete but the earlier one is not"
            );
        }

        let mut emitted = Vec::new();
        for (seq, payload) in rest {
            emitted.extend(reassembler.push(*seq, payload));
        }
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].bytes, unit(2_500));
        assert_eq!(emitted[1].bytes, unit(1_000));
    }

    #[test]
    fn a_unit_lost_outright_does_not_stall_the_next_one() {
        let lost = fragment(&unit(2_500), None);
        let arriving = numbered(fragment(&unit(900), None), lost.len() as u32);

        let units = reassemble(&arriving);
        assert_eq!(units.len(), 1, "nothing earlier is being waited for");
        assert_eq!(units[0].bytes, unit(900));
    }

    #[test]
    fn a_fragment_of_a_resolved_unit_is_counted_and_dropped() {
        let fragments = numbered(fragment(&unit(2_500), None), 0);
        let mut reassembler = Reassembler::default();
        for (seq, payload) in &fragments {
            reassembler.push(*seq, payload);
        }
        assert_eq!(reassembler.late_fragments(), 0);

        assert!(reassembler.push(0, &fragments[0].1).is_empty());
        assert_eq!(reassembler.late_fragments(), 1);
    }

    #[test]
    fn a_peer_that_never_marks_a_last_fragment_is_given_up_on() {
        let mut reassembler = Reassembler::default();
        for seq in 0..=MAX_PENDING_FRAGMENTS as u32 {
            let marks = if seq == 0 { FIRST } else { 0 };
            assert!(reassembler.push(seq, &[marks]).is_empty());
        }
        assert_eq!(reassembler.abandoned(), 1);

        let recovered = numbered(
            fragment(&unit(1_000), None),
            MAX_PENDING_FRAGMENTS as u32 + 1,
        );
        let mut emitted = Vec::new();
        for (seq, payload) in &recovered {
            emitted.extend(reassembler.push(*seq, payload));
        }
        assert_eq!(emitted.len(), 1, "the track recovers after the abandonment");
        assert_eq!(emitted[0].bytes, unit(1_000));
    }

    #[test]
    fn a_fragment_with_an_unknown_mark_is_refused() {
        let mut reassembler = Reassembler::default();
        assert!(reassembler.push(0, &[FIRST | LAST | 1 << 5]).is_empty());
        assert!(reassembler.push(1, &[]).is_empty());
        assert!(
            reassembler
                .push(2, &[FIRST | LAST | TIMESTAMP, 0, 0])
                .is_empty(),
            "a timestamp mark with too few bytes behind it"
        );
    }
}
