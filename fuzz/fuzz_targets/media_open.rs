#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_crypto::media::TrackKey;
use throcc_proto::frame::{HEADER_BYTES, Header};
use throcc_proto::ids::MediaId;

fuzz_target!(|data: &[u8]| {
    let key = TrackKey::derive(&[7; 32], MediaId(1));
    let _ = key.open(&mut data.to_vec());

    if let Ok(header) = Header::parse_strict(data) {
        let plaintext = &data[HEADER_BYTES..];
        let mut sealed = Vec::new();
        if key.seal(&header, &[plaintext], &mut sealed).is_ok() {
            assert_eq!(key.open(&mut sealed), Ok((header, plaintext)));
        }
    }
});
