use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use bytes::Bytes;
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoCodecType,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264,
    kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use throcc_proto::messages::{Codec, Recovery};
use tracing::{debug, info};

use super::attributes::{self, Dictionary, boolean, number};
use super::transfer::Fitter;
use super::{NO_ERR, PixelBuffer, TIMESCALE};
use crate::frame::VideoFrame;
use crate::nal;
#[cfg(target_os = "macos")]
use crate::software::vpx;
use crate::{EncodeError, EncodedUnit, EncoderConfig};

/// A GOP cap of this many seconds bounds how long a picture stays broken without a request.
const GOP_SECONDS: u32 = 4;

/// The encoders tried in order: hardware HEVC, then hardware H.264, then on macOS VP9 in
/// libvpx.
const LADDER: &[Codec] = &[Codec::Hevc, Codec::H264, Codec::Vp9];

/// A low-latency video encoder: no B-frames, no lookahead, constant bitrate, and an IDR on
/// request. Takes captured pixel buffers uncopied, and fits pictures of another size into
/// its own on the GPU, with black bars around them.
pub struct Encoder {
    backend: Backend,
    width: usize,
    height: usize,
    idr_next: bool,
    fitter: Option<Fitter>,
}

enum Backend {
    VideoToolbox(VideoToolbox),
    #[cfg(target_os = "macos")]
    Vpx(vpx::Encoder),
}

impl Encoder {
    /// Opens the best encoder that works here.
    pub fn open(config: EncoderConfig) -> Result<Self, EncodeError> {
        LADDER
            .iter()
            .find_map(|codec| match Self::open_with(*codec, config, true) {
                Ok(encoder) => Some(encoder),
                Err(e) => {
                    debug!(?codec, "unusable: {e}");
                    None
                }
            })
            .ok_or(EncodeError::NoEncoder)
    }

    /// Opens an encoder for `codec`, in hardware only if `hardware` holds.
    pub(crate) fn open_with(
        codec: Codec,
        config: EncoderConfig,
        hardware: bool,
    ) -> Result<Self, EncodeError> {
        let backend = match codec {
            Codec::H264 | Codec::Hevc => {
                Backend::VideoToolbox(VideoToolbox::open(codec, config, hardware)?)
            }
            #[cfg(target_os = "macos")]
            Codec::Vp9 => Backend::Vpx(vpx::Encoder::open(config)?),
            _ => return Err(EncodeError::NoEncoder),
        };
        let encoder = Self {
            backend,
            width: config.width,
            height: config.height,
            idr_next: true,
            fitter: None,
        };
        info!(encoder = encoder.name(), "video encoder ready");
        Ok(encoder)
    }

    pub fn codec(&self) -> Codec {
        match &self.backend {
            Backend::VideoToolbox(vt) => vt.codec,
            #[cfg(target_os = "macos")]
            Backend::Vpx(_) => Codec::Vp9,
        }
    }

    pub fn name(&self) -> &'static str {
        match self.codec() {
            Codec::Hevc => "hevc_videotoolbox",
            Codec::H264 => "h264_videotoolbox",
            _ => "libvpx-vp9",
        }
    }

    /// The hardware API or software library doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        match &self.backend {
            Backend::VideoToolbox(_) => "VideoToolbox",
            #[cfg(target_os = "macos")]
            Backend::Vpx(_) => "libvpx (software)",
        }
    }

    /// Every encoder here heals a broken picture with an IDR on request.
    pub fn recovery(&self) -> Recovery {
        Recovery::Idr
    }

    pub fn recover(&mut self, _kind: Recovery) {
        self.idr_next = true;
    }

    /// Whether [`Encoder::encode_pixel_buffer`] takes captured pixel buffers.
    pub fn takes_pixel_buffers(&self) -> bool {
        matches!(self.backend, Backend::VideoToolbox(_))
    }

    pub fn encode(&mut self, picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        let force = std::mem::take(&mut self.idr_next);
        match &mut self.backend {
            Backend::VideoToolbox(vt) => {
                let fitter = fitter(&mut self.fitter, (self.width, self.height))?;
                let buffer = fitter.fit_frame(picture).ok_or(EncodeError::Scaler)?;
                vt.encode(&buffer, force)
            }
            #[cfg(target_os = "macos")]
            Backend::Vpx(vpx) => {
                if force {
                    vpx.force_keyframe();
                }
                if (picture.width, picture.height) == (self.width, self.height) {
                    return vpx.encode(picture);
                }
                let fitter = fitter(&mut self.fitter, (self.width, self.height))?;
                let buffer = fitter.fit_frame(picture).ok_or(EncodeError::Scaler)?;
                vpx.encode(&buffer.copied_to_memory())
            }
        }
    }

    /// Encodes a captured picture by handing its pixel buffer to VideoToolbox: uncopied at
    /// the encoder's size, else scaled on the GPU first. Only VideoToolbox encoders take
    /// them.
    pub fn encode_pixel_buffer(
        &mut self,
        picture: &PixelBuffer,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        #[allow(
            clippy::infallible_destructuring_match,
            reason = "only macOS has a second backend"
        )]
        let vt = match &mut self.backend {
            Backend::VideoToolbox(vt) => vt,
            #[cfg(target_os = "macos")]
            Backend::Vpx(_) => return Err(EncodeError::NoGpuInput),
        };
        let force = std::mem::take(&mut self.idr_next);
        if (picture.width(), picture.height()) == (self.width, self.height) {
            return vt.encode(picture, force);
        }
        let fitter = fitter(&mut self.fitter, (self.width, self.height))?;
        let fitted = fitter.fit(picture).ok_or(EncodeError::Scaler)?;
        vt.encode(&fitted, force)
    }
}

fn fitter(slot: &mut Option<Fitter>, size: (usize, usize)) -> Result<&mut Fitter, EncodeError> {
    if slot.is_none() {
        *slot = Some(Fitter::new(size).ok_or(EncodeError::Scaler)?);
    }
    slot.as_mut().ok_or(EncodeError::Scaler)
}

/// What the output callback left: each encoded unit, or the status it failed with.
type Outputs = Mutex<Vec<Result<EncodedUnit, i32>>>;

/// A VideoToolbox compression session, flushed after every frame so each comes out before
/// the next goes in.
struct VideoToolbox {
    codec: Codec,
    session: CFRetained<VTCompressionSession>,
    /// Where the output callback leaves each unit. Boxed, so its address holds while the
    /// session points at it.
    outputs: Box<Outputs>,
    force_keyframe: Dictionary,
}

// SAFETY: the session is used through `&mut self` only, one thread at a time, and the
// outputs it writes are behind a mutex.
unsafe impl Send for VideoToolbox {}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        // SAFETY: invalidating stops the callbacks before the outputs box can go.
        unsafe { self.session.invalidate() };
    }
}

impl VideoToolbox {
    fn open(codec: Codec, config: EncoderConfig, hardware: bool) -> Result<Self, EncodeError> {
        let codec_type: CMVideoCodecType = match codec {
            Codec::Hevc => kCMVideoCodecType_HEVC,
            Codec::H264 => kCMVideoCodecType_H264,
            _ => return Err(EncodeError::NoEncoder),
        };
        // VideoToolbox's low-latency mode holds 2 to 4 frames when the height isn't a
        // multiple of 16, where its normal mode holds one.
        let low_latency = config.height.is_multiple_of(16);
        // SAFETY: VideoToolbox's keys are immutable statics.
        let specification = unsafe {
            attributes::dictionary(&[
                (
                    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                    boolean(hardware),
                ),
                (
                    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                    boolean(low_latency),
                ),
            ])
        };
        let source = attributes::pixel_buffer(Some((config.width, config.height)));
        let outputs: Box<Outputs> = Box::default();
        let mut raw: *mut VTCompressionSession = std::ptr::null_mut();
        let size = |side: usize| i32::try_from(side).map_err(|_| EncodeError::NoEncoder);
        // SAFETY: the callback's refcon is the boxed outputs, which outlive the session:
        // dropping `VideoToolbox` invalidates the session before the box goes.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                size(config.width)?,
                size(config.height)?,
                codec_type,
                Some(specification.as_opaque()),
                Some(source.as_opaque()),
                None,
                Some(encoded),
                (&*outputs as *const Outputs).cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        if status != NO_ERR {
            return Err(EncodeError::VideoToolbox(status));
        }
        let session = NonNull::new(raw).ok_or(EncodeError::VideoToolbox(status))?;
        // SAFETY: a successful create returned an owned reference.
        let session = unsafe { CFRetained::from_raw(session) };
        // SAFETY: the key and value are VideoToolbox's statics.
        let force_keyframe = unsafe {
            attributes::dictionary(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, boolean(true))])
        };
        let encoder = Self {
            codec,
            session,
            outputs,
            force_keyframe,
        };
        encoder.configure(config)?;
        Ok(encoder)
    }

    fn configure(&self, config: EncoderConfig) -> Result<(), EncodeError> {
        let fps = config.fps.max(1);
        let bitrate = i64::from(config.kbps) * 1000;
        let bitrate_number = number(bitrate);
        let fps_number = number(i64::from(fps));
        let gop = number(i64::from(fps * GOP_SECONDS));
        let gop_seconds = number(i64::from(GOP_SECONDS));
        // At most a second's worth of the bitrate in any one second.
        let byte_limit = number(bitrate / 8);
        let one_second = number(1);
        let limits = CFArray::<CFType>::from_objects(&[&*byte_limit, &*one_second]);
        // SAFETY: VideoToolbox's and CoreVideo's keys and values are immutable statics.
        let properties: Vec<(&CFString, &CFType)> = unsafe {
            vec![
                (kVTCompressionPropertyKey_RealTime, boolean(true)),
                (
                    kVTCompressionPropertyKey_AllowFrameReordering,
                    boolean(false),
                ),
                (
                    kVTCompressionPropertyKey_ProfileLevel,
                    match self.codec {
                        Codec::Hevc => kVTProfileLevel_HEVC_Main_AutoLevel,
                        _ => kVTProfileLevel_H264_High_AutoLevel,
                    },
                ),
                (kVTCompressionPropertyKey_AverageBitRate, &bitrate_number),
                (kVTCompressionPropertyKey_DataRateLimits, &limits),
                (kVTCompressionPropertyKey_ExpectedFrameRate, &fps_number),
                (kVTCompressionPropertyKey_MaxKeyFrameInterval, &gop),
                (
                    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                    &gop_seconds,
                ),
                (
                    kVTCompressionPropertyKey_ColorPrimaries,
                    kCVImageBufferColorPrimaries_ITU_R_709_2,
                ),
                (
                    kVTCompressionPropertyKey_TransferFunction,
                    kCVImageBufferTransferFunction_ITU_R_709_2,
                ),
                (
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                ),
            ]
        };
        for (key, value) in properties {
            // SAFETY: the session, key and value are live.
            let status = unsafe { VTSessionSetProperty(&self.session, key, Some(value)) };
            // Data rate limits and some profiles are refused by some encoders; the rest
            // must hold.
            if status != NO_ERR && key != unsafe { kVTCompressionPropertyKey_DataRateLimits } {
                return Err(EncodeError::VideoToolbox(status));
            }
        }
        Ok(())
    }

    /// Encodes `picture` and waits for what comes out.
    fn encode(
        &mut self,
        picture: &PixelBuffer,
        force: bool,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        let timestamp = picture.timestamp;
        // SAFETY: a time made from plain values.
        let presentation = unsafe { CMTime::new(i64::from(timestamp), TIMESCALE) };
        let properties = force.then(|| self.force_keyframe.as_opaque());
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: the session and buffer are live; the frame refcon carries the timestamp
        // as a plain number, never dereferenced.
        let status = unsafe {
            self.session.encode_frame(
                &picture.buffer,
                presentation,
                kCMTimeInvalid,
                properties,
                timestamp as usize as *mut c_void,
                &mut flags,
            )
        };
        if status != NO_ERR {
            return Err(EncodeError::VideoToolbox(status));
        }
        // SAFETY: as above.
        let status = unsafe { self.session.complete_frames(presentation) };
        if status != NO_ERR {
            return Err(EncodeError::VideoToolbox(status));
        }
        let outputs = std::mem::take(&mut *lock(&self.outputs));
        outputs
            .into_iter()
            .map(|output| output.map_err(EncodeError::VideoToolbox))
            .collect()
    }
}

fn lock(outputs: &Outputs) -> std::sync::MutexGuard<'_, Vec<Result<EncodedUnit, i32>>> {
    outputs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// VideoToolbox's output callback: converts each sample to Annex B, parameter sets first on
/// keyframes, for [`VideoToolbox::encode`].
unsafe extern "C-unwind" fn encoded(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the session's boxed outputs, alive while the session is.
    let outputs = unsafe { &*refcon.cast::<Outputs>() };
    let timestamp = frame_refcon as usize as u32;
    let output = if status != NO_ERR {
        Some(Err(status))
    } else {
        // SAFETY: a non-null sample is live for the callback.
        unsafe { sample.as_ref() }.and_then(|sample| unit(sample, timestamp).map(Ok))
    };
    if let Some(output) = output {
        lock(outputs).push(output);
    }
}

/// The access unit in `sample`, as Annex B.
fn unit(sample: &CMSampleBuffer, timestamp: u32) -> Option<EncodedUnit> {
    // SAFETY: the sample is live, and its block buffer holds `length` bytes.
    let data = unsafe {
        let block = sample.data_buffer()?;
        let length = block.data_length();
        let mut data = vec![0u8; length];
        let status = block.copy_data_bytes(0, length, NonNull::from(&mut data[..]).cast());
        if status != NO_ERR {
            return None;
        }
        data
    };
    // SAFETY: as above.
    let format = unsafe { sample.format_description() }?;
    let codec = match () {
        // SAFETY: reading the description's type.
        () if unsafe { format.media_sub_type() } == kCMVideoCodecType_H264 => Codec::H264,
        () => Codec::Hevc,
    };
    let keyframe = length_prefixed_keyframe(codec, &data);
    let sets = if keyframe {
        parameter_sets(codec, &format)
    } else {
        Vec::new()
    };
    let sets: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
    let annex_b = nal::to_annex_b(&sets, &data);
    Some(EncodedUnit {
        data: Bytes::from(annex_b),
        keyframe,
        timestamp,
    })
}

/// Whether length-prefixed NAL units hold a keyframe slice.
fn length_prefixed_keyframe(codec: Codec, data: &[u8]) -> bool {
    nal::is_keyframe(codec, &nal::to_annex_b(&[], data))
}

/// The parameter sets of an H.264 or HEVC format description, in order.
fn parameter_sets(codec: Codec, format: &CMFormatDescription) -> Vec<Vec<u8>> {
    let mut sets = Vec::new();
    let mut count = 1;
    let mut index = 0;
    while index < count {
        let mut pointer: *const u8 = std::ptr::null();
        let mut size = 0usize;
        // SAFETY: the description is live, and the outputs are locals.
        let status = unsafe {
            match codec {
                Codec::H264 => CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format,
                    index,
                    &mut pointer,
                    &mut size,
                    &mut count,
                    std::ptr::null_mut(),
                ),
                _ => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &mut pointer,
                    &mut size,
                    &mut count,
                    std::ptr::null_mut(),
                ),
            }
        };
        if status != NO_ERR || pointer.is_null() {
            break;
        }
        // SAFETY: the description holds `size` bytes at `pointer` while it lives.
        sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
        index += 1;
    }
    sets
}
