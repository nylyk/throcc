//! Pools of pixel buffers, scaling pictures into another size on the GPU with black bars
//! around them, and turning them upright.

use std::ptr::NonNull;

use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool, kCVReturnSuccess};
use objc2_video_toolbox::{
    VTPixelRotationSession, VTPixelTransferSession, VTSessionSetProperty,
    kVTPixelRotationPropertyKey_Rotation, kVTPixelTransferPropertyKey_ScalingMode, kVTRotation_0,
    kVTRotation_180, kVTRotation_CCW90, kVTRotation_CW90, kVTScalingMode_Letterbox,
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

/// How a captured picture is turned from upright, from the orientation a capture tags it
/// with. Mirrored orientations count as their turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turn {
    #[default]
    Upright,
    /// Shown upright once turned a quarter clockwise.
    Clockwise,
    HalfTurn,
    /// Shown upright once turned a quarter counterclockwise.
    Counterclockwise,
}

impl Turn {
    /// The turn of a picture ScreenCaptureKit tagged with `orientation`.
    pub fn from_orientation(orientation: i32) -> Self {
        // ScreenCaptureKit on iOS 27 tags the quarter turns the other way round from what
        // CGImagePropertyOrientation defines: a picture tagged 6 is upright once turned
        // counterclockwise.
        match orientation {
            3 | 4 => Self::HalfTurn,
            5 | 6 => Self::Counterclockwise,
            7 | 8 => Self::Clockwise,
            _ => Self::Upright,
        }
    }

    /// Whether the picture lies on its side, so upright it is as wide as it is tall now.
    pub fn sideways(self) -> bool {
        matches!(self, Self::Clockwise | Self::Counterclockwise)
    }

    /// The size of a `width`×`height` picture once upright.
    pub fn upright_size(self, (width, height): (usize, usize)) -> (usize, usize) {
        if self.sideways() {
            (height, width)
        } else {
            (width, height)
        }
    }
}

/// Turns pictures upright on the GPU.
pub struct Turner {
    session: CFRetained<VTPixelRotationSession>,
    /// The turn the session is set to.
    turn: Turn,
    /// Buffers of the size of the last upright picture.
    target: Option<Pool>,
}

// SAFETY: the rotation session is used through `&mut self` only, one thread at a time.
unsafe impl Send for Turner {}

impl Turner {
    pub fn new() -> Option<Self> {
        let mut raw: *mut VTPixelRotationSession = std::ptr::null_mut();
        // SAFETY: `raw` receives a +1 reference on success.
        let status = unsafe { VTPixelRotationSession::create(None, NonNull::from(&mut raw)) };
        if status != NO_ERR {
            return None;
        }
        // SAFETY: a successful create returned an owned reference.
        let session = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        Some(Self {
            session,
            turn: Turn::Upright,
            target: None,
        })
    }

    /// `source` turned upright from `turn`, or `None` if the GPU can't turn it.
    pub fn upright(&mut self, source: &PixelBuffer, turn: Turn) -> Option<PixelBuffer> {
        if turn != self.turn {
            // SAFETY: the keys and values are VideoToolbox's immutable statics.
            let rotation = unsafe {
                match turn {
                    Turn::Upright => kVTRotation_0,
                    Turn::Clockwise => kVTRotation_CW90,
                    Turn::HalfTurn => kVTRotation_180,
                    Turn::Counterclockwise => kVTRotation_CCW90,
                }
            };
            // SAFETY: the session is live, and the key and value are VideoToolbox's statics.
            let status = unsafe {
                VTSessionSetProperty(
                    &self.session,
                    kVTPixelRotationPropertyKey_Rotation,
                    Some(rotation),
                )
            };
            if status != NO_ERR {
                return None;
            }
            self.turn = turn;
        }
        let size = turn.upright_size((source.width(), source.height()));
        let target = match &mut self.target {
            Some(pool) if pool.size() == size => pool,
            slot => slot.insert(Pool::new(size)?),
        };
        let turned = target.take(source.timestamp)?;
        // SAFETY: both buffers are live pixel buffers, the target of the upright size.
        let status = unsafe { self.session.rotate_image(&source.buffer, &turned.buffer) };
        (status == NO_ERR).then_some(turned)
    }
}
