#![no_main]

use libfuzzer_sys::fuzz_target;
use throcc_proto::frame::Header;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = Header::parse(data) {
        assert_eq!(Header::parse(&header.encode()), Ok(header));
        match Header::parse_strict(data) {
            Ok(strict) => assert_eq!(strict, header),
            Err(_) => assert!(header.flags.has_reserved()),
        }
    }
});
