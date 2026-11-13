#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{CFRetained, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVReturnSuccess,
};

use super::attributes;
use crate::frame::{VideoFrame, copy_nv12};

/// CoreVideo's `420f`: full-range NV12, as ScreenCaptureKit captures, VideoToolbox decodes
/// into here, and renderers draw.
pub const FULL_RANGE_NV12: u32 = u32::from_be_bytes(*b"420f");

/// A captured or decoded picture, full-range NV12 in GPU memory.
#[derive(Clone)]
pub struct PixelBuffer {
    pub buffer: CFRetained<CVPixelBuffer>,
    pub timestamp: u32,
}

// SAFETY: CoreVideo pixel buffers are reference counted thread-safely, and are only read
// here, under a lock of their base address.
unsafe impl Send for PixelBuffer {}
// SAFETY: as above; nothing reachable through `&PixelBuffer` writes to the buffer.
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    /// Takes a reference of its own to the `CVPixelBufferRef` at `raw`, as another wrapper
    /// hands it out.
    ///
    /// # Safety
    /// `raw` must be a live `CVPixelBufferRef` or null.
    pub unsafe fn retaining(raw: *mut c_void, timestamp: u32) -> Option<Self> {
        let raw = NonNull::new(raw.cast::<CVPixelBuffer>())?;
        // SAFETY: the caller promises a live pixel buffer.
        let buffer = unsafe { CFRetained::retain(raw) };
        Some(Self { buffer, timestamp })
    }

    /// A new full-range NV12 buffer in GPU memory that renderers can draw from.
    pub fn new(width: usize, height: usize, timestamp: u32) -> Option<Self> {
        let attributes = attributes::pixel_buffer(None);
        let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: `raw` receives a +1 reference on success.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                FULL_RANGE_NV12,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        if status != kCVReturnSuccess {
            return None;
        }
        // SAFETY: a successful create returned an owned reference.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        Some(Self { buffer, timestamp })
    }

    pub fn width(&self) -> usize {
        CVPixelBufferGetWidth(&self.buffer)
    }

    pub fn height(&self) -> usize {
        CVPixelBufferGetHeight(&self.buffer)
    }

    pub fn pixel_format(&self) -> u32 {
        CVPixelBufferGetPixelFormatType(&self.buffer)
    }

    /// The `CVPixelBufferRef`, valid while this lives.
    pub fn as_ptr(&self) -> *mut c_void {
        let buffer: &CVPixelBuffer = &self.buffer;
        (buffer as *const CVPixelBuffer).cast_mut().cast()
    }

    /// The buffer as a CoreFoundation object.
    pub fn as_cf(&self) -> &CFType {
        &self.buffer
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
        let Some(lock) = Lock::new(&self.buffer, CVPixelBufferLockFlags::ReadOnly) else {
            return false;
        };
        let (Some(y), Some(uv)) = (lock.plane(0), lock.plane(1)) else {
            return false;
        };
        copy_nv12(into, y, uv, (self.width(), self.height()));
        into.timestamp = self.timestamp;
        true
    }

    /// Writes `frame` into the buffer, which must be NV12 of the frame's size.
    pub fn write(&self, frame: &VideoFrame) -> bool {
        if (self.width(), self.height()) != (frame.width, frame.height) {
            return false;
        }
        let Some(lock) = Lock::new(&self.buffer, CVPixelBufferLockFlags::empty()) else {
            return false;
        };
        for (index, plane) in [&frame.y, &frame.uv].into_iter().enumerate() {
            let Some((base, stride)) = lock.plane_mut(index) else {
                return false;
            };
            let rows = CVPixelBufferGetHeightOfPlane(&self.buffer, index);
            for (row, source) in plane
                .chunks_exact(frame.width.max(1))
                .take(rows)
                .enumerate()
            {
                // SAFETY: the buffer is locked for writing, and each plane has `rows` rows of
                // `stride` bytes, each at least the frame's width.
                unsafe {
                    let destination = base.add(row * stride);
                    std::ptr::copy_nonoverlapping(source.as_ptr(), destination, frame.width);
                }
            }
        }
        true
    }

    /// A pixel buffer holding `frame`, as ScreenCaptureKit would deliver it.
    pub fn holding(frame: &VideoFrame) -> Option<Self> {
        let buffer = Self::new(frame.width, frame.height, frame.timestamp)?;
        buffer.write(frame).then_some(buffer)
    }
}

/// A pixel buffer's base address locked, unlocked on drop.
struct Lock<'a> {
    buffer: &'a CVPixelBuffer,
    flags: CVPixelBufferLockFlags,
}

impl<'a> Lock<'a> {
    fn new(buffer: &'a CVPixelBuffer, flags: CVPixelBufferLockFlags) -> Option<Self> {
        // SAFETY: `buffer` is live; the lock is undone in drop with the same flags.
        let status = unsafe { CVPixelBufferLockBaseAddress(buffer, flags) };
        (status == kCVReturnSuccess).then_some(Self { buffer, flags })
    }

    /// Plane `index`, with its stride.
    fn plane(&self, index: usize) -> Option<(&[u8], usize)> {
        let (base, stride) = self.plane_mut(index)?;
        let len = stride * CVPixelBufferGetHeightOfPlane(self.buffer, index);
        // SAFETY: the buffer stays locked while `self` lives, and plane `index` is
        // `height_of_plane` rows of `stride` bytes.
        Some((unsafe { std::slice::from_raw_parts(base, len) }, stride))
    }

    fn plane_mut(&self, index: usize) -> Option<(*mut u8, usize)> {
        let base = CVPixelBufferGetBaseAddressOfPlane(self.buffer, index).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(self.buffer, index);
        (!base.is_null()).then_some((base, stride))
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        // SAFETY: locked in `new` with these flags.
        unsafe { CVPixelBufferUnlockBaseAddress(self.buffer, self.flags) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_buffer_copies_out_unchanged() {
        let frame = VideoFrame::test_pattern(1280, 720, 7, 4500);
        let buffer = PixelBuffer::holding(&frame).unwrap();
        assert_eq!(buffer.pixel_format(), FULL_RANGE_NV12);
        assert_eq!(buffer.copied_to_memory(), frame);
    }
}
