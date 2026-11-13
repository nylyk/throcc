#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use screencapturekit::cv::{CVPixelBuffer, CVPixelBufferLockGuard};

use crate::frame::{VideoFrame, copy_nv12};

/// CoreVideo's `420f`: full-range NV12, as ScreenCaptureKit captures and renderers draw.
pub const FULL_RANGE_NV12: u32 = u32::from_be_bytes(*b"420f");

/// A captured or decoded picture, full-range NV12 in GPU memory.
#[derive(Clone)]
pub struct PixelBuffer {
    pub buffer: CVPixelBuffer,
    pub timestamp: u32,
}

impl PixelBuffer {
    pub fn width(&self) -> usize {
        self.buffer.width()
    }

    pub fn height(&self) -> usize {
        self.buffer.height()
    }

    /// The `CVPixelBufferRef`, valid while this lives.
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.buffer.as_ptr()
    }

    /// The picture copied into memory.
    pub fn copied_to_memory(&self) -> VideoFrame {
        let mut out = VideoFrame::blank(self.width(), self.height());
        self.copy_into(&mut out);
        out
    }

    /// Copies the picture into `into`, resizing it to fit. False if the buffer couldn't be
    /// read.
    pub fn copy_into(&self, into: &mut VideoFrame) -> bool {
        let Ok(guard) = self.buffer.lock_read_only() else {
            return false;
        };
        let (Some(y), Some(uv)) = (plane(&guard, 0), plane(&guard, 1)) else {
            return false;
        };
        copy_nv12(into, y, uv, (guard.width(), guard.height()));
        into.timestamp = self.timestamp;
        true
    }

    /// A pixel buffer holding `frame`, as ScreenCaptureKit would deliver it.
    pub fn holding(frame: &VideoFrame) -> Option<Self> {
        let buffer = CVPixelBuffer::create(frame.width, frame.height, FULL_RANGE_NV12).ok()?;
        {
            let mut guard = buffer.lock_read_write().ok()?;
            for (index, plane) in [&frame.y, &frame.uv].into_iter().enumerate() {
                let stride = guard.bytes_per_row_of_plane(index);
                let base = guard.base_address_of_plane_mut(index)?;
                for (row, source) in plane.chunks_exact(frame.width).enumerate() {
                    // SAFETY: the buffer is locked for writing, and each plane has as many
                    // rows of `stride` bytes as `frame` has of `width`.
                    unsafe {
                        let destination = base.add(row * stride);
                        std::ptr::copy_nonoverlapping(source.as_ptr(), destination, frame.width);
                    }
                }
            }
        }
        Some(Self {
            buffer,
            timestamp: frame.timestamp,
        })
    }
}

/// Plane `index` of a locked pixel buffer, with its stride.
fn plane<'a>(guard: &'a CVPixelBufferLockGuard<'_>, index: usize) -> Option<(&'a [u8], usize)> {
    let base = guard.base_address_of_plane(index)?;
    let stride = guard.bytes_per_row_of_plane(index);
    let len = stride * guard.height_of_plane(index);
    // SAFETY: the buffer stays locked while `guard` is borrowed, and plane `index` is
    // `height_of_plane` rows of `stride` bytes.
    Some((unsafe { std::slice::from_raw_parts(base, len) }, stride))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_buffer_copies_out_unchanged() {
        let frame = VideoFrame::test_pattern(1280, 720, 7, 4500);
        let buffer = PixelBuffer::holding(&frame).unwrap();
        assert_eq!(buffer.copied_to_memory(), frame);
    }
}
