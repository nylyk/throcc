#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use throcc_client_core::media::fragment::{
    MAX_AUDIO_UNIT_FRAGMENTS, MAX_VIDEO_UNIT_FRAGMENTS, Reassembler, ReplayWindow, Seen,
};
use throcc_proto::frame::{Flags, TIMESTAMP_BYTES};

fuzz_target!(|data: &[u8]| {
    let Some((&kind, data)) = data.split_first() else { return };
    let max = if kind & 1 == 0 { MAX_AUDIO_UNIT_FRAGMENTS } else { MAX_VIDEO_UNIT_FRAGMENTS };
    let mut window = ReplayWindow::default();
    let mut reassembler = Reassembler::new(max);
    let mut lengths = HashMap::new();
    for chunk in data.chunks(8) {
        let Some((head, body)) = chunk.split_first_chunk::<3>() else { break };
        let seq = u32::from(u16::from_be_bytes([head[0], head[1]]));
        let seen = window.peek(seq);
        assert_eq!(window.check(seq), seen);
        if seen != Seen::New {
            continue;
        }
        lengths.insert(seq, body.len());
        if let Some(unit) = reassembler.push(seq, Flags(head[2]), body) {
            assert!(unit.first_seq <= unit.last_seq);
            assert!(((unit.last_seq - unit.first_seq) as usize) < max);
            let pushed: usize = (unit.first_seq..=unit.last_seq).map(|s| lengths[&s]).sum();
            assert_eq!(unit.data.len() + TIMESTAMP_BYTES, pushed);
            reassembler.forget_before(unit.first_seq);
        }
    }
    let _ = window.lost();
});
