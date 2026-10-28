#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_client_core::media::fragment::{Reassembler, ReplayWindow, Seen};
use throcc_proto::frame::Flags;

fuzz_target!(|data: &[u8]| {
    let mut window = ReplayWindow::default();
    let mut reassembler = Reassembler::default();
    for chunk in data.chunks(8) {
        let Some((head, body)) = chunk.split_first_chunk::<3>() else { break };
        let seq = u32::from(u16::from_be_bytes([head[0], head[1]]));
        if window.check(seq) == Seen::New
            && let Some(unit) = reassembler.push(seq, Flags(head[2]), body)
        {
            assert!(unit.first_seq <= unit.last_seq);
        }
    }
    let _ = window.lost();
});
