#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::invite::{self, InviteCode};

fuzz_target!(|data: &[u8]| {
    if let Some(bytes) = data.first_chunk() {
        let code = InviteCode::from_random(*bytes);
        assert_eq!(code.to_string().parse::<InviteCode>(), Ok(code));
    }

    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let normalized = invite::normalize(text);
    assert_eq!(invite::normalize(&normalized), normalized);
    assert!(invite::format_partial(text).len() <= 9);
    if let Ok(code) = text.parse::<InviteCode>() {
        assert_eq!(code.to_string().parse::<InviteCode>(), Ok(code));
    }
});
