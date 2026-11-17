//! Pools of pixel buffers, and scaling pictures into another size on the GPU with black
//! bars around them.

use std::ptr::NonNull;

use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool, kCVReturnSuccess};
use objc2_video_toolbox::{
    VTPixelTransferSession, VTSessionSetProperty, kVTPixelTransferPropertyKey_ScalingMode,
    kVTScalingMode_Letterbox,
};

use super::{NO_ERR, PixelBuffer, attributes};
use crate::frame::VideoFrame;

/// Full-range NV12 pixel buffers of one size, reused once their users let go of them.
pub struct Pool {
    pool: CFRetained<CVPixelBufferPool>,
    size: (usize, usize),
}

// SAFETY: CoreVideo pools are thread-safe.
unsafe impl Send for Pool {}

impl Pool {
    pub fn new(size: (usize, usize)) -> Option<Self> {
        let attributes = attributes::pixel_buffer(Some(size));
        let mut raw: *mut CVPixelBufferPool = std::ptr::null_mut();
        // SAFETY: `raw` receives a +1 reference on success.
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                None,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        if status != kCVReturnSuccess {
            return None;
        }
        // SAFETY: a successful create returned an owned reference.
        let pool = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        Some(Self { pool, size })
    }

    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    /// A free buffer from the pool, stamped `timestamp`.
    pub fn take(&self, timestamp: u32) -> Option<PixelBuffer> {
        take_from(&self.pool, timestamp)
    }

    /// A buffer from the pool holding `frame`, which must be of the pool's size.
    pub fn upload(&self, frame: &VideoFrame) -> Option<PixelBuffer> {
        let mut buffer = self.take(frame.timestamp)?;
        buffer.write(frame).then_some(buffer)
    }
}

/// A free buffer from `pool`, stamped `timestamp`.
pub fn take_from(pool: &CVPixelBufferPool, timestamp: u32) -> Option<PixelBuffer> {
    let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: `raw` receives a +1 reference on success.
    let status =
        unsafe { CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut raw)) };
    if status != kCVReturnSuccess {
        return None;
    }
    // SAFETY: a successful create returned an owned reference.
    let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
    Some(PixelBuffer { buffer, timestamp })
}

/// Scales pictures of any size into one size on the GPU, keeping their aspect ratio, with
/// black bars around them.
pub struct Fitter {
    session: CFRetained<VTPixelTransferSession>,
    target: Pool,
    /// Buffers pictures in memory are uploaded into before they are scaled, of the size of
    /// the last one.
    uploads: Option<Pool>,
}

// SAFETY: the transfer session is used through `&mut self` only, one thread at a time.
unsafe impl Send for Fitter {}

impl Fitter {
    pub fn new(size: (usize, usize)) -> Option<Self> {
        let target = Pool::new(size)?;
        let mut raw: *mut VTPixelTransferSession = std::ptr::null_mut();
        // SAFETY: `raw` receives a +1 reference on success.
        let status = unsafe { VTPixelTransferSession::create(None, NonNull::from(&mut raw)) };
        if status != NO_ERR {
            return None;
        }
        // SAFETY: a successful create returned an owned reference.
        let session = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        // SAFETY: the key and value are VideoToolbox's immutable statics.
        let status = unsafe {
            VTSessionSetProperty(
                &session,
                kVTPixelTransferPropertyKey_ScalingMode,
                Some(kVTScalingMode_Letterbox),
            )
        };
        if status != NO_ERR {
            return None;
        }
        Some(Self {
            session,
            target,
            uploads: None,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        self.target.size()
    }

    /// `source` scaled into the fitter's size.
    pub fn fit(&mut self, source: &PixelBuffer) -> Option<PixelBuffer> {
        let fitted = self.target.take(source.timestamp)?;
        // SAFETY: both buffers are live pixel buffers.
        let status = unsafe { self.session.transfer_image(&source.buffer, &fitted.buffer) };
        (status == NO_ERR).then_some(fitted)
    }

    /// `frame`, in memory, in a pixel buffer of the fitter's size: uploaded as it is, or
    /// scaled on the GPU when it is of another size.
    pub fn fit_frame(&mut self, frame: &VideoFrame) -> Option<PixelBuffer> {
        let size = (frame.width, frame.height);
        if size == self.size() {
            return self.target.upload(frame);
        }
        let uploads = match &mut self.uploads {
            Some(pool) if pool.size() == size => pool,
            slot => slot.insert(Pool::new(size)?),
        };
        let uploaded = uploads.upload(frame)?;
        self.fit(&uploaded)
    }
}
