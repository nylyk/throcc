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

/// Software decoders, fastest first.
fn decoders(codec: Codec) -> &'static [&'static CStr] {
    match codec {
        Codec::Av1 => &[c"libdav1d", c"av1"],
        Codec::Hevc => &[c"hevc"],
        Codec::H264 => &[c"h264"],
        Codec::Vp9 => &[c"vp9"],
    }
}

/// FFmpeg's own decoder for `codec`, the one that hands work to a hardware device.
fn native(codec: Codec) -> &'static CStr {
    match codec {
        Codec::Av1 => c"av1",
        Codec::Hevc => c"hevc",
        Codec::H264 => c"h264",
        Codec::Vp9 => c"vp9",
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
        let found = AVCodec::find_decoder_by_name(native(codec))?;
        devices().iter().find_map(|&device_type| {
            let supported = (0..).map_while(|i| found.hw_config(i)).any(|config| {
                config.device_type == device_type
                    && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            });
            if !supported {
                return None;
            }
            let device = AVHWDeviceContext::create(device_type, None, None, 0).ok()?;
            let mut ctx = AVCodecContext::new(&found);
            ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
            limit_pixels(&mut ctx, max_pixels);
            ctx.set_hw_device_ctx(device);
            ctx.open(None).ok()?;
            Some(Self {
                ctx,
                codec,
                hardware: true,
                max_pixels,
            })
        })
    }

    fn in_software(codec: Codec, max_pixels: usize) -> Result<Self, DecodeError> {
        let found = decoders(codec)
            .iter()
            .find_map(|name| AVCodec::find_decoder_by_name(name))
            .ok_or(DecodeError::NoDecoder(codec))?;
        let mut ctx = AVCodecContext::new(&found);
        ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
        limit_pixels(&mut ctx, max_pixels);
        ctx.open(None)?;
        Ok(Self {
            ctx,
            codec,
            hardware: false,
            max_pixels,
        })
    }

    /// Whether frames are decoded on a hardware device.
    pub fn is_hardware(&self) -> bool {
        self.hardware
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
                Ok(frame) if frame.hw_frames_ctx.is_null() => {
                    newest = Some(to_nv12(&frame, timestamp)?);
                }
                Ok(frame) => {
                    let mut copied = AVFrame::new();
                    copied.hwframe_transfer_data(&frame)?;
                    newest = Some(to_nv12(&copied, timestamp)?);
                }
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(newest)
    }
}

fn limit_pixels(ctx: &mut AVCodecContext, max_pixels: usize) {
    // SAFETY: `ctx` is a live codec context that has not been opened yet.
    unsafe { (*ctx.as_mut_ptr()).max_pixels = i64::try_from(max_pixels).unwrap_or(i64::MAX) };
}

fn fits(frame: &AVFrame, max_pixels: usize) -> bool {
    let side = |v: i32| usize::try_from(v).unwrap_or(usize::MAX);
    side(frame.width).saturating_mul(side(frame.height)) <= max_pixels
}

/// Copies a decoded 4:2:0 frame into NV12, cropped to even dimensions.
fn to_nv12(frame: &AVFrame, timestamp: u32) -> Result<VideoFrame, DecodeError> {
    let (width, height) = (frame.width as usize & !1, frame.height as usize & !1);
    let mut picture = VideoFrame::blank(width, height);
    picture.timestamp = timestamp;
    let chroma_rows = height / 2;
    let chroma_cols = width / 2;
    let planar = [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_YUVJ420P];
    // SAFETY: the decoder sized each plane for `width`×`height` 4:2:0 with the strides in
    // `linesize`; reads stay within one row of one plane.
    unsafe {
        for row in 0..height {
            let src = frame.data[0].add(row * frame.linesize[0] as usize);
            std::ptr::copy_nonoverlapping(src, picture.y.as_mut_ptr().add(row * width), width);
        }
        if planar.contains(&frame.format) {
            for row in 0..chroma_rows {
                for col in 0..chroma_cols {
                    let at = row * width + col * 2;
                    picture.uv[at] = *frame.data[1].add(row * frame.linesize[1] as usize + col);
                    picture.uv[at + 1] = *frame.data[2].add(row * frame.linesize[2] as usize + col);
                }
            }
        } else if frame.format == ffi::AV_PIX_FMT_NV12 {
            for row in 0..chroma_rows {
                let src = frame.data[1].add(row * frame.linesize[1] as usize);
                std::ptr::copy_nonoverlapping(src, picture.uv.as_mut_ptr().add(row * width), width);
            }
        } else {
            return Err(DecodeError::Format(frame.format));
        }
    }
    Ok(picture)
}
