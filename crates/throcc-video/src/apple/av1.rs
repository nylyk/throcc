use std::ptr::NonNull;

use dav1d_sys as dav1d;
use dav1d_sys::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422, Dav1dSequenceHeader,
};
use objc2_core_foundation::{CFData, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMVideoFormatDescriptionCreate,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMVideoCodecType_AV1,
};

use super::{NO_ERR, attributes};
use crate::DecodeError;

const SEQUENCE_HEADER: u8 = 1;
const TEMPORAL_DELIMITER: u8 = 2;

/// One AV1 temporal unit as VideoToolbox takes it.
#[derive(Debug, Default, PartialEq)]
pub struct Split<'a> {
    /// The sequence header OBU, header included, if the unit carries one.
    pub sequence_header: Option<&'a [u8]>,
    /// The unit's OBUs without temporal delimiters, which AV1 samples leave out.
    pub sample: Vec<u8>,
}

/// Splits a temporal unit of OBUs. A damaged OBU ends the unit there.
pub fn split(unit: &[u8]) -> Split<'_> {
    let mut split = Split::default();
    let mut rest = unit;
    while let Some((obu, kind)) = next_obu(rest) {
        rest = &rest[obu.len()..];
        match kind {
            TEMPORAL_DELIMITER => continue,
            SEQUENCE_HEADER => split.sequence_header = Some(obu),
            _ => {}
        }
        split.sample.extend_from_slice(obu);
    }
    split
}

/// The first OBU of `data`, header included, and its type. An OBU without a size field runs
/// to the end.
fn next_obu(data: &[u8]) -> Option<(&[u8], u8)> {
    let header = *data.first()?;
    let kind = (header >> 3) & 0xf;
    let extension = usize::from(header & 0x4 != 0);
    let has_size = header & 0x2 != 0;
    let mut at = 1 + extension;
    if !has_size {
        return (data.len() >= at).then_some((data, kind));
    }
    let mut size: u64 = 0;
    for byte in 0..8 {
        let value = *data.get(at)?;
        at += 1;
        size |= u64::from(value & 0x7f) << (7 * byte);
        if value & 0x80 == 0 {
            let end = at.checked_add(usize::try_from(size).ok()?)?;
            return data.get(..end).map(|obu| (obu, kind));
        }
    }
    None
}

/// A format description for AV1 with the `av1C` box made from `sequence_header`.
pub fn format_description(
    sequence_header: &[u8],
) -> Result<CFRetained<CMFormatDescription>, DecodeError> {
    // SAFETY: dav1d fills in every field it parses; the rest stay zero.
    let mut parsed: Dav1dSequenceHeader = unsafe { std::mem::zeroed() };
    // SAFETY: the header and the slice are live for the call.
    let status = unsafe {
        dav1d::dav1d_parse_sequence_header(
            &mut parsed,
            sequence_header.as_ptr(),
            sequence_header.len(),
        )
    };
    if status != 0 {
        return Err(DecodeError::Dav1d(status));
    }
    let record = CFData::from_bytes(&configuration_record(&parsed, sequence_header));
    let box_name = CFString::from_static_str("av1C");
    let boxes = attributes::dictionary(&[(&box_name, &record)]);
    // SAFETY: CoreMedia's key is an immutable static.
    let extensions = attributes::dictionary(&[(
        unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms },
        &boxes as &CFType,
    )]);
    let mut raw: *const CMFormatDescription = std::ptr::null();
    // SAFETY: the dictionary is live for the call, which copies it.
    let status = unsafe {
        CMVideoFormatDescriptionCreate(
            None,
            kCMVideoCodecType_AV1,
            parsed.max_width,
            parsed.max_height,
            Some(extensions.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    if status != NO_ERR {
        return Err(DecodeError::VideoToolbox(status));
    }
    let raw = NonNull::new(raw.cast_mut()).ok_or(DecodeError::VideoToolbox(status))?;
    // SAFETY: a successful create returned an owned reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// The AV1CodecConfigurationRecord of the `av1C` box, with the sequence header as its one
/// configuration OBU.
fn configuration_record(parsed: &Dav1dSequenceHeader, sequence_header: &[u8]) -> Vec<u8> {
    let point = &parsed.operating_points[0];
    let level = (point.major_level.saturating_sub(2) << 2 | point.minor_level) & 0x1f;
    let monochrome = parsed.layout == DAV1D_PIXEL_LAYOUT_I400;
    let (subsampled_x, subsampled_y) = match parsed.layout {
        DAV1D_PIXEL_LAYOUT_I400 | DAV1D_PIXEL_LAYOUT_I420 => (true, true),
        DAV1D_PIXEL_LAYOUT_I422 => (true, false),
        _ => (false, false),
    };
    let flags = [
        (point.tier != 0, 7),
        (parsed.hbd > 0, 6),
        (parsed.hbd == 2, 5),
        (monochrome, 4),
        (subsampled_x, 3),
        (subsampled_y, 2),
    ];
    let bits = flags
        .iter()
        .fold(0u8, |byte, &(set, bit)| byte | u8::from(set) << bit);
    let mut record = vec![
        0x81,
        (parsed.profile & 0x7) << 5 | level,
        bits | (parsed.chr as u8 & 0x3),
        0,
    ];
    record.extend_from_slice(sequence_header);
    record
}

/// Whether this device decodes AV1 in hardware through VideoToolbox.
pub fn decodes_in_hardware() -> bool {
    // SAFETY: plain calls with a codec type; registering is a no-op where unneeded.
    unsafe {
        #[cfg(target_os = "macos")]
        objc2_video_toolbox::VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1);
        objc2_video_toolbox::VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELIMITER: [u8; 2] = [0x12, 0x00];

    #[test]
    fn delimiters_go_and_the_sequence_header_is_found() {
        let header = [0x0a, 0x03, 1, 2, 3];
        let frame = [0x32, 0x02, 9, 9];
        let unit = [&DELIMITER[..], &header, &frame].concat();
        let split = split(&unit);
        assert_eq!(split.sequence_header, Some(&header[..]));
        assert_eq!(split.sample, [&header[..], &frame].concat());
    }

    #[test]
    fn an_obu_longer_than_the_unit_ends_it() {
        let frame = [0x32, 0x02, 9, 9];
        let cut = [0x32, 0x7f, 1];
        let unit = [&frame[..], &cut].concat();
        assert_eq!(split(&unit).sample, frame);
    }

    #[test]
    fn a_size_past_eight_bytes_ends_the_unit() {
        let unit = [0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(split(&unit), Split::default());
    }

    #[test]
    fn the_record_describes_main_profile_8_bit_4_2_0() {
        let file = include_bytes!("../../testdata/pattern.ivf");
        let first = crate::software::dav1d::ivf_units(file)[0];
        let header = split(first).sequence_header.unwrap();
        // SAFETY: dav1d fills in every field it parses; the rest stay zero.
        let mut parsed: Dav1dSequenceHeader = unsafe { std::mem::zeroed() };
        // SAFETY: the header and the slice are live for the call.
        let status = unsafe {
            dav1d::dav1d_parse_sequence_header(&mut parsed, header.as_ptr(), header.len())
        };
        assert_eq!(status, 0);
        let record = configuration_record(&parsed, header);
        // Version 1, profile 0 at level 2.0, tier 0, 8-bit 4:2:0, no presentation delay.
        assert_eq!(record[..4], [0x81, 0x00, 0x0c, 0x00]);
        assert_eq!(&record[4..], header);
    }

    #[test]
    fn the_record_carries_the_sequence_header_of_the_test_stream() {
        let file = include_bytes!("../../testdata/pattern.ivf");
        let first = crate::software::dav1d::ivf_units(file)[0];
        let header = split(first).sequence_header.unwrap();
        let description = format_description(header).unwrap();
        // SAFETY: a description made for AV1 is a video one.
        let size = unsafe { objc2_core_media::CMVideoFormatDescriptionGetDimensions(&description) };
        assert_eq!((size.width, size.height), (320, 180));
    }
}
