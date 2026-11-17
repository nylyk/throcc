use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// The frame format, first in every frame.
const FORMAT: u8 = 1;
/// The bytes of the blob one frame carries.
const BLOCK_BYTES: usize = 480;
/// Format, blob id, blob length and sequence number.
const HEADER_BYTES: usize = 1 + 4 + 4 + 4;
/// The largest blob a collector takes, which bounds what a frame can make it hold.
const MAX_BLOB_BYTES: usize = 1 << 20;
/// How many frames past the block count a transfer is expected to need, for the estimate.
const EXPECTED_OVERHEAD: f64 = 1.15;
/// The QR code alphanumeric mode's characters, which base 45 writes in.
const BASE45: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// A blob cut into an endless run of frames, as text a QR code holds in alphanumeric mode.
/// Frames below the block count carry one block each, in order; every later one carries the
/// XOR of a few blocks that its sequence number picks, so a receiver that missed some frames
/// fills them in from any later ones.
pub struct Fountain {
    blob: Vec<u8>,
    id: [u8; 4],
}

impl Fountain {
    pub fn new(blob: Vec<u8>) -> Self {
        let id = blob_id(&blob);
        Self { blob, id }
    }

    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    pub fn blocks(&self) -> u32 {
        block_count(self.blob.len())
    }

    /// Frame `seq` as text.
    pub fn frame(&self, seq: u32) -> String {
        let blocks = self.blocks();
        let mut body = vec![0; BLOCK_BYTES];
        for index in mixed_blocks(&self.id, seq, blocks) {
            let start = index * BLOCK_BYTES;
            let end = (start + BLOCK_BYTES).min(self.blob.len());
            for (out, byte) in body.iter_mut().zip(&self.blob[start..end]) {
                *out ^= byte;
            }
        }
        let mut frame = Vec::with_capacity(HEADER_BYTES + BLOCK_BYTES);
        frame.push(FORMAT);
        frame.extend_from_slice(&self.id);
        frame.extend_from_slice(&(self.blob.len() as u32).to_be_bytes());
        frame.extend_from_slice(&seq.to_be_bytes());
        frame.extend_from_slice(&body);
        base45(&frame)
    }
}

/// How far a [`Collector`] has come.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    /// The share of the blob known, from 0 to 1.
    pub fraction: f32,
    /// How long the rest should take at the rate frames arrived so far, once there is a rate.
    pub remaining: Option<Duration>,
}

/// Collects a [`Fountain`]'s frames, in any order and with repeats, until the blob is whole.
#[derive(Default)]
pub struct Collector {
    transfer: Option<Transfer>,
}

struct Transfer {
    id: [u8; 4],
    len: usize,
    blocks: Vec<Option<Vec<u8>>>,
    /// Frames that mix blocks not all known yet, as the unknown blocks and their XOR.
    pending: Vec<(BTreeSet<usize>, Vec<u8>)>,
    seen: HashSet<u32>,
    started: Instant,
    blob: Option<Vec<u8>>,
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes in a scanned frame. False for text that is no frame or one already taken. A
    /// frame of another blob starts over with that one.
    pub fn add(&mut self, text: &str) -> bool {
        let Some(frame) = unbase45(text) else {
            return false;
        };
        let Some((id, len, seq, body)) = parse_frame(&frame) else {
            return false;
        };
        let transfer = match &mut self.transfer {
            Some(transfer) if transfer.id == id && transfer.len == len => transfer,
            other => other.insert(Transfer::new(id, len)),
        };
        transfer.add(seq, body)
    }

    pub fn progress(&self) -> Progress {
        let Some(transfer) = &self.transfer else {
            return Progress {
                fraction: 0.0,
                remaining: None,
            };
        };
        let known = transfer.blocks.iter().filter(|b| b.is_some()).count();
        let total = transfer.blocks.len();
        let frames = transfer.seen.len();
        let elapsed = transfer.started.elapsed().as_secs_f64();
        let remaining = (frames >= 3 && elapsed > 0.0).then(|| {
            let per_second = (frames - 1) as f64 / elapsed;
            let needed = (total - known) as f64 * EXPECTED_OVERHEAD;
            Duration::from_secs_f64(needed / per_second)
        });
        Progress {
            fraction: known as f32 / total as f32,
            remaining: if known == total {
                Some(Duration::ZERO)
            } else {
                remaining
            },
        }
    }

    /// The whole blob, once every block is known.
    pub fn blob(&self) -> Option<&[u8]> {
        self.transfer.as_ref()?.blob.as_deref()
    }
}

impl Transfer {
    fn new(id: [u8; 4], len: usize) -> Self {
        Self {
            id,
            len,
            blocks: vec![None; block_count(len) as usize],
            pending: Vec::new(),
            seen: HashSet::new(),
            started: Instant::now(),
            blob: None,
        }
    }

    fn add(&mut self, seq: u32, body: &[u8]) -> bool {
        if self.blob.is_some() || !self.seen.insert(seq) {
            return false;
        }
        let mut unknown = BTreeSet::new();
        let mut body = body.to_vec();
        for index in mixed_blocks(&self.id, seq, self.blocks.len() as u32) {
            match &self.blocks[index] {
                Some(block) => xor_into(&mut body, block),
                None => {
                    unknown.insert(index);
                }
            }
        }
        self.take(unknown, body);
        if self.blocks.iter().all(Option::is_some) {
            let mut blob: Vec<u8> = self.blocks.iter().flatten().flatten().copied().collect();
            blob.truncate(self.len);
            if blob_id(&blob) == self.id {
                self.blob = Some(blob);
            } else {
                *self = Self::new(self.id, self.len);
            }
        }
        true
    }

    /// Notes a frame mixing `unknown` blocks into `body`, then solves every pending frame
    /// left with a single unknown block.
    fn take(&mut self, unknown: BTreeSet<usize>, body: Vec<u8>) {
        let mut solved = Vec::new();
        match unknown.len() {
            0 => return,
            1 => solved.push((*unknown.first().unwrap(), body)),
            _ => self.pending.push((unknown, body)),
        }
        while let Some((index, block)) = solved.pop() {
            if self.blocks[index].is_some() {
                continue;
            }
            for (unknown, body) in &mut self.pending {
                if unknown.remove(&index) {
                    xor_into(body, &block);
                }
            }
            self.pending
                .retain_mut(|(unknown, body)| match unknown.len() {
                    0 => false,
                    1 => {
                        solved.push((*unknown.first().unwrap(), std::mem::take(body)));
                        false
                    }
                    _ => true,
                });
            self.blocks[index] = Some(block);
        }
    }
}

fn block_count(len: usize) -> u32 {
    len.div_ceil(BLOCK_BYTES).max(1) as u32
}

fn blob_id(blob: &[u8]) -> [u8; 4] {
    Sha256::digest(blob)[..4].try_into().unwrap()
}

fn parse_frame(frame: &[u8]) -> Option<([u8; 4], usize, u32, &[u8])> {
    if frame.len() != HEADER_BYTES + BLOCK_BYTES || frame[0] != FORMAT {
        return None;
    }
    let id = frame[1..5].try_into().unwrap();
    let len = u32::from_be_bytes(frame[5..9].try_into().unwrap()) as usize;
    let seq = u32::from_be_bytes(frame[9..13].try_into().unwrap());
    (len <= MAX_BLOB_BYTES).then_some((id, len, seq, &frame[HEADER_BYTES..]))
}

fn xor_into(body: &mut [u8], block: &[u8]) {
    for (out, byte) in body.iter_mut().zip(block) {
        *out ^= byte;
    }
}

/// The blocks frame `seq` of `blocks` mixes: block `seq` alone for the first `blocks`
/// frames, then a number of blocks picked with weight 1/n for n blocks, each chosen at
/// random by a generator seeded with the blob id and `seq`.
fn mixed_blocks(id: &[u8; 4], seq: u32, blocks: u32) -> Vec<usize> {
    if seq < blocks {
        return vec![seq as usize];
    }
    let mut random = Random::seeded(id, seq);
    let weights = (1..=blocks).map(|n| 1.0 / f64::from(n));
    let mut pick = random.unit() * weights.clone().sum::<f64>();
    let mut degree = blocks;
    for (n, weight) in (1..).zip(weights) {
        pick -= weight;
        if pick < 0.0 {
            degree = n;
            break;
        }
    }
    let mut indices: Vec<usize> = (0..blocks as usize).collect();
    for i in 0..degree as usize {
        let j = i + random.below(indices.len() - i);
        indices.swap(i, j);
    }
    indices.truncate(degree as usize);
    indices
}

/// SHA-256 in counter mode: the same stream for the same seed on every device.
struct Random {
    seed: [u8; 8],
    counter: u64,
    buffer: Vec<u8>,
}

impl Random {
    fn seeded(id: &[u8; 4], seq: u32) -> Self {
        let mut seed = [0; 8];
        seed[..4].copy_from_slice(id);
        seed[4..].copy_from_slice(&seq.to_be_bytes());
        Self {
            seed,
            counter: 0,
            buffer: Vec::new(),
        }
    }

    fn next_u64(&mut self) -> u64 {
        if self.buffer.len() < 8 {
            let mut hasher = Sha256::new();
            hasher.update(self.seed);
            hasher.update(self.counter.to_be_bytes());
            self.counter += 1;
            self.buffer.extend_from_slice(&hasher.finalize());
        }
        let bytes: [u8; 8] = self
            .buffer
            .drain(..8)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        u64::from_be_bytes(bytes)
    }

    /// A number in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// `bytes` in base 45, as RFC 9285 writes it.
fn base45(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() / 2 * 3 + 2);
    for pair in bytes.chunks(2) {
        let (mut n, digits) = match pair {
            [a, b] => (usize::from(*a) * 256 + usize::from(*b), 3),
            [a] => (usize::from(*a), 2),
            _ => unreachable!(),
        };
        for _ in 0..digits {
            out.push(char::from(BASE45[n % 45]));
            n /= 45;
        }
    }
    out
}

fn unbase45(text: &str) -> Option<Vec<u8>> {
    let digits: Vec<usize> = text
        .bytes()
        .map(|c| BASE45.iter().position(|d| *d == c))
        .collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() / 3 * 2 + 1);
    for group in digits.chunks(3) {
        match group {
            [c, d, e] => {
                let n = c + d * 45 + e * 45 * 45;
                out.extend_from_slice(&u16::try_from(n).ok()?.to_be_bytes());
            }
            [c, d] => out.push(u8::try_from(c + d * 45).ok()?),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    #[test]
    fn base45_matches_rfc_9285() {
        assert_eq!(base45(b"AB"), "BB8");
        assert_eq!(base45(b"Hello!!"), "%69 VD92EX0");
        assert_eq!(base45(b"base-45"), "UJCLQE7W581");
        assert_eq!(unbase45("QED8WEX0").unwrap(), b"ietf!");
        assert_eq!(unbase45("GGW"), None);
        assert_eq!(unbase45("a"), None);
        for len in 0..10 {
            assert_eq!(unbase45(&base45(&blob(len))).unwrap(), blob(len));
        }
    }

    #[test]
    fn every_frame_in_order_gives_the_blob_after_the_block_count() {
        for len in [0, 1, BLOCK_BYTES, BLOCK_BYTES + 1, 20_000] {
            let fountain = Fountain::new(blob(len));
            let mut collector = Collector::new();
            for seq in 0..fountain.blocks() {
                assert!(collector.blob().is_none());
                assert!(collector.add(&fountain.frame(seq)));
            }
            assert_eq!(collector.blob(), Some(blob(len).as_slice()), "{len}");
            assert_eq!(collector.progress().fraction, 1.0);
        }
    }

    #[test]
    fn missed_frames_are_filled_in_from_later_ones() {
        let fountain = Fountain::new(blob(30_000));
        let blocks = fountain.blocks();
        let mut collector = Collector::new();
        let mut taken = 0;
        for seq in (0..).filter(|seq| seq % 3 != 0) {
            collector.add(&fountain.frame(seq));
            taken += 1;
            if collector.blob().is_some() {
                break;
            }
            assert!(taken < blocks * 3, "no blob after {taken} frames");
        }
        assert_eq!(collector.blob(), Some(blob(30_000).as_slice()));
        assert!(taken < blocks * 2, "{taken} frames for {blocks} blocks");
    }

    #[test]
    fn repeats_and_strangers_are_ignored_and_another_blob_starts_over() {
        let fountain = Fountain::new(blob(2_000));
        let mut collector = Collector::new();
        assert!(collector.add(&fountain.frame(0)));
        assert!(!collector.add(&fountain.frame(0)));
        assert!(!collector.add("HELLO"));
        assert!(!collector.add("not even base 45"));
        assert!(collector.progress().fraction > 0.0);

        let other = Fountain::new(blob(3_000));
        assert!(collector.add(&other.frame(1)));
        for seq in 0..other.blocks() {
            collector.add(&other.frame(seq));
        }
        assert_eq!(collector.blob(), Some(blob(3_000).as_slice()));
    }

    #[test]
    fn a_frame_claiming_a_huge_blob_is_refused() {
        let mut frame = vec![FORMAT, 1, 2, 3, 4];
        frame.extend_from_slice(&(MAX_BLOB_BYTES as u32 + 1).to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&[0; BLOCK_BYTES]);
        assert!(!Collector::new().add(&base45(&frame)));
    }

    #[test]
    fn the_estimate_shrinks_to_nothing() {
        let fountain = Fountain::new(blob(5_000));
        let mut collector = Collector::new();
        assert_eq!(collector.progress().remaining, None);
        for seq in 0..fountain.blocks() {
            std::thread::sleep(Duration::from_millis(2));
            collector.add(&fountain.frame(seq));
            if seq == 3 {
                assert!(
                    collector
                        .progress()
                        .remaining
                        .is_some_and(|r| r > Duration::ZERO)
                );
            }
        }
        assert_eq!(collector.progress().remaining, Some(Duration::ZERO));
    }
}
