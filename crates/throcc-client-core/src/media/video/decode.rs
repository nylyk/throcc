use std::ffi::CStr;

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVPacket};
use rsmpeg::avutil::AVFrame;
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use throcc_proto::messages::Codec;

use super::frame::VideoFrame;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("no decoder for {0:?}")]
    NoDecoder(Codec),
    #[error("decoded frames in pixel format {0}, which throcc doesn't present")]
    Format(i32),
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] RsmpegError),
}

/// Software decoders, fastest first. A hardware path replaces these per platform.
fn decoders(codec: Codec) -> &'static [&'static CStr] {
    match codec {
        Codec::Av1 => &[c"libdav1d", c"av1"],
        Codec::Hevc => &[c"hevc"],
        Codec::H264 => &[c"h264"],
    }
}

/// Decodes one remote share. Shows the newest frame; presentation time is ignored.
pub struct Decoder {
    ctx: AVCodecContext,
}

impl Decoder {
    pub fn new(codec: Codec) -> Result<Self, DecodeError> {
        let found = decoders(codec)
            .iter()
            .find_map(|name| AVCodec::find_decoder_by_name(name))
            .ok_or(DecodeError::NoDecoder(codec))?;
        let mut ctx = AVCodecContext::new(&found);
        ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
        ctx.open(None)?;
        Ok(Self { ctx })
    }

    /// Decodes one access unit. Returns the newest picture it produced, if any.
    pub fn decode(
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
                Ok(frame) => newest = Some(to_nv12(&frame, timestamp)?),
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(newest)
    }
}

/// Copies a decoded 4:2:0 frame into NV12.
fn to_nv12(frame: &AVFrame, timestamp: u32) -> Result<VideoFrame, DecodeError> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let mut picture = VideoFrame::blank(width, height);
    picture.timestamp = timestamp;
    let chroma_rows = height.div_ceil(2);
    let chroma_cols = width.div_ceil(2);
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
