use std::sync::OnceLock;

use throcc_proto::messages::Codec;

/// Whether `THROCC_SOFTWARE_DECODE` asks for software decoders, read once.
pub fn software_decoding() -> bool {
    static SET: OnceLock<bool> = OnceLock::new();
    *SET.get_or_init(|| std::env::var("THROCC_SOFTWARE_DECODE").is_ok_and(|value| value != "0"))
}

/// Whether encoders may use `codec`: any, unless `THROCC_CODEC` names one, read once.
pub fn encodes(codec: Codec) -> bool {
    static ONLY: OnceLock<Option<Codec>> = OnceLock::new();
    let only = ONLY.get_or_init(|| {
        let named = std::env::var("THROCC_CODEC").ok()?;
        parse(&named)
    });
    only.is_none_or(|only| only == codec)
}

fn parse(named: &str) -> Option<Codec> {
    match named.to_ascii_lowercase().as_str() {
        "av1" => Some(Codec::Av1),
        "hevc" | "h265" => Some(Codec::Hevc),
        "h264" | "avc" => Some(Codec::H264),
        "vp9" => Some(Codec::Vp9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_parse_by_their_usual_names() {
        assert_eq!(parse("AV1"), Some(Codec::Av1));
        assert_eq!(parse("h265"), Some(Codec::Hevc));
        assert_eq!(parse("avc"), Some(Codec::H264));
        assert_eq!(parse("vp9"), Some(Codec::Vp9));
        assert_eq!(parse("mpeg2"), None);
    }
}
