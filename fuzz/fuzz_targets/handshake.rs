#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::framing;
use throcc_proto::messages::{AuthResult, PakeReply, PakeStep, ServerHello};

fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    while let Ok(Some((body, tail))) = framing::split(rest) {
        let _ = framing::decode::<ServerHello>(body);
        let _ = framing::decode::<PakeStep>(body);
        let _ = framing::decode::<PakeReply>(body);
        let _ = framing::decode::<AuthResult>(body);
        rest = tail;
    }
});
