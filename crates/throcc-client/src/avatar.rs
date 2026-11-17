use std::io::Cursor;
use std::sync::Arc;

use gpui::RenderImage;
use image::{DynamicImage, ImageFormat};
pub use throcc_client_core::picture::prepare;

/// The side every profile picture is prepared at.
const SIDE: u32 = 128;

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

/// Decodes a stored picture for gpui, which takes pixels in BGRA order, as
/// [`throcc_client_core::picture::decode`] reads it.
pub fn decode(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut pixels = throcc_client_core::picture::decode(bytes)?;
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(pixels)
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_decodes_at_its_size() {
        let picture = decode(&sample()).unwrap();
        assert_eq!(picture.size(0).width.0, SIDE as i32);
        assert_eq!(picture.size(0).height.0, SIDE as i32);
    }
}
