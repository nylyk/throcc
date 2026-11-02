#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use throcc_crypto::media::TrackKey;
use throcc_proto::frame::{HEADER_BYTES, Header};
use throcc_proto::ids::MediaId;

static KEY: LazyLock<TrackKey> = LazyLock::new(|| TrackKey::derive(&[7; 32], MediaId(1)));

fuzz_target!(|data: &[u8]| {
    let _ = KEY.open(&mut data.to_vec());

    if let Ok(header) = Header::parse_strict(data) {
        let plaintext = &data[HEADER_BYTES..];
        let mut sealed = Vec::new();
        if KEY.seal(&header, &[plaintext], &mut sealed).is_ok() {
            assert_eq!(KEY.open(&mut sealed), Ok((header, plaintext)));
        }
    }
});
