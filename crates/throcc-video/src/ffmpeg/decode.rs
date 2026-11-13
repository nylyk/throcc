use std::ffi::CStr;

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use throcc_proto::messages::Codec;
use tracing::{info, warn};

use crate::frame::VideoFrame;
use crate::{DecodeError, Frame, MaxSize, Output, switches};

/// FFmpeg's own decoder for `codec`, the one that hands work to a hardware device.
fn native_decoder(codec: Codec) -> &'static CStr {
    match codec {
        Codec::Av1 => c"av1",
        Codec::Hevc => c"hevc",
        Codec::H264 => c"h264",
        Codec::Vp9 => c"vp9",
    }
}

/// The decoder for `codec` without a hardware device. FFmpeg's own AV1 decoder decodes
/// only on hardware.
fn software_decoder(codec: Codec) -> &'static CStr {
    match codec {
        Codec::Av1 => c"libdav1d",
        other => native_decoder(other),
    }
}

/// Hardware decoding devices on this platform, tried in order before software.
fn devices() -> &'static [ffi::AVHWDeviceType] {
    if cfg!(target_os = "linux") {
        &[ffi::AV_HWDEVICE_TYPE_VAAPI, ffi::AV_HWDEVICE_TYPE_CUDA]
    } else if cfg!(windows) {
        &[ffi::AV_HWDEVICE_TYPE_D3D11VA, ffi::AV_HWDEVICE_TYPE_CUDA]
    } else {
        &[]
    }
}

/// Decodes one remote share, on the GPU where it can. Shows the newest frame;
/// presentation time is ignored.
pub struct Decoder {
    ctx: AVCodecContext,
    codec: Codec,
    hardware: bool,
    engine: &'static str,
    max_size: MaxSize,
    /// Where hardware pictures are downloaded, with the pixel format the decoder had them
    /// in, kept while that format and their size hold.
    downloaded: Option<(ffi::AVPixelFormat, AVFrame)>,
    /// A spent picture, whose planes the next picture is written into.
    spare: Option<VideoFrame>,
    /// The timestamp of the first unit in the current run of hardware failures.
    failing_since: Option<u32>,
    /// Whether reference pictures were dropped since [`Decoder::take_flushed`] last ran.
    flushed: bool,
}

/// How long, in 90 kHz media time, a hardware decoder may keep failing before it is swapped
/// for a software one.
const HARDWARE_GRACE: u32 = 3 * 90_000;

impl Decoder {
    /// Opens a decoder that refuses pictures larger than `max_size`. Desktops draw pictures
    /// themselves, so `output` stays unused.
    pub fn new(codec: Codec, max_size: MaxSize, _output: Output) -> Result<Self, DecodeError> {
        let hardware = (!switches::software_decoding())
            .then(|| Self::on_hardware(codec, max_size))
            .flatten();
        let decoder = match hardware {
            Some(decoder) => decoder,
            None => Self::in_software(codec, max_size)?,
        };
        let place = if decoder.hardware {
            "hardware"
        } else {
            "software"
        };
        info!("decoding {codec:?} in {place}");
        Ok(decoder)
    }

    #[allow(
        clippy::unnecessary_cast,
        reason = "bindgen types AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 on MSVC only"
    )]
    fn on_hardware(codec: Codec, max_size: MaxSize) -> Option<Self> {
        let found = AVCodec::find_decoder_by_name(native_decoder(codec))?;
        devices().iter().find_map(|&device_type| {
            let supported = (0..).map_while(|i| found.hw_config(i)).any(|config| {
                config.device_type == device_type
                    && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            });
            if !supported {
                return None;
            }
            let device = AVHWDeviceContext::create(device_type, None, None, 0).ok()?;
            let mut ctx = context(&found, max_size.pixels());
            ctx.set_hw_device_ctx(device);
            ctx.open(None).ok()?;
            Some(Self {
                ctx,
                codec,
                hardware: true,
                engine: device_name(device_type),
                max_size,
                downloaded: None,
                spare: None,
                failing_since: None,
                flushed: false,
            })
        })
    }

    fn in_software(codec: Codec, max_size: MaxSize) -> Result<Self, DecodeError> {
        let found = AVCodec::find_decoder_by_name(software_decoder(codec))
            .ok_or(DecodeError::NoDecoder(codec))?;
        let mut ctx = context(&found, max_size.pixels());
        // Zero is a thread per core; FFmpeg's default is one. The low-delay flag keeps
        // pictures from being held across units, so this threads within each picture.
        // SAFETY: `ctx` is a live codec context that has not been opened yet.
        unsafe { (*ctx.as_mut_ptr()).thread_count = 0 };
        ctx.open(None)?;
        let engine = if found.name().to_bytes() == b"libdav1d" {
            "dav1d (software)"
        } else {
            "FFmpeg (software)"
        };
        Ok(Self {
            ctx,
            codec,
            hardware: false,
            engine,
            max_size,
            downloaded: None,
            spare: None,
            failing_since: None,
            flushed: false,
        })
    }

    /// Whether frames are decoded on a hardware device.
    #[cfg(test)]
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Desktops draw pictures themselves.
    pub fn present(&mut self, _frame: &Frame) {}

    /// Desktops have no views to follow.
    pub fn follow_view(&mut self) -> bool {
        false
    }

    /// Hands back a spent picture, so the next one reuses its memory.
    pub fn recycle(&mut self, frame: VideoFrame) {
        self.spare = Some(frame);
    }

    /// Pictures in flight come out only with the next unit: FFmpeg drains only at the end
    /// of a stream.
    pub fn drain(&mut self) -> Result<Option<Frame>, DecodeError> {
        Ok(None)
    }

    /// Whether the decoder dropped its reference pictures since the last call, so only a
    /// keyframe decodes cleanly again.
    pub fn take_flushed(&mut self) -> bool {
        std::mem::take(&mut self.flushed)
    }

    /// The hardware API or software decoder doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        self.engine
    }

    /// Decodes one access unit into its newest picture, if any. A hardware failure, not bad
    /// data, resets the decoder; failing for [`HARDWARE_GRACE`] swaps in software decoding.
    pub fn decode(&mut self, unit: &[u8], timestamp: u32) -> Result<Option<Frame>, DecodeError> {
        let mut result = self.decode_unit(unit, timestamp);
        if self.hardware_failed(&result) {
            let since = *self.failing_since.get_or_insert(timestamp);
            if timestamp.wrapping_sub(since) >= HARDWARE_GRACE {
                warn!("hardware decoding keeps failing; decoding in software");
                *self = Self::in_software(self.codec, self.max_size)?;
                return self.decode_unit(unit, timestamp);
            }
            self.ctx.flush_buffers();
            self.flushed = true;
            result = match self.decode_unit(unit, timestamp) {
                Ok(None) => result,
                retried => retried,
            };
        }
        if let Ok(Some(_)) = result {
            self.failing_since = None;
        }
        result
    }

    /// Whether `result` is a failure of the hardware rather than of the data it was given.
    fn hardware_failed(&self, result: &Result<Option<Frame>, DecodeError>) -> bool {
        self.hardware
            && matches!(result, Err(DecodeError::Ffmpeg(e)) if e.raw_error() != Some(ffi::AVERROR_INVALIDDATA))
    }

    fn decode_unit(&mut self, unit: &[u8], timestamp: u32) -> Result<Option<Frame>, DecodeError> {
        let mut packet = AVPacket::new();
        // SAFETY: av_new_packet allocates `len` bytes plus padding, filled right after.
        unsafe {
            if ffi::av_new_packet(packet.as_mut_ptr(), unit.len() as i32) < 0 {
                return Ok(None);
            }
            std::ptr::copy_nonoverlapping(unit.as_ptr(), packet.data, unit.len());
        }
        packet.set_pts(i64::from(timestamp));
        self.ctx.send_packet(Some(&packet))?;
        let mut newest = None;
        loop {
            match self.ctx.receive_frame() {
                Ok(frame) if !self.max_size.fits(frame.width.into(), frame.height.into()) => {
                    return Err(DecodeError::TooLarge(frame.width, frame.height));
                }
                Ok(frame) => newest = Some(frame),
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let Some(frame) = newest else {
            return Ok(None);
        };
        let frame = if frame.hw_frames_ctx.is_null() {
            &frame
        } else {
            // A transfer into a frame that is already allocated writes the decoder's planes
            // without checking that the frame has room for them, so the frame is kept only
            // for pictures a transfer would allocate the same frame for: ones of the format
            // and size it was allocated for.
            let format = decoded_format(&frame);
            let fits = |(of, d): &(ffi::AVPixelFormat, AVFrame)| {
                *of == format && d.width == frame.width && d.height == frame.height
            };
            if !self.downloaded.as_ref().is_some_and(fits) {
                self.downloaded = None;
            }
            let (_, downloaded) = self
                .downloaded
                .get_or_insert_with(|| (format, AVFrame::new()));
            downloaded.hwframe_transfer_data(&frame)?;
            &*downloaded
        };
        let picture = to_nv12(frame, timestamp, self.spare.take())
            .ok_or(DecodeError::Format(frame.format))?;
        Ok(Some(Frame::Planes(picture)))
    }
}

/// The pixel format the decoder holds the picture of a hardware `frame` in.
fn decoded_format(frame: &AVFrame) -> ffi::AVPixelFormat {
    // SAFETY: a hardware frame holds a reference to its frames context for as long as it
    // lives, and that buffer's data is the `AVHWFramesContext`.
    unsafe { (*(*frame.hw_frames_ctx).data.cast::<ffi::AVHWFramesContext>()).sw_format }
}

fn device_name(device_type: ffi::AVHWDeviceType) -> &'static str {
    match device_type {
        ffi::AV_HWDEVICE_TYPE_VAAPI => "VA-API",
        ffi::AV_HWDEVICE_TYPE_CUDA => "NVDEC",
        ffi::AV_HWDEVICE_TYPE_D3D11VA => "D3D11VA",
        _ => "hardware",
    }
}

/// A low-delay context for `found` that refuses pictures of more than `max_pixels`.
fn context(found: &AVCodec, max_pixels: usize) -> AVCodecContext {
    let mut ctx = AVCodecContext::new(found);
    ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
    // SAFETY: `ctx` is a live codec context that has not been opened yet.
    unsafe { (*ctx.as_mut_ptr()).max_pixels = i64::try_from(max_pixels).unwrap_or(i64::MAX) };
    ctx
}

/// Copies a 4:2:0 frame, planar or NV12, into NV12 cropped to even dimensions, reusing the
/// planes of `spare` if given. `None` for frames in any other format.
pub(crate) fn to_nv12(
    frame: &AVFrame,
    timestamp: u32,
    spare: Option<VideoFrame>,
) -> Option<VideoFrame> {
    let (width, height) = (frame.width as usize & !1, frame.height as usize & !1);
    let planar = [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_YUVJ420P].contains(&frame.format);
    if !planar && frame.format != ffi::AV_PIX_FMT_NV12 {
        return None;
    }
    let (mut y, mut uv) = spare.map_or_else(Default::default, |spare| (spare.y, spare.uv));
    y.clear();
    uv.clear();
    y.reserve(width * height);
    uv.reserve(width * height / 2);
    // SAFETY: the decoder or transfer sized each plane for `width`×`height` 4:2:0 with the
    // strides in `linesize`, and each row read lies within one row of one plane.
    unsafe {
        for row in 0..height {
            y.extend_from_slice(plane_row(frame, 0, row, width));
        }
        for row in 0..height / 2 {
            if planar {
                let u = plane_row(frame, 1, row, width / 2);
                let v = plane_row(frame, 2, row, width / 2);
                uv.extend(u.iter().zip(v).flat_map(|(u, v)| [*u, *v]));
            } else {
                uv.extend_from_slice(plane_row(frame, 1, row, width));
            }
        }
    }
    Some(VideoFrame {
        width,
        height,
        y,
        uv,
        timestamp,
    })
}

/// The first `len` bytes of row `row` of `frame`'s plane `plane`.
///
/// # Safety
/// The plane must hold at least `row + 1` rows of its `linesize`, each at least `len` long.
unsafe fn plane_row(frame: &AVFrame, plane: usize, row: usize, len: usize) -> &[u8] {
    // SAFETY: the caller promises the row is within the plane.
    unsafe {
        std::slice::from_raw_parts(
            frame.data[plane].add(row * frame.linesize[plane] as usize),
            len,
        )
    }
}
