use std::ffi::CStr;

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use throcc_proto::messages::Codec;
use tracing::{info, warn};

use super::frame::VideoFrame;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("no decoder for {0:?}")]
    NoDecoder(Codec),
    #[error("decoded frames in pixel format {0}, which throcc doesn't present")]
    Format(i32),
    #[error("a {0}×{1} picture is larger than this share may send")]
    TooLarge(i32, i32),
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] RsmpegError),
}

/// Software decoders, fastest first. The last is FFmpeg's own, which is also the one that
/// hands work to a hardware device.
fn decoders(codec: Codec) -> &'static [&'static CStr] {
    match codec {
        Codec::Av1 => &[c"libdav1d", c"av1"],
        Codec::Hevc => &[c"hevc"],
        Codec::H264 => &[c"h264"],
        Codec::Vp9 => &[c"vp9"],
    }
}

/// Hardware decoding devices on this platform, tried in order before software.
fn devices() -> &'static [ffi::AVHWDeviceType] {
    if cfg!(target_os = "macos") {
        &[ffi::AV_HWDEVICE_TYPE_VIDEOTOOLBOX]
    } else if cfg!(target_os = "linux") {
        &[ffi::AV_HWDEVICE_TYPE_VAAPI, ffi::AV_HWDEVICE_TYPE_CUDA]
    } else if cfg!(windows) {
        &[ffi::AV_HWDEVICE_TYPE_D3D11VA, ffi::AV_HWDEVICE_TYPE_CUDA]
    } else {
        &[]
    }
}

/// The most pixels any decoded picture may have.
pub const MAX_PIXELS: usize = 4096 * 4096;

/// Decodes one remote share, on the GPU where it can. Shows the newest frame;
/// presentation time is ignored.
pub struct Decoder {
    ctx: AVCodecContext,
    codec: Codec,
    hardware: bool,
    engine: &'static str,
    max_pixels: usize,
}

impl Decoder {
    /// Opens a decoder that refuses pictures of more than `max_pixels`, capped at
    /// [`MAX_PIXELS`].
    pub fn new(codec: Codec, max_pixels: usize) -> Result<Self, DecodeError> {
        let max_pixels = max_pixels.min(MAX_PIXELS);
        let decoder = match Self::on_hardware(codec, max_pixels) {
            Some(decoder) => decoder,
            None => Self::in_software(codec, max_pixels)?,
        };
        let place = if decoder.hardware {
            "hardware"
        } else {
            "software"
        };
        info!("decoding {codec:?} in {place}");
        Ok(decoder)
    }

    fn on_hardware(codec: Codec, max_pixels: usize) -> Option<Self> {
        let found = AVCodec::find_decoder_by_name(decoders(codec).last()?)?;
        devices().iter().find_map(|&device_type| {
            let supported = (0..).map_while(|i| found.hw_config(i)).any(|config| {
                config.device_type == device_type
                    && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            });
            if !supported {
                return None;
            }
            let device = AVHWDeviceContext::create(device_type, None, None, 0).ok()?;
            let mut ctx = context(&found, max_pixels);
            ctx.set_hw_device_ctx(device);
            ctx.open(None).ok()?;
            Some(Self {
                ctx,
                codec,
                hardware: true,
                engine: device_name(device_type),
                max_pixels,
            })
        })
    }

    fn in_software(codec: Codec, max_pixels: usize) -> Result<Self, DecodeError> {
        let found = decoders(codec)
            .iter()
            .find_map(|name| AVCodec::find_decoder_by_name(name))
            .ok_or(DecodeError::NoDecoder(codec))?;
        let mut ctx = context(&found, max_pixels);
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
            max_pixels,
        })
    }

    /// Whether frames are decoded on a hardware device.
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// The hardware API or software decoder doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        self.engine
    }

    /// Decodes one access unit. Returns the newest picture it produced, if any. A hardware
    /// decoder that fails is swapped for a software one, which decodes the same unit.
    pub fn decode(
        &mut self,
        unit: &[u8],
        timestamp: u32,
    ) -> Result<Option<VideoFrame>, DecodeError> {
        match self.decode_unit(unit, timestamp) {
            Err(e) if self.hardware => {
                warn!("hardware decoding failed ({e}); decoding in software");
                *self = Self::in_software(self.codec, self.max_pixels)?;
                self.decode_unit(unit, timestamp)
            }
            result => result,
        }
    }

    fn decode_unit(
        &mut self,
        unit: &[u8],
        timestamp: u32,
    ) -> Result<Option<VideoFrame>, DecodeError> {
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
                Ok(frame) if !fits(&frame, self.max_pixels) => {
                    return Err(DecodeError::TooLarge(frame.width, frame.height));
                }
                Ok(frame) => newest = Some(frame),
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let Some(mut frame) = newest else {
            return Ok(None);
        };
        if !frame.hw_frames_ctx.is_null() {
            let mut copied = AVFrame::new();
            copied.hwframe_transfer_data(&frame)?;
            frame = copied;
        }
        let picture = to_nv12(&frame, timestamp).ok_or(DecodeError::Format(frame.format))?;
        Ok(Some(picture))
    }
}

fn device_name(device_type: ffi::AVHWDeviceType) -> &'static str {
    match device_type {
        ffi::AV_HWDEVICE_TYPE_VIDEOTOOLBOX => "VideoToolbox",
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

fn fits(frame: &AVFrame, max_pixels: usize) -> bool {
    let side = |v: i32| usize::try_from(v).unwrap_or(usize::MAX);
    side(frame.width).saturating_mul(side(frame.height)) <= max_pixels
}

/// Copies a 4:2:0 frame, planar or NV12, into NV12 cropped to even dimensions. `None` for
/// frames in any other format.
pub(super) fn to_nv12(frame: &AVFrame, timestamp: u32) -> Option<VideoFrame> {
    let (width, height) = (frame.width as usize & !1, frame.height as usize & !1);
    let planar = [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_YUVJ420P].contains(&frame.format);
    if !planar && frame.format != ffi::AV_PIX_FMT_NV12 {
        return None;
    }
    let mut y = Vec::with_capacity(width * height);
    let mut uv = Vec::with_capacity(width * height / 2);
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
