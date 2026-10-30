use std::ffi::{CStr, CString};

use bytes::Bytes;
use rsmpeg::avcodec::{AVCodec, AVCodecContext};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, opt_set, ra};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;
use throcc_proto::frame::VIDEO_CLOCK_HZ;
use throcc_proto::messages::{Codec, Recovery};
use tracing::{debug, info};

use super::frame::VideoFrame;

/// A GOP cap of this many seconds bounds how long a picture stays broken without a request.
const GOP_SECONDS: i32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("no usable video encoder on this machine")]
    NoEncoder,
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] RsmpegError),
    #[error("can't scale the picture to the encoder's size")]
    Scaler,
    #[error("the VA-API encoder has no surface pool")]
    NoSurfaces,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub kbps: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    Nv12,
    Yuv420p,
}

/// One rung of the encoder ladder (PLAN §5.9, S4).
struct Candidate {
    name: &'static CStr,
    codec: Codec,
    input: Input,
    intra_refresh: bool,
    /// Largest picture the encoder is used for, as (pixels, fps).
    limit: Option<(usize, u32)>,
    options: &'static [(&'static CStr, &'static CStr)],
    /// Takes frames in VA-API surfaces, uploaded from the NV12 picture.
    vaapi: bool,
}

const LADDER: &[Candidate] = &[
    Candidate {
        name: c"av1_nvenc",
        codec: Codec::Av1,
        input: Input::Nv12,
        intra_refresh: true,
        limit: None,
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"av1_vaapi",
        codec: Codec::Av1,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"hevc_nvenc",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: true,
        limit: None,
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"hevc_mf",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: MEDIA_FOUNDATION,
        vaapi: false,
    },
    Candidate {
        name: c"hevc_vaapi",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"hevc_videotoolbox",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: VIDEOTOOLBOX,
        vaapi: false,
    },
    Candidate {
        name: c"h264_nvenc",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: true,
        limit: None,
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"h264_mf",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: MEDIA_FOUNDATION,
        vaapi: false,
    },
    Candidate {
        name: c"h264_vaapi",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"h264_videotoolbox",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        limit: None,
        options: VIDEOTOOLBOX,
        vaapi: false,
    },
    Candidate {
        name: c"libopenh264",
        codec: Codec::H264,
        input: Input::Yuv420p,
        intra_refresh: false,
        limit: Some((1920 * 1080, 30)),
        options: &[(c"allow_skip_frames", c"1")],
        vaapi: false,
    },
];

const NVENC: &[(&CStr, &CStr)] = &[
    (c"preset", c"p1"),
    (c"tune", c"ull"),
    (c"zerolatency", c"1"),
    (c"rc", c"cbr"),
    (c"delay", c"0"),
    (c"intra-refresh", c"1"),
    (c"forced-idr", c"1"),
];
const MEDIA_FOUNDATION: &[(&CStr, &CStr)] = &[
    (c"hw_encoding", c"1"),
    (c"scenario", c"display_remoting"),
    (c"rate_control", c"cbr"),
];
/// Surfaces in a VA-API encoder's pool: enough for the frames it holds plus one uploading.
const VAAPI_SURFACES: i32 = 8;
const VAAPI: &[(&CStr, &CStr)] = &[(c"rc_mode", c"CBR"), (c"async_depth", c"1")];
const VIDEOTOOLBOX: &[(&CStr, &CStr)] = &[(c"realtime", c"1"), (c"allow_sw", c"0")];

/// An encoded access unit.
#[derive(Clone, Debug)]
pub struct EncodedUnit {
    pub data: Bytes,
    pub keyframe: bool,
    pub timestamp: u32,
}

/// A low-latency hardware video encoder: no B-frames, no lookahead, a VBV of about two
/// frames, and recovery by intra refresh where the encoder has it, else by IDR.
pub struct Encoder {
    ctx: AVCodecContext,
    codec: Codec,
    name: &'static str,
    input: Input,
    intra_refresh: bool,
    vaapi: bool,
    width: usize,
    height: usize,
    idr_next: bool,
    /// Fits pictures of another size into the encoder's, keyed by their size.
    scaler: Option<((usize, usize), SwsContext)>,
}

impl Encoder {
    /// Opens the best encoder that works here, in the order AV1, HEVC, H.264.
    pub fn open(config: EncoderConfig) -> Result<Self, EncodeError> {
        let pixels = config.width * config.height;
        for candidate in LADDER {
            if candidate
                .limit
                .is_some_and(|(max_pixels, max_fps)| pixels > max_pixels || config.fps > max_fps)
            {
                continue;
            }
            let Some(codec) = AVCodec::find_encoder_by_name(candidate.name) else {
                continue;
            };
            match open_candidate(&codec, candidate, config) {
                Ok(ctx) => {
                    let name = candidate.name.to_str().unwrap_or("encoder");
                    info!(encoder = name, "video encoder ready");
                    return Ok(Self {
                        ctx,
                        codec: candidate.codec,
                        name,
                        input: candidate.input,
                        intra_refresh: candidate.intra_refresh,
                        vaapi: candidate.vaapi,
                        width: config.width,
                        height: config.height,
                        idr_next: true,
                        scaler: None,
                    });
                }
                Err(e) => debug!(encoder = ?candidate.name, "unusable: {e}"),
            }
        }
        Err(EncodeError::NoEncoder)
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Asks for recovery: an IDR, or for `Refresh`, whatever the encoder's intra refresh
    /// does on its own. An encoder without intra refresh treats `Refresh` as `Idr`.
    pub fn recover(&mut self, kind: Recovery) {
        if kind == Recovery::Idr || !self.intra_refresh {
            self.idr_next = true;
        }
    }

    pub fn encode(&mut self, picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        let mut frame = new_frame(self.width, self.height, self.input)?;
        if (picture.width, picture.height) == (self.width, self.height) {
            copy_into(&mut frame, picture, self.input);
        } else {
            self.fit(picture, &mut frame)?;
        }
        if self.vaapi {
            frame = self.upload(&frame)?;
        }
        frame.set_pts(i64::from(picture.timestamp));
        if std::mem::take(&mut self.idr_next) {
            frame.set_pict_type(ffi::AV_PICTURE_TYPE_I);
        }
        let mut units = Vec::new();
        match self.ctx.send_frame(Some(&frame)) {
            Err(RsmpegError::SendFrameAgainError) => {
                self.drain(&mut units)?;
                self.ctx.send_frame(Some(&frame))?;
            }
            sent => sent?,
        }
        self.drain(&mut units)?;
        Ok(units)
    }

    /// Copies an NV12 frame into a VA-API surface from the encoder's pool.
    fn upload(&mut self, frame: &AVFrame) -> Result<AVFrame, EncodeError> {
        let mut surface = AVFrame::new();
        let Some(mut pool) = self.ctx.hw_frames_ctx_mut() else {
            return Err(EncodeError::NoSurfaces);
        };
        pool.get_buffer(&mut surface)?;
        surface.hwframe_transfer_data(frame)?;
        Ok(surface)
    }

    /// Takes every packet the encoder has ready.
    fn drain(&mut self, units: &mut Vec<EncodedUnit>) -> Result<(), EncodeError> {
        loop {
            match self.ctx.receive_packet() {
                Ok(packet) => {
                    // SAFETY: the packet owns `size` bytes at `data` until it drops below.
                    let data =
                        unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) };
                    units.push(EncodedUnit {
                        data: Bytes::copy_from_slice(data),
                        keyframe: packet.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0,
                        timestamp: packet.pts as u32,
                    });
                }
                Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Encoder {
    /// Scales `picture` into `frame` keeping its aspect ratio, with black bars around it.
    fn fit(&mut self, picture: &VideoFrame, frame: &mut AVFrame) -> Result<(), EncodeError> {
        let source_size = (picture.width, picture.height);
        let (x, y, width, height) = fitted(source_size, (self.width, self.height));
        if self
            .scaler
            .as_ref()
            .is_none_or(|(size, _)| *size != source_size)
        {
            let context = SwsContext::get_context(
                picture.width as i32,
                picture.height as i32,
                ffi::AV_PIX_FMT_NV12,
                width as i32,
                height as i32,
                pixel_format(self.input),
                ffi::SWS_BILINEAR,
                None,
                None,
                None,
            )
            .ok_or(EncodeError::Scaler)?;
            self.scaler = Some((source_size, context));
        }
        let Some((_, scaler)) = self.scaler.as_mut() else {
            return Err(EncodeError::Scaler);
        };
        let mut source = new_frame(picture.width, picture.height, Input::Nv12)?;
        copy_into(&mut source, picture, Input::Nv12);
        fill_black(frame, self.input, self.height);
        let chroma_x = match self.input {
            Input::Nv12 => x,
            Input::Yuv420p => x / 2,
        };
        // SAFETY: `fitted` keeps the rectangle inside the frame with even offsets, so every
        // plane pointer stays within its plane and sws writes only `width`×`height` from it.
        unsafe {
            let destination = [
                frame.data[0].add(y * frame.linesize[0] as usize + x),
                frame.data[1].add(y / 2 * frame.linesize[1] as usize + chroma_x),
                match self.input {
                    Input::Nv12 => std::ptr::null_mut(),
                    Input::Yuv420p => frame.data[2].add(y / 2 * frame.linesize[2] as usize + x / 2),
                },
                std::ptr::null_mut(),
            ];
            scaler.scale(
                source.data.as_ptr() as _,
                source.linesize.as_ptr(),
                0,
                picture.height as i32,
                destination.as_ptr(),
                frame.linesize.as_ptr(),
            )?;
        }
        Ok(())
    }
}

/// The largest rectangle with the aspect ratio of `source`, centred in `target`, as
/// (x, y, width, height), all even.
fn fitted(source: (usize, usize), target: (usize, usize)) -> (usize, usize, usize, usize) {
    let (sw, sh) = (source.0.max(1), source.1.max(1));
    let (tw, th) = target;
    let (width, height) = if sw * th > tw * sh {
        (tw, sh * tw / sw)
    } else {
        (sw * th / sh, th)
    };
    let even = |v: usize| (v & !1).max(2);
    let (width, height) = (even(width).min(tw & !1), even(height).min(th & !1));
    (
        ((tw - width) / 2) & !1,
        ((th - height) / 2) & !1,
        width,
        height,
    )
}

fn pixel_format(input: Input) -> i32 {
    match input {
        Input::Nv12 => ffi::AV_PIX_FMT_NV12,
        Input::Yuv420p => ffi::AV_PIX_FMT_YUV420P,
    }
}

fn new_frame(width: usize, height: usize, input: Input) -> Result<AVFrame, RsmpegError> {
    let mut frame = AVFrame::new();
    frame.set_width(width as i32);
    frame.set_height(height as i32);
    frame.set_format(pixel_format(input));
    frame.alloc_buffer()?;
    Ok(frame)
}

/// Paints the whole frame full-range black.
fn fill_black(frame: &mut AVFrame, input: Input, height: usize) {
    let planes: &[(usize, u8, usize)] = match input {
        Input::Nv12 => &[(0, 0, height), (1, 128, height.div_ceil(2))],
        Input::Yuv420p => &[
            (0, 0, height),
            (1, 128, height.div_ceil(2)),
            (2, 128, height.div_ceil(2)),
        ],
    };
    for &(plane, value, rows) in planes {
        // SAFETY: `alloc_buffer` gave the plane `rows` rows of `linesize` bytes.
        unsafe {
            std::ptr::write_bytes(
                frame.data[plane],
                value,
                rows * frame.linesize[plane] as usize,
            );
        }
    }
}

fn open_candidate(
    codec: &AVCodec,
    candidate: &Candidate,
    config: EncoderConfig,
) -> Result<AVCodecContext, RsmpegError> {
    let mut ctx = AVCodecContext::new(codec);
    ctx.set_width(config.width as i32);
    ctx.set_height(config.height as i32);
    ctx.set_time_base(ra(1, VIDEO_CLOCK_HZ as i32));
    ctx.set_framerate(ra(config.fps as i32, 1));
    ctx.set_pix_fmt(pixel_format(candidate.input));
    if candidate.vaapi {
        let device = AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_VAAPI, None, None, 0)?;
        let mut pool = device.hwframe_ctx_alloc();
        let surfaces = pool.data();
        surfaces.format = ffi::AV_PIX_FMT_VAAPI;
        surfaces.sw_format = pixel_format(candidate.input);
        surfaces.width = config.width as i32;
        surfaces.height = config.height as i32;
        surfaces.initial_pool_size = VAAPI_SURFACES;
        pool.init()?;
        ctx.set_pix_fmt(ffi::AV_PIX_FMT_VAAPI);
        ctx.set_hw_frames_ctx(pool);
    }
    let bitrate = i64::from(config.kbps) * 1000;
    ctx.set_bit_rate(bitrate);
    ctx.set_max_b_frames(0);
    ctx.set_gop_size(config.fps as i32 * GOP_SECONDS);
    ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
    let maxrate = cstring(&bitrate.to_string());
    let bufsize = cstring(&(bitrate * 2 / i64::from(config.fps.max(1))).to_string());
    let common = [
        (c"maxrate", maxrate.as_c_str()),
        (c"bufsize", bufsize.as_c_str()),
        (c"color_range", c"pc"),
        (c"colorspace", c"bt709"),
    ];
    for (key, value) in common.into_iter().chain(candidate.options.iter().copied()) {
        set_option(&mut ctx, key, value)?;
    }
    // rsmpeg 0.18 frees an options dictionary twice when opening fails after ffmpeg has
    // replaced it, which crashes the process; the options go straight onto the context.
    ctx.open(None)?;
    Ok(ctx)
}

/// ffmpeg's `AVERROR_OPTION_NOT_FOUND`, `-MKTAG(0xF8, 'O', 'P', 'T')`.
const OPTION_NOT_FOUND: i32 = -0x5450_4FF8;

/// Sets an encoder option as an options dictionary would: one the encoder doesn't have
/// is skipped, a value it rejects is an error.
fn set_option(ctx: &mut AVCodecContext, key: &CStr, value: &CStr) -> Result<(), RsmpegError> {
    let search_children = ffi::AV_OPT_SEARCH_CHILDREN as i32;
    // SAFETY: an AVCodecContext begins with its AVClass pointer, as `opt_set` requires.
    match unsafe { opt_set(ctx.as_mut_ptr().cast(), key, value, search_children) } {
        Err(RsmpegError::AVError(OPTION_NOT_FOUND)) => Ok(()),
        set => set,
    }
}

fn cstring(text: &str) -> CString {
    CString::new(text).unwrap_or_default()
}

/// Copies an NV12 picture into a freshly allocated frame of the encoder's input format.
/// The frame must have been allocated at the picture's size.
fn copy_into(frame: &mut AVFrame, picture: &VideoFrame, input: Input) {
    let (width, height) = (picture.width, picture.height);
    let chroma_rows = height.div_ceil(2);
    // SAFETY: `alloc_buffer` sized every plane for `width`×`height` with the strides in
    // `linesize`; each write below stays within one row of one plane.
    unsafe {
        let y_stride = frame.linesize[0] as usize;
        for row in 0..height {
            let dst = frame.data[0].add(row * y_stride);
            std::ptr::copy_nonoverlapping(picture.y.as_ptr().add(row * width), dst, width);
        }
        match input {
            Input::Nv12 => {
                let stride = frame.linesize[1] as usize;
                for row in 0..chroma_rows {
                    let dst = frame.data[1].add(row * stride);
                    std::ptr::copy_nonoverlapping(picture.uv.as_ptr().add(row * width), dst, width);
                }
            }
            Input::Yuv420p => {
                let (u_stride, v_stride) = (frame.linesize[1] as usize, frame.linesize[2] as usize);
                for row in 0..chroma_rows {
                    for col in 0..width.div_ceil(2) {
                        let at = row * width + col * 2;
                        *frame.data[1].add(row * u_stride + col) = picture.uv[at];
                        *frame.data[2].add(row * v_stride + col) = picture.uv[at + 1];
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fitted;

    #[test]
    fn fitted_rectangles_are_even_centred_and_inside() {
        assert_eq!(fitted((2560, 1440), (1920, 1080)), (0, 0, 1920, 1080));
        assert_eq!(fitted((1000, 1000), (1280, 720)), (280, 0, 720, 720));
        assert_eq!(fitted((3840, 1080), (1280, 720)), (0, 180, 1280, 360));
        for source in [(1, 1), (1, 5000), (5000, 1), (641, 359), (1279, 721)] {
            let (x, y, width, height) = fitted(source, (1280, 720));
            assert!(
                [x, y, width, height].iter().all(|v| v % 2 == 0),
                "{source:?}"
            );
            assert!(
                width >= 2 && height >= 2 && x + width <= 1280 && y + height <= 720,
                "{source:?}"
            );
        }
    }
}

#[cfg(test)]
mod failed_open {
    use super::*;

    #[test]
    fn an_encoder_that_fails_to_open_is_cleaned_up_safely() {
        let Some(codec) = AVCodec::find_encoder_by_name(c"hevc_videotoolbox") else {
            return;
        };
        let Some(candidate) = LADDER.iter().find(|c| c.name == c"hevc_videotoolbox") else {
            return;
        };
        for _ in 0..20 {
            let config = EncoderConfig {
                width: 0,
                height: 0,
                fps: 30,
                kbps: 500,
            };
            assert!(open_candidate(&codec, candidate, config).is_err());
        }
    }
}
