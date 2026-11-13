use std::ffi::{c_int, c_void};
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2_core_foundation::{CFRetained, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMBlockBufferFlags, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo,
    CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, CMVideoFormatDescriptionGetDimensions,
    kCMBlockBufferAssureMemoryNowFlag, kCMTimeInvalid,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTSessionSetProperty, kVTDecompressionPropertyKey_RealTime,
};
use throcc_proto::messages::Codec;
use tracing::{info, warn};

use super::transfer::Pool;
use super::{NO_ERR, PixelBuffer, TIMESCALE, attributes};
use crate::frame::VideoFrame;
use crate::nal::{self, LENGTH_SIZE};
use crate::software::{dav1d, vpx};
use crate::{DecodeError, Frame, MaxSize, Output};

/// VideoToolbox's `kVTInvalidSessionErr`: the session is gone, as after iOS took the
/// hardware from a backgrounded app, and has to be made again.
const INVALID_SESSION: i32 = -12903;

/// Decodes one remote share into pixel buffers: H.264 and HEVC in VideoToolbox, which falls
/// back to Apple's own software decoders where the hardware lacks them, AV1 in dav1d and
/// VP9 in libvpx. Shows the newest frame; presentation time is ignored.
pub struct Decoder {
    inner: Inner,
    max_size: MaxSize,
    /// The view iOS shows pictures in.
    #[cfg_attr(
        not(target_os = "ios"),
        allow(dead_code, reason = "macOS draws them itself")
    )]
    output: Output,
    /// Whether reference pictures were dropped since [`Decoder::take_flushed`] last ran.
    flushed: bool,
}

enum Inner {
    VideoToolbox(VideoToolbox),
    Software {
        decoder: Software,
        /// Buffers software pictures are uploaded into, of the size of the last one.
        pool: Option<Pool>,
        /// A spent picture in memory, whose planes the next one is written into.
        spare: Option<VideoFrame>,
    },
}

enum Software {
    Dav1d(dav1d::Decoder),
    Vpx(vpx::Decoder),
}

impl Decoder {
    /// Opens a decoder that refuses pictures larger than `max_size`. On iOS it shows its
    /// pictures in the view `output` holds.
    pub fn new(codec: Codec, max_size: MaxSize, output: Output) -> Result<Self, DecodeError> {
        let software = |decoder| Inner::Software {
            decoder,
            pool: None,
            spare: None,
        };
        let inner = match codec {
            Codec::H264 | Codec::Hevc => Inner::VideoToolbox(VideoToolbox::new(codec)),
            Codec::Av1 => software(Software::Dav1d(dav1d::Decoder::new(max_size)?)),
            Codec::Vp9 => software(Software::Vpx(vpx::Decoder::new(max_size)?)),
        };
        let decoder = Self {
            inner,
            max_size,
            output,
            flushed: false,
        };
        info!("decoding {codec:?} in {}", decoder.engine());
        Ok(decoder)
    }

    /// Hands back a spent picture, so the next one reuses its memory.
    pub fn recycle(&mut self, frame: VideoFrame) {
        if let Inner::Software { spare, .. } = &mut self.inner {
            *spare = Some(frame);
        }
    }

    /// Whether the decoder dropped its reference pictures since the last call, so only a
    /// keyframe decodes cleanly again.
    pub fn take_flushed(&mut self) -> bool {
        std::mem::take(&mut self.flushed)
    }

    /// The hardware API or software decoder doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        match &self.inner {
            Inner::VideoToolbox(_) => "VideoToolbox",
            Inner::Software {
                decoder: Software::Dav1d(_),
                ..
            } => "dav1d (software)",
            Inner::Software {
                decoder: Software::Vpx(_),
                ..
            } => "libvpx (software)",
        }
    }

    /// Decodes one access unit into its newest picture, if any.
    pub fn decode(&mut self, unit: &[u8], timestamp: u32) -> Result<Option<Frame>, DecodeError> {
        match &mut self.inner {
            Inner::VideoToolbox(decoder) => {
                let decoded = decoder.decode(unit, timestamp, self.max_size);
                if decoder.take_lost() {
                    self.flushed = true;
                }
                Ok(decoded?.map(Frame::PixelBuffer))
            }
            Inner::Software {
                decoder,
                pool,
                spare,
            } => {
                let decoded = match decoder {
                    Software::Dav1d(d) => d.decode(unit, timestamp, spare.take())?,
                    Software::Vpx(d) => d.decode(unit, timestamp, spare.take())?,
                };
                let Some(frame) = decoded else {
                    return Ok(None);
                };
                Ok(Some(upload(pool, frame, spare)))
            }
        }
    }

    /// Shows `frame`, the newest picture due, in iOS's view. macOS draws pictures itself.
    pub fn present(&mut self, frame: &Frame) {
        #[cfg(target_os = "ios")]
        if let (Frame::PixelBuffer(picture), Some(view)) = (frame, self.output.view()) {
            view.show(picture);
        }
        #[cfg(not(target_os = "ios"))]
        let _ = frame;
    }
}

/// `frame` in a pixel buffer from `pool`, made for its size; in memory if that fails. Its
/// planes are kept in `spare` for the next picture.
fn upload(pool: &mut Option<Pool>, frame: VideoFrame, spare: &mut Option<VideoFrame>) -> Frame {
    let size = (frame.width, frame.height);
    if pool.as_ref().is_none_or(|pool| pool.size() != size) {
        *pool = Pool::new(size);
    }
    match pool.as_ref().and_then(|pool| pool.upload(&frame)) {
        Some(buffer) => {
            *spare = Some(frame);
            Frame::PixelBuffer(buffer)
        }
        None => Frame::Planes(frame),
    }
}

/// What the output callback of a decode left.
#[derive(Default)]
struct Decoded {
    status: i32,
    picture: Option<CFRetained<CVImageBuffer>>,
}

/// H.264 or HEVC in VideoToolbox. The session is made from the first parameter sets and
/// made again whenever ones it can't take arrive.
struct VideoToolbox {
    codec: Codec,
    session: Option<Session>,
    /// The parameter sets the session's format description was made from.
    parameter_sets: Vec<Vec<u8>>,
    /// Where the output callback leaves each picture. Boxed, so its address holds while the
    /// session points at it.
    decoded: Box<Mutex<Decoded>>,
    /// Whether the session was lost since the last [`VideoToolbox::take_lost`].
    lost: bool,
}

// SAFETY: the session is used through `&mut self` only, one thread at a time, and the
// output it writes is behind a mutex.
unsafe impl Send for VideoToolbox {}

struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: invalidating stops the callbacks before the output box can go.
        unsafe { self.session.invalidate() };
    }
}

impl VideoToolbox {
    fn new(codec: Codec) -> Self {
        Self {
            codec,
            session: None,
            parameter_sets: Vec::new(),
            decoded: Box::default(),
            lost: false,
        }
    }

    fn take_lost(&mut self) -> bool {
        std::mem::take(&mut self.lost)
    }

    fn decode(
        &mut self,
        unit: &[u8],
        timestamp: u32,
        max_size: MaxSize,
    ) -> Result<Option<PixelBuffer>, DecodeError> {
        let split = nal::split(self.codec, unit);
        if !split.parameter_sets.is_empty() && split.parameter_sets != self.parameter_sets {
            self.start(&split.parameter_sets, max_size)?;
        }
        let Some(session) = &self.session else {
            return Err(DecodeError::NoKeyframe);
        };
        if split.length_prefixed.is_empty() {
            return Ok(None);
        }
        let sample = sample_buffer(&split.length_prefixed, &session.format, timestamp)?;
        *lock(&self.decoded) = Decoded::default();
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: the session and sample are live, and decoding synchronously calls the
        // output callback before it returns.
        let status = unsafe {
            session.session.decode_frame(
                &sample,
                VTDecodeFrameFlags::empty(),
                std::ptr::null_mut(),
                &mut info,
            )
        };
        if status == INVALID_SESSION {
            warn!("the VideoToolbox session was lost; waiting for a keyframe");
            self.session = None;
            self.parameter_sets.clear();
            self.lost = true;
        }
        if status != NO_ERR {
            return Err(DecodeError::VideoToolbox(status));
        }
        let output = std::mem::take(&mut *lock(&self.decoded));
        if output.status != NO_ERR {
            return Err(DecodeError::VideoToolbox(output.status));
        }
        let Some(picture) = output.picture else {
            return Ok(None);
        };
        let picture = PixelBuffer {
            buffer: picture,
            timestamp,
        };
        let (width, height) = (picture.width(), picture.height());
        if !max_size.fits(width as i64, height as i64) {
            return Err(DecodeError::TooLarge(width as i32, height as i32));
        }
        Ok(Some(picture))
    }

    /// Makes a format description from `parameter_sets`, and a session for it unless the
    /// running one takes it.
    fn start(&mut self, parameter_sets: &[&[u8]], max_size: MaxSize) -> Result<(), DecodeError> {
        let format = format_description(self.codec, parameter_sets)?;
        // SAFETY: a description made from H.264 or HEVC parameter sets is a video one.
        let size = unsafe { CMVideoFormatDescriptionGetDimensions(&format) };
        if !max_size.fits(size.width.into(), size.height.into()) {
            return Err(DecodeError::TooLarge(size.width, size.height));
        }
        let keeps = self.session.as_ref().is_some_and(|running| {
            // SAFETY: both are live.
            unsafe { running.session.can_accept_format_description(&format) }
        });
        self.parameter_sets = parameter_sets.iter().map(|set| set.to_vec()).collect();
        if let Some(running) = &mut self.session
            && keeps
        {
            running.format = format;
            return Ok(());
        }
        self.session = None;
        self.session = Some(self.open(format)?);
        Ok(())
    }

    fn open(&self, format: CFRetained<CMFormatDescription>) -> Result<Session, DecodeError> {
        let destination = attributes::pixel_buffer(None);
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decoded),
            decompressionOutputRefCon: (&*self.decoded as *const Mutex<Decoded>).cast_mut().cast(),
        };
        let mut raw: *mut VTDecompressionSession = std::ptr::null_mut();
        // SAFETY: the callback's refcon is the boxed output, which outlives the session:
        // `Session` invalidates the session when dropped, before the box can go.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(destination.as_opaque()),
                &callback,
                NonNull::from(&mut raw),
            )
        };
        if status != NO_ERR {
            return Err(DecodeError::VideoToolbox(status));
        }
        let session = NonNull::new(raw).ok_or(DecodeError::VideoToolbox(status))?;
        // SAFETY: a successful create returned an owned reference.
        let session = unsafe { CFRetained::from_raw(session) };
        // SAFETY: the key and value are VideoToolbox's and CoreFoundation's statics.
        unsafe {
            let real_time: &CFType = attributes::boolean(true);
            VTSessionSetProperty(
                &session,
                kVTDecompressionPropertyKey_RealTime,
                Some(real_time),
            );
        }
        Ok(Session { session, format })
    }
}

fn lock(output: &Mutex<Decoded>) -> std::sync::MutexGuard<'_, Decoded> {
    output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// VideoToolbox's output callback: keeps the picture for [`VideoToolbox::decode`].
unsafe extern "C-unwind" fn decoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    picture: *mut CVImageBuffer,
    _presentation: CMTime,
    _duration: CMTime,
) {
    // SAFETY: the refcon is the session's boxed output, alive while the session is.
    let output = unsafe { &*refcon.cast::<Mutex<Decoded>>() };
    let mut output = lock(output);
    output.status = status;
    // SAFETY: a non-null picture is live for the callback; the output takes a reference.
    output.picture = NonNull::new(picture).map(|picture| unsafe { CFRetained::retain(picture) });
}

/// A format description made from H.264 or HEVC parameter sets.
fn format_description(
    codec: Codec,
    parameter_sets: &[&[u8]],
) -> Result<CFRetained<CMFormatDescription>, DecodeError> {
    let pointers: Vec<NonNull<u8>> = parameter_sets
        .iter()
        .map(|set| NonNull::from(*set).cast::<u8>())
        .collect();
    let sizes: Vec<usize> = parameter_sets.iter().map(|set| set.len()).collect();
    let (Some(pointers_at), Some(sizes_at)) = (
        NonNull::new(pointers.as_ptr().cast_mut()),
        NonNull::new(sizes.as_ptr().cast_mut()),
    ) else {
        return Err(DecodeError::NoKeyframe);
    };
    let mut raw: *const CMFormatDescription = std::ptr::null();
    let out = NonNull::from(&mut raw);
    let length = LENGTH_SIZE as c_int;
    // SAFETY: the pointers and sizes describe live parameter sets for the call.
    let status = unsafe {
        match codec {
            Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                parameter_sets.len(),
                pointers_at,
                sizes_at,
                length,
                out,
            ),
            _ => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                parameter_sets.len(),
                pointers_at,
                sizes_at,
                length,
                None,
                out,
            ),
        }
    };
    if status != NO_ERR {
        return Err(DecodeError::VideoToolbox(status));
    }
    let raw = NonNull::new(raw.cast_mut()).ok_or(DecodeError::VideoToolbox(status))?;
    // SAFETY: a successful create returned an owned reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// A sample buffer holding a copy of `data`, length-prefixed NAL units of one access unit.
fn sample_buffer(
    data: &[u8],
    format: &CMFormatDescription,
    timestamp: u32,
) -> Result<CFRetained<CMSampleBuffer>, DecodeError> {
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: CoreMedia allocates `len` bytes of its own and returns a +1 block buffer.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            data.len(),
            None,
            std::ptr::null(),
            0,
            data.len(),
            kCMBlockBufferAssureMemoryNowFlag as CMBlockBufferFlags,
            NonNull::from(&mut block),
        )
    };
    if status != NO_ERR {
        return Err(DecodeError::VideoToolbox(status));
    }
    let block = NonNull::new(block).ok_or(DecodeError::VideoToolbox(status))?;
    // SAFETY: a successful create returned an owned reference.
    let block = unsafe { CFRetained::from_raw(block) };
    let source = NonNull::from(data).cast::<c_void>();
    // SAFETY: the block holds `len` bytes, which `data` fills.
    let status = unsafe { CMBlockBuffer::replace_data_bytes(source, &block, 0, data.len()) };
    if status != NO_ERR {
        return Err(DecodeError::VideoToolbox(status));
    }
    // SAFETY: reading CoreMedia's immutable statics, and making a time from plain values.
    let timing = unsafe {
        CMSampleTimingInfo {
            duration: kCMTimeInvalid,
            presentationTimeStamp: CMTime::new(i64::from(timestamp), TIMESCALE),
            decodeTimeStamp: kCMTimeInvalid,
        }
    };
    let size = data.len();
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: the block and format are live, and the timing and size arrays hold one entry.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            1,
            &timing,
            1,
            &size,
            NonNull::from(&mut sample),
        )
    };
    if status != NO_ERR {
        return Err(DecodeError::VideoToolbox(status));
    }
    let sample = NonNull::new(sample).ok_or(DecodeError::VideoToolbox(status))?;
    // SAFETY: a successful create returned an owned reference.
    Ok(unsafe { CFRetained::from_raw(sample) })
}
