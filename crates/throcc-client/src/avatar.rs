use std::io::Cursor;
use std::sync::Arc;

use gpui::RenderImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use throcc_proto::messages::MAX_AVATAR_BYTES;

const SIDE: u32 = 128;
const JPEG_QUALITY: u8 = 85;

/// Turns any PNG or JPEG into a centred 128 px square that fits the server's size cap.
pub fn prepare(file: &[u8]) -> Result<Vec<u8>, String> {
    let picture = image::load_from_memory(file).map_err(|_| "That isn't a PNG or JPEG picture.")?;
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
        .encode_image(&DynamicImage::ImageRgb8(small.to_rgb8()))
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
pub fn decode(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut pixels = image::load_from_memory(bytes).ok()?.to_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(pixels)
    ])))
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
}
