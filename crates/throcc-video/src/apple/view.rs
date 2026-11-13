//! iOS: decoded pixel buffers go straight into the `AVSampleBufferDisplayLayer` the app
//! hands over, uncopied, each shown as soon as it is enqueued.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_av_foundation::{
    AVQueuedSampleBufferRendering, AVQueuedSampleBufferRenderingStatus, AVSampleBufferDisplayLayer,
    AVSampleBufferVideoRenderer,
};
use objc2_core_foundation::{CFDictionary, CFMutableDictionary, CFRetained};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateForImageBuffer, kCMSampleAttachmentKey_DisplayImmediately,
    kCMTimeInvalid,
};

use super::{NO_ERR, PixelBuffer, TIMESCALE, attributes};

/// A display layer the app handed over, and the renderer that takes pictures for it from any
/// thread. Clones share the layer.
#[derive(Clone)]
pub struct View {
    renderer: Retained<AVSampleBufferVideoRenderer>,
}

// SAFETY: Apple documents the sample buffer renderer as safe to enqueue to from a background
// thread, and the layer itself is only touched to get it.
unsafe impl Send for View {}
// SAFETY: as above.
unsafe impl Sync for View {}

impl View {
    /// Takes over the reference to the `AVSampleBufferDisplayLayer` at `raw`, as Swift's
    /// `Unmanaged.passRetained` hands one out.
    ///
    /// # Safety
    /// `raw` must be a live `AVSampleBufferDisplayLayer*` whose reference the caller gives up,
    /// or null.
    pub unsafe fn from_raw(raw: u64) -> Option<Self> {
        let layer = raw as usize as *mut AVSampleBufferDisplayLayer;
        // SAFETY: the caller hands over a +1 reference to a live layer.
        let layer = unsafe { Retained::from_raw(layer) }?;
        // SAFETY: the layer is live; its renderer may be used from any thread.
        let renderer = unsafe { layer.sampleBufferRenderer() };
        Some(Self { renderer })
    }

    /// Shows `picture` as soon as it can, in place of what the layer shows.
    pub(crate) fn show(&self, picture: &PixelBuffer) -> bool {
        // SAFETY: the renderer is live; a failed one, as after the app was in the
        // background, takes pictures again once flushed.
        unsafe {
            if self.renderer.status() == AVQueuedSampleBufferRenderingStatus::Failed
                || self.renderer.requiresFlushToResumeDecoding()
            {
                self.renderer.flush();
            }
        }
        let Some(sample) = shown_at_once(picture) else {
            return false;
        };
        // SAFETY: the renderer and sample are live.
        unsafe { self.renderer.enqueueSampleBuffer(&sample) };
        true
    }
}

/// A sample buffer holding `picture`, marked to be shown as soon as it is enqueued.
fn shown_at_once(picture: &PixelBuffer) -> Option<CFRetained<CMSampleBuffer>> {
    let mut format: *const CMFormatDescription = std::ptr::null();
    // SAFETY: the pixel buffer is live; a description of it comes back +1.
    let status = unsafe {
        CMVideoFormatDescriptionCreateForImageBuffer(
            None,
            &picture.buffer,
            NonNull::from(&mut format),
        )
    };
    if status != NO_ERR {
        return None;
    }
    // SAFETY: a successful create returned an owned reference.
    let format = unsafe { CFRetained::from_raw(NonNull::new(format.cast_mut())?) };
    // SAFETY: reading CoreMedia's immutable statics, and making a time from plain values.
    let timing = unsafe {
        CMSampleTimingInfo {
            duration: kCMTimeInvalid,
            presentationTimeStamp: CMTime::new(i64::from(picture.timestamp), TIMESCALE),
            decodeTimeStamp: kCMTimeInvalid,
        }
    };
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: the buffer, description and timing are live for the call.
    let status = unsafe {
        CMSampleBuffer::create_ready_with_image_buffer(
            None,
            &picture.buffer,
            &format,
            NonNull::from(&timing),
            NonNull::from(&mut sample),
        )
    };
    if status != NO_ERR {
        return None;
    }
    // SAFETY: a successful create returned an owned reference.
    let sample = unsafe { CFRetained::from_raw(NonNull::new(sample)?) };
    // SAFETY: a sample buffer with one sample has one attachments dictionary, mutable, made
    // here if it isn't there yet.
    unsafe {
        let attachments = sample.sample_attachments_array(true)?;
        let first = attachments
            .value_at_index(0)
            .cast::<CFDictionary>()
            .cast_mut();
        let first = NonNull::new(first.cast::<CFMutableDictionary>())?;
        CFMutableDictionary::set_value(
            Some(first.as_ref()),
            kCMSampleAttachmentKey_DisplayImmediately as *const _ as *const c_void,
            attributes::boolean(true) as *const _ as *const c_void,
        );
    }
    Some(sample)
}
