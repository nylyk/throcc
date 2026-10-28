#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::framing;
use throcc_proto::messages::{ClientMessage, Hello, PakeFinish, ServerMessage};

fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    while let Ok(Some((body, tail))) = framing::split(rest) {
        if let Ok(message) = framing::decode::<ClientMessage>(body) {
            let frame = framing::encode(&message).expect("a decoded message re-encodes");
            assert_eq!(framing::decode::<ClientMessage>(&frame[4..]).ok(), Some(message));
        }
        let _ = framing::decode::<ServerMessage>(body);
        let _ = framing::decode::<Hello>(body);
        let _ = framing::decode::<PakeFinish>(body);
        rest = tail;
    }
});
