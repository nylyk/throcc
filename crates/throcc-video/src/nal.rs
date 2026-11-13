//! H.264 and HEVC access units as senders send them, Annex B with start codes and the
//! parameter sets before each keyframe, and as VideoToolbox takes them: NAL units each
//! behind a 4-byte length, with the parameter sets kept apart in a format description.

use throcc_proto::messages::Codec;

/// The bytes of the length before each NAL unit VideoToolbox reads and writes.
pub const LENGTH_SIZE: usize = 4;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// What a NAL unit holds, as far as converting between the two forms cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A parameter set, which VideoToolbox takes in the format description.
    ParameterSet,
    /// An access unit delimiter, which VideoToolbox doesn't want.
    Delimiter,
    /// A slice of a picture that decodes on its own.
    Keyframe,
    Other,
}

/// The kind of `nal` in `codec`, which must be H.264 or HEVC.
pub fn kind(codec: Codec, nal: &[u8]) -> Kind {
    let Some(&header) = nal.first() else {
        return Kind::Other;
    };
    match codec {
        Codec::H264 => match header & 0x1f {
            7 | 8 => Kind::ParameterSet,
            9 => Kind::Delimiter,
            5 => Kind::Keyframe,
            _ => Kind::Other,
        },
        _ => match (header >> 1) & 0x3f {
            32..=34 => Kind::ParameterSet,
            35 => Kind::Delimiter,
            16..=21 => Kind::Keyframe,
            _ => Kind::Other,
        },
    }
}

/// The NAL units of an Annex B access unit, without their start codes.
pub fn units(annex_b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = annex_b;
    std::iter::from_fn(move || {
        let start = start_code_at(rest)?;
        let body = &rest[start.end..];
        let end = start_code_at(body).map_or(body.len(), |next| next.start);
        let unit = trim_trailing_zeros(&body[..end]);
        rest = &body[end..];
        Some(unit)
    })
    .filter(|unit| !unit.is_empty())
}

/// Where the first start code (`00 00 01`, or with a leading zero) in `data` lies.
fn start_code_at(data: &[u8]) -> Option<std::ops::Range<usize>> {
    let at = data.windows(3).position(|w| w == [0, 0, 1])?;
    Some(at..at + 3)
}

/// `unit` without the zeros that pad it up to the next start code.
fn trim_trailing_zeros(unit: &[u8]) -> &[u8] {
    let end = unit.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    &unit[..end]
}

/// An Annex B access unit taken apart: its parameter sets, and everything else as 4-byte
/// length-prefixed NAL units.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Split<'a> {
    pub parameter_sets: Vec<&'a [u8]>,
    pub length_prefixed: Vec<u8>,
    pub keyframe: bool,
}

pub fn split(codec: Codec, annex_b: &[u8]) -> Split<'_> {
    let mut split = Split::default();
    for unit in units(annex_b) {
        match kind(codec, unit) {
            Kind::ParameterSet => split.parameter_sets.push(unit),
            Kind::Delimiter => {}
            other => {
                split.keyframe |= other == Kind::Keyframe;
                let Ok(len) = u32::try_from(unit.len()) else {
                    continue;
                };
                split.length_prefixed.extend_from_slice(&len.to_be_bytes());
                split.length_prefixed.extend_from_slice(unit);
            }
        }
    }
    split
}

/// An access unit of 4-byte length-prefixed NAL units as Annex B, after `parameter_sets`.
/// A length that runs past the end ends the unit there.
pub fn to_annex_b(parameter_sets: &[&[u8]], length_prefixed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(length_prefixed.len() + 64);
    for set in parameter_sets {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(set);
    }
    let mut rest = length_prefixed;
    while let Some((len, body)) = rest.split_first_chunk::<LENGTH_SIZE>() {
        let len = u32::from_be_bytes(*len) as usize;
        let Some(unit) = body.get(..len) else {
            break;
        };
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(unit);
        rest = &body[len..];
    }
    out
}

/// Whether an Annex B access unit holds a keyframe.
pub fn is_keyframe(codec: Codec, annex_b: &[u8]) -> bool {
    units(annex_b).any(|unit| kind(codec, unit) == Kind::Keyframe)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x1f];
    const PPS: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01];
    const P: &[u8] = &[0x41, 0x9a, 0x02];

    fn annex_b(units: &[&[u8]], long_start: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            if long_start {
                out.push(0);
            }
            out.extend_from_slice(&[0, 0, 1]);
            out.extend_from_slice(unit);
        }
        out
    }

    #[test]
    fn units_split_on_both_start_codes() {
        for long_start in [false, true] {
            let data = annex_b(&[SPS, PPS, IDR], long_start);
            assert_eq!(units(&data).collect::<Vec<_>>(), [SPS, PPS, IDR]);
        }
        let mixed = [&[0, 0, 0, 1][..], SPS, &[0, 0, 1], PPS].concat();
        assert_eq!(units(&mixed).collect::<Vec<_>>(), [SPS, PPS]);
        assert_eq!(units(&[]).count(), 0);
        assert_eq!(units(&[0, 0, 0]).count(), 0);
    }

    #[test]
    fn a_keyframe_splits_into_parameter_sets_and_lengths_and_back() {
        let delimiter: &[u8] = &[0x09, 0xf0];
        let data = annex_b(&[delimiter, SPS, PPS, IDR], true);
        let split = split(Codec::H264, &data);
        assert_eq!(split.parameter_sets, [SPS, PPS]);
        assert!(split.keyframe);
        let mut expected = vec![0, 0, 0, IDR.len() as u8];
        expected.extend_from_slice(IDR);
        assert_eq!(split.length_prefixed, expected);
        let back = to_annex_b(&split.parameter_sets, &split.length_prefixed);
        assert_eq!(back, annex_b(&[SPS, PPS, IDR], true));
        assert!(is_keyframe(Codec::H264, &back));
    }

    #[test]
    fn a_predicted_picture_is_no_keyframe() {
        let data = annex_b(&[P, P], false);
        let split = split(Codec::H264, &data);
        assert!(split.parameter_sets.is_empty() && !split.keyframe);
        assert!(!is_keyframe(Codec::H264, &data));
        assert_eq!(
            to_annex_b(&[], &split.length_prefixed),
            annex_b(&[P, P], true)
        );
    }

    #[test]
    fn hevc_kinds_come_from_the_six_bit_type() {
        let vps: &[u8] = &[0x40, 0x01];
        let sps: &[u8] = &[0x42, 0x01];
        let pps: &[u8] = &[0x44, 0x01];
        let idr: &[u8] = &[0x26, 0x01, 0xaf];
        let cra: &[u8] = &[0x2a, 0x01];
        let trail: &[u8] = &[0x02, 0x01];
        let delimiter: &[u8] = &[0x46, 0x01, 0x50];
        for set in [vps, sps, pps] {
            assert_eq!(kind(Codec::Hevc, set), Kind::ParameterSet);
        }
        assert_eq!(kind(Codec::Hevc, idr), Kind::Keyframe);
        assert_eq!(kind(Codec::Hevc, cra), Kind::Keyframe);
        assert_eq!(kind(Codec::Hevc, trail), Kind::Other);
        assert_eq!(kind(Codec::Hevc, delimiter), Kind::Delimiter);
    }

    #[test]
    fn a_length_past_the_end_ends_the_unit() {
        let mut data = vec![0, 0, 0, 3];
        data.extend_from_slice(P);
        data.extend_from_slice(&[0, 0, 0, 9, 1, 2]);
        assert_eq!(to_annex_b(&[], &data), annex_b(&[P], true));
    }
}
