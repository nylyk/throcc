use std::io::Cursor;
use std::sync::Arc;

use gpui::RenderImage;
use image::codecs::jpeg::JpegEncoder;
use image::error::{ImageFormatHint, UnsupportedError};
use image::imageops::FilterType;
use image::{DynamicImage, ImageError, ImageFormat, ImageReader, ImageResult, Limits};
use throcc_proto::messages::MAX_AVATAR_BYTES;

const SIDE: u32 = 128;
const JPEG_QUALITY: u8 = 85;
const MAX_CHOSEN_SIDE: u32 = 8192;
const MAX_CHOSEN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHOWN_SIDE: u32 = 512;
const MAX_SHOWN_BYTES: u64 = 4 * 1024 * 1024;
/// The formats [`prepare`] writes, and so the only ones pictures from others are read in.
/// gpui turns on many more of `image`'s decoders.
const STORED_FORMATS: &[ImageFormat] = &[ImageFormat::Png, ImageFormat::Jpeg];

/// Turns a picture the user chose, in any format `image` reads, into a centred 128 px PNG
/// or JPEG square that fits the server's size cap.
pub fn prepare(file: &[u8]) -> Result<Vec<u8>, String> {
    let picture = decode_within(file, MAX_CHOSEN_SIDE, MAX_CHOSEN_BYTES, None).map_err(|e| {
        match e {
            ImageError::Limits(_) => format!(
                "That picture is too large. Pick one up to {MAX_CHOSEN_SIDE}×{MAX_CHOSEN_SIDE} pixels."
            ),
            _ => "That file isn’t a picture throcc can read.".into(),
        }
    })?;
    let side = picture.width().min(picture.height());
    let square = picture.crop_imm(
        (picture.width() - side) / 2,
        (picture.height() - side) / 2,
        side,
        side,
    );
    let small = square.resize_exact(SIDE, SIDE, FilterType::Lanczos3);
    let mut png = Vec::new();
    small
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    if png.len() <= MAX_AVATAR_BYTES {
        return Ok(png);
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode_image(&DynamicImage::ImageRgb8(small.into_rgb8()))
        .map_err(|e| e.to_string())?;
    Ok(jpeg)
}

/// A generated picture for the mock client's screenshots.
pub fn sample() -> Vec<u8> {
    let gradient = image::ImageBuffer::from_fn(SIDE, SIDE, |x, y| {
        image::Rgb([(x * 2) as u8, (y * 2) as u8, 180 - (x + y) as u8 / 2])
    });
    let mut png = Vec::new();
    DynamicImage::ImageRgb8(gradient)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok();
    png
}

/// Decodes a stored picture for gpui, which takes pixels in BGRA order.
/// A picture over 512 px a side is refused before its pixels are allocated, and one over
/// the 128 px every picture is prepared at is shrunk to it, so each one kept stays small.
pub fn decode(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut picture =
        decode_within(bytes, MAX_SHOWN_SIDE, MAX_SHOWN_BYTES, Some(STORED_FORMATS)).ok()?;
    if picture.width().max(picture.height()) > SIDE {
        picture = picture.thumbnail(SIDE, SIDE);
    }
    let mut pixels = picture.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(pixels)
    ])))
}

/// Decodes `bytes` within the size limits, in one of `formats` if given.
fn decode_within(
    bytes: &[u8],
    max_side: u32,
    max_bytes: u64,
    formats: Option<&[ImageFormat]>,
) -> ImageResult<DynamicImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(max_bytes);
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let refused = |format| formats.is_some_and(|allowed| !allowed.contains(&format));
    if reader.format().is_none_or(refused) {
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
    use image::{ImageBuffer, Rgb};

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
            (1600, 900, ImageFormat::Png),
            (300, 2000, ImageFormat::Jpeg),
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
        let bomb = huge_header_png();
        assert!(bomb.len() < 100);
        let started = std::time::Instant::now();
        assert!(matches!(
            decode_within(&bomb, MAX_SHOWN_SIDE, MAX_SHOWN_BYTES, Some(STORED_FORMATS)),
            Err(ImageError::Limits(_))
        ));
        assert!(matches!(
            decode_within(&bomb, MAX_CHOSEN_SIDE, MAX_CHOSEN_BYTES, None),
            Err(ImageError::Limits(_))
        ));
        assert!(decode(&bomb).is_none());
        assert!(prepare(&bomb).unwrap_err().contains("too large"));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn a_stored_picture_decodes() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let picture = decode(&encoded(SIDE, SIDE, format)).unwrap();
            assert_eq!(picture.size(0).width.0, SIDE as i32);
            assert_eq!(picture.size(0).height.0, SIDE as i32);
        }
        assert!(decode(&sample()).is_some());
    }

    #[test]
    fn a_large_picture_is_kept_at_the_prepared_size() {
        let picture = decode(&encoded(
            MAX_SHOWN_SIDE,
            MAX_SHOWN_SIDE / 2,
            ImageFormat::Png,
        ))
        .unwrap();
        assert_eq!(picture.size(0).width.0, SIDE as i32);
        assert_eq!(picture.size(0).height.0, SIDE as i32 / 2);
    }

    #[test]
    fn only_png_and_jpeg_are_decoded_from_others_but_any_picture_can_be_chosen() {
        for format in [
            ImageFormat::Bmp,
            ImageFormat::Gif,
            ImageFormat::Tiff,
            ImageFormat::Qoi,
        ] {
            let other = encoded(16, 16, format);
            assert!(
                image::load_from_memory(&other).is_ok(),
                "{format:?} can be decoded at all"
            );
            assert!(decode(&other).is_none(), "{format:?}");
            let prepared = prepare(&other).unwrap();
            assert!(decode(&prepared).is_some(), "{format:?} prepared");
        }
    }

    #[test]
    fn a_picture_wider_than_shown_pictures_is_refused() {
        assert!(decode(&encoded(MAX_SHOWN_SIDE + 1, 16, ImageFormat::Png)).is_none());
    }
}
