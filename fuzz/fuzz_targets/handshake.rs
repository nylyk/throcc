#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::framing;
use throcc_proto::messages::{AuthResult, ClientHello, PakeReply, PakeStep, ServerHello};

fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    while let Ok(Some((body, tail))) = framing::split(rest) {
        let _ = framing::decode::<ServerHello>(body);
        let protocol = ClientHello::protocol_of(body);
        if let Ok(hello) = framing::decode::<ClientHello>(body) {
            assert_eq!(protocol.ok(), Some(hello.protocol));
        }
        let _ = framing::decode::<PakeStep>(body);
        let _ = framing::decode::<PakeReply>(body);
        let _ = framing::decode::<AuthResult>(body);
        rest = tail;
    }
});
