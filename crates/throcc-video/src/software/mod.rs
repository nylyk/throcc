//! Software codecs for platforms whose own codecs lack one: dav1d (as rav1d) for AV1 and
//! libvpx for VP9. Both are BSD-licensed and link statically.

pub mod dav1d;
pub mod vpx;

use crate::frame::VideoFrame;

/// One plane of a 4:2:0 picture: where it starts and how far apart its rows are.
#[derive(Clone, Copy)]
pub(crate) struct Plane {
    pub data: *const u8,
    pub stride: usize,
}

/// Copies an 8-bit planar 4:2:0 picture of `width`×`height`, cropped to even sides, into
/// NV12, reusing the planes of `spare` if given.
///
/// # Safety
/// Each plane must hold its rows of the picture at its stride: `height` rows of `width`
/// bytes for luma, half as many and half as wide for each chroma plane.
pub(crate) unsafe fn i420_to_nv12(
    [y_plane, u_plane, v_plane]: [Plane; 3],
    (width, height): (usize, usize),
    timestamp: u32,
    spare: Option<VideoFrame>,
) -> VideoFrame {
    let (width, height) = (width & !1, height & !1);
    let (mut y, mut uv) = spare.map_or_else(Default::default, |spare| (spare.y, spare.uv));
    y.clear();
    uv.clear();
    y.reserve(width * height);
    uv.reserve(width * height / 2);
    // SAFETY: the caller promises every row read lies within its plane.
    unsafe {
        for row in 0..height {
            let line = std::slice::from_raw_parts(y_plane.data.add(row * y_plane.stride), width);
            y.extend_from_slice(line);
        }
        for row in 0..height / 2 {
            let u = std::slice::from_raw_parts(u_plane.data.add(row * u_plane.stride), width / 2);
            let v = std::slice::from_raw_parts(v_plane.data.add(row * v_plane.stride), width / 2);
            uv.extend(u.iter().zip(v).flat_map(|(u, v)| [*u, *v]));
        }
    }
    VideoFrame {
        width,
        height,
        y,
        uv,
        timestamp,
    }
}

/// An NV12 picture's planes split into I420's three, tightly packed. An odd-width picture
/// lacks the V of its last chroma column; it stays neutral.
pub(crate) fn nv12_to_i420(picture: &VideoFrame, into: &mut Vec<u8>) {
    let (width, height) = (picture.width, picture.height);
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    into.clear();
    into.resize(width * height + 2 * chroma_width * chroma_height, 128);
    let (luma, chroma) = into.split_at_mut(width * height);
    for (to, from) in luma.iter_mut().zip(&picture.y) {
        *to = *from;
    }
    let (u, v) = chroma.split_at_mut(chroma_width * chroma_height);
    let rows = picture.uv.chunks(width.max(1)).take(chroma_height);
    for ((line, u_row), v_row) in rows
        .zip(u.chunks_exact_mut(chroma_width.max(1)))
        .zip(v.chunks_exact_mut(chroma_width.max(1)))
    {
        for ((pair, u), v) in line.chunks(2).zip(u_row).zip(v_row) {
            *u = pair[0];
            if let Some(value) = pair.get(1) {
                *v = *value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_splits_into_i420_and_back() {
        for (width, height) in [(4, 2), (6, 4), (5, 3)] {
            let mut picture = VideoFrame::test_pattern(width, height, 0, 9);
            for (i, sample) in picture.uv.iter_mut().enumerate() {
                *sample = i as u8;
            }
            let mut planar = Vec::new();
            nv12_to_i420(&picture, &mut planar);
            let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
            assert_eq!(planar.len(), width * height + 2 * cw * ch);
            let luma = width * height;
            let plane = |offset: usize, stride: usize| Plane {
                data: planar[offset..].as_ptr(),
                stride,
            };
            let planes = [plane(0, width), plane(luma, cw), plane(luma + cw * ch, cw)];
            // SAFETY: `planar` holds the three planes at those strides.
            let back = unsafe { i420_to_nv12(planes, (width, height), 9, None) };
            let (even_width, even_height) = (width & !1, height & !1);
            assert_eq!((back.width, back.height), (even_width, even_height));
            for row in 0..even_height {
                assert_eq!(
                    back.y[row * even_width..(row + 1) * even_width],
                    picture.y[row * width..row * width + even_width]
                );
            }
            for row in 0..even_height / 2 {
                assert_eq!(
                    back.uv[row * even_width..(row + 1) * even_width],
                    picture.uv[row * width..row * width + even_width]
                );
            }
        }
    }
}
