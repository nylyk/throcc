#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::Fingerprint;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(fingerprint) = text.parse::<Fingerprint>() {
        assert_eq!(
            fingerprint.to_string().parse::<Fingerprint>().ok(),
            Some(fingerprint)
        );
        let _ = fingerprint.short();
    }
});
