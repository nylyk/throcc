//! Profile pictures: the one we choose, made small enough for any server, and the ones
//! others chose, read only as the JPEG we make.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::error::{ImageFormatHint, UnsupportedError};
use image::imageops::FilterType;
use image::{
    DynamicImage, ImageError, ImageFormat, ImageReader, ImageResult, Limits, Rgb, RgbImage,
    RgbaImage,
};

const SIDE: u32 = 128;
const JPEG_QUALITY: u8 = 85;
const MAX_CHOSEN_SIDE: u32 = 8192;
const MAX_CHOSEN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHOWN_SIDE: u32 = 512;
const MAX_SHOWN_BYTES: u64 = 4 * 1024 * 1024;
/// The format [`prepare`] writes, and so the only one pictures are read in. The platforms'
/// own decoders read many more.
const STORED_FORMATS: &[ImageFormat] = &[ImageFormat::Jpeg];
/// What a transparent part of a chosen picture becomes.
const BACKDROP: Rgb<u8> = Rgb([255, 255, 255]);
/// The everyday formats a chosen picture may come in. Rarer decoders stay unused, so a stray
/// file can't reach them.
const CHOSEN_FORMATS: &[ImageFormat] = &[
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::Bmp,
    ImageFormat::WebP,
];

/// Turns a picture the user chose, in one of [`CHOSEN_FORMATS`], into a centred 128 px JPEG
/// square, which fits the server's size cap.
pub fn prepare(file: &[u8]) -> Result<Vec<u8>, String> {
    let picture = decode_within(file, MAX_CHOSEN_SIDE, MAX_CHOSEN_BYTES, CHOSEN_FORMATS)
        .map_err(|e| match e {
            ImageError::Limits(_) => format!(
                "That picture is too large. Pick one up to {MAX_CHOSEN_SIDE}×{MAX_CHOSEN_SIDE} pixels."
            ),
            _ => "That file isn’t a picture throcc can read.".into(),
        })?;
    let side = picture.width().min(picture.height());
    let square = picture.crop_imm(
        (picture.width() - side) / 2,
        (picture.height() - side) / 2,
        side,
        side,
    );
    let small = on_backdrop(
        &square
            .resize_exact(SIDE, SIDE, FilterType::Lanczos3)
            .into_rgba8(),
    );
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode_image(&small)
        .map_err(|e| e.to_string())?;
    Ok(jpeg)
}

fn on_backdrop(picture: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(picture.width(), picture.height(), |x, y| {
        let [r, g, b, a] = picture.get_pixel(x, y).0;
        let mix = |c: u8, under: u8| {
            ((u16::from(c) * u16::from(a) + u16::from(under) * u16::from(255 - a) + 127) / 255)
                as u8
        };
        Rgb([
            mix(r, BACKDROP[0]),
            mix(g, BACKDROP[1]),
            mix(b, BACKDROP[2]),
        ])
    })
}

/// Decodes a stored picture from someone else. A picture over 512 px a side is refused
/// before its pixels are allocated, and one over the 128 px every picture is prepared at is
/// shrunk to it, so each one kept stays small.
pub fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut picture = decode_within(bytes, MAX_SHOWN_SIDE, MAX_SHOWN_BYTES, STORED_FORMATS).ok()?;
    if picture.width().max(picture.height()) > SIDE {
        picture = picture.thumbnail(SIDE, SIDE);
    }
    Some(picture.into_rgba8())
}

/// Whether `bytes` is a picture as [`prepare`] makes them, which may be shown: a JPEG of at
/// most 512 px a side. Only its header is read, so a platform's own decoder can show
/// it without any rarer format reaching that decoder.
pub fn showable(bytes: &[u8]) -> bool {
    let Ok(reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
        return false;
    };
    if reader.format().is_none_or(|f| !STORED_FORMATS.contains(&f)) {
        return false;
    }
    reader
        .into_dimensions()
        .is_ok_and(|(w, h)| w.max(h) <= MAX_SHOWN_SIDE && w > 0 && h > 0)
}

/// Decodes `bytes` within the size limits, if it is in one of `formats`.
fn decode_within(
    bytes: &[u8],
    max_side: u32,
    max_bytes: u64,
    formats: &[ImageFormat],
) -> ImageResult<DynamicImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(max_bytes);
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    if reader
        .format()
        .is_none_or(|format| !formats.contains(&format))
    {
        let hint = reader
            .format()
            .map_or(ImageFormatHint::Unknown, ImageFormatHint::Exact);
        return Err(ImageError::Unsupported(UnsupportedError::from(hint)));
    }
    reader.limits(limits);
    reader.decode()
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};
    use throcc_proto::messages::MAX_AVATAR_BYTES;

    use super::*;

    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let noise = ImageBuffer::from_fn(width, height, |x, y| {
            let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) as u8;
            Rgb([v, v.wrapping_mul(3), v.wrapping_add(x as u8)])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(noise)
            .write_to(&mut Cursor::new(&mut out), format)
            .unwrap();
        out
    }

    #[test]
    fn any_picture_becomes_a_small_square() {
        for (width, height, format) in [
            (400, 225, ImageFormat::Png),
            (150, 1000, ImageFormat::Jpeg),
            (64, 64, ImageFormat::Png),
        ] {
            let prepared = prepare(&encoded(width, height, format)).unwrap();
            assert!(
                prepared.len() <= MAX_AVATAR_BYTES,
                "{} bytes",
                prepared.len()
            );
            let back = image::load_from_memory(&prepared).unwrap();
            assert_eq!((back.width(), back.height()), (SIDE, SIDE));
        }
    }

    #[test]
    fn transparency_becomes_white() {
        let clear = RgbaImage::from_pixel(32, 32, Rgba([10, 200, 30, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(clear)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let prepared = decode(&prepare(&png).unwrap()).unwrap();
        let [r, g, b, a] = prepared.get_pixel(SIDE / 2, SIDE / 2).0;
        assert!(r > 250 && g > 250 && b > 250 && a == 255, "{r} {g} {b} {a}");
    }

    #[test]
    fn something_else_is_refused() {
        assert!(prepare(b"not a picture").is_err());
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
        !crc
    }

    fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    }

    /// A JPEG of a few dozen bytes whose header claims a 20000 × 20000 greyscale picture.
    fn huge_header_jpeg() -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08];
        jpeg.extend_from_slice(&20_000u16.to_be_bytes());
        jpeg.extend_from_slice(&20_000u16.to_be_bytes());
        jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        jpeg.extend_from_slice(&[0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00]);
        jpeg.extend_from_slice(&[0x00, 0xff, 0xd9]);
        jpeg
    }

    /// A PNG of a few dozen bytes whose header claims a 1-bit 20000 × 20000 picture.
    fn huge_header_png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&20_000u32.to_be_bytes());
        header.extend_from_slice(&20_000u32.to_be_bytes());
        header.extend_from_slice(&[1, 0, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &header);
        chunk(&mut png, b"IDAT", &[0x78, 0x9c, 0x03, 0x00]);
        chunk(&mut png, b"IEND", &[]);
        png
    }

    #[test]
    fn a_huge_declared_size_is_refused_before_decoding() {
        let (png, jpeg) = (huge_header_png(), huge_header_jpeg());
        assert!(png.len() < 100);
        let started = std::time::Instant::now();
        assert!(matches!(
            decode_within(&jpeg, MAX_SHOWN_SIDE, MAX_SHOWN_BYTES, STORED_FORMATS),
            Err(ImageError::Limits(_))
        ));
        for bomb in [&png, &jpeg] {
            assert!(matches!(
                decode_within(bomb, MAX_CHOSEN_SIDE, MAX_CHOSEN_BYTES, CHOSEN_FORMATS),
                Err(ImageError::Limits(_))
            ));
            assert!(decode(bomb).is_none());
            assert!(prepare(bomb).unwrap_err().contains("too large"));
        }
        // Decoding 20000×20000 pixels would take far longer, even in a debug build.
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_stored_picture_decodes() {
        let stored = encoded(SIDE, SIDE, ImageFormat::Jpeg);
        let picture = decode(&stored).unwrap();
        assert_eq!(picture.dimensions(), (SIDE, SIDE));
        assert!(showable(&stored));
    }

    #[test]
    fn a_large_picture_is_kept_at_the_prepared_size() {
        let picture = decode(&encoded(
            MAX_SHOWN_SIDE,
            MAX_SHOWN_SIDE / 2,
            ImageFormat::Jpeg,
        ))
        .unwrap();
        assert_eq!(picture.dimensions(), (SIDE, SIDE / 2));
    }

    #[test]
    fn only_jpeg_is_read_and_everyday_formats_can_be_chosen() {
        for format in [ImageFormat::Png, ImageFormat::Bmp, ImageFormat::Gif] {
            let other = encoded(16, 16, format);
            assert!(
                image::load_from_memory(&other).is_ok(),
                "{format:?} can be decoded at all"
            );
            assert!(decode(&other).is_none(), "{format:?}");
            assert!(!showable(&other), "{format:?}");
            assert!(
                decode(&prepare(&other).unwrap()).is_some(),
                "{format:?} prepared"
            );
        }
    }

    #[test]
    fn a_picture_wider_than_shown_pictures_is_refused() {
        let wide = encoded(MAX_SHOWN_SIDE + 1, 16, ImageFormat::Jpeg);
        assert!(decode(&wide).is_none());
        assert!(!showable(&wide));
        assert!(!showable(&huge_header_jpeg()));
        assert!(!showable(b"not a picture"));
    }
}
