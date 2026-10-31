use std::ffi::{CStr, CString};

use bytes::Bytes;
use rsmpeg::avcodec::{AVCodec, AVCodecContext};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext, opt_set, ra};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;
use throcc_proto::frame::VIDEO_CLOCK_HZ;
use throcc_proto::messages::{Codec, Recovery};
use tracing::{debug, info};

#[cfg(target_os = "linux")]
use super::frame::share_size;
use super::frame::{VideoFrame, fit_within};
#[cfg(target_os = "linux")]
use super::vaapi::{self, Rectangle, VaError, Vpp};
#[cfg(target_os = "linux")]
use crate::media::capture::DmaBuf;

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
    #[error("this encoder takes no frames in GPU memory")]
    NoGpuInput,
    #[cfg(target_os = "linux")]
    #[error("importing a frame in GPU memory: {0}")]
    Import(VaError),
    #[cfg(target_os = "linux")]
    #[error("GPU video processing: {0}")]
    Gpu(VaError),
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
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"av1_vaapi",
        codec: Codec::Av1,
        input: Input::Nv12,
        intra_refresh: false,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"hevc_nvenc",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: true,
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"hevc_mf",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        options: MEDIA_FOUNDATION,
        vaapi: false,
    },
    Candidate {
        name: c"hevc_vaapi",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"hevc_videotoolbox",
        codec: Codec::Hevc,
        input: Input::Nv12,
        intra_refresh: false,
        options: VIDEOTOOLBOX,
        vaapi: false,
    },
    Candidate {
        name: c"h264_nvenc",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: true,
        options: NVENC,
        vaapi: false,
    },
    Candidate {
        name: c"h264_mf",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        options: MEDIA_FOUNDATION,
        vaapi: false,
    },
    Candidate {
        name: c"h264_vaapi",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        options: VAAPI,
        vaapi: true,
    },
    Candidate {
        name: c"h264_videotoolbox",
        codec: Codec::H264,
        input: Input::Nv12,
        intra_refresh: false,
        options: VIDEOTOOLBOX,
        vaapi: false,
    },
    Candidate {
        name: c"libvpx-vp9",
        codec: Codec::Vp9,
        input: Input::Yuv420p,
        intra_refresh: false,
        options: LIBVPX,
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
#[cfg(target_os = "linux")]
const PREVIEW_SURFACES: i32 = 2;
const VAAPI: &[(&CStr, &CStr)] = &[(c"rc_mode", c"CBR"), (c"async_depth", c"1")];
const LIBVPX: &[(&CStr, &CStr)] = &[
    (c"deadline", c"realtime"),
    (c"cpu-used", c"8"),
    (c"lag-in-frames", c"0"),
    (c"row-mt", c"1"),
    (c"tune-content", c"screen"),
    (c"threads", c"auto"),
];
const VIDEOTOOLBOX: &[(&CStr, &CStr)] = &[(c"realtime", c"1"), (c"allow_sw", c"0")];

/// An encoded access unit.
#[derive(Clone, Debug)]
pub struct EncodedUnit {
    pub data: Bytes,
    pub keyframe: bool,
    pub timestamp: u32,
}

/// A low-latency video encoder, hardware where there is one: no B-frames, no lookahead,
/// a VBV of about two frames, and recovery by intra refresh where the encoder has it,
/// else by IDR.
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
    /// The VA-API device of a VA-API encoder.
    device: Option<AVHWDeviceContext>,
    #[cfg(target_os = "linux")]
    gpu: Option<GpuInput>,
}

/// Video processing on a VA-API encoder's device, and the pool previews are drawn into.
#[cfg(target_os = "linux")]
struct GpuInput {
    vpp: Vpp,
    device: AVHWDeviceContext,
    previews: Option<((usize, usize), AVHWFramesContext)>,
}

impl Encoder {
    /// Opens the best encoder that works here: hardware AV1, HEVC, then H.264, then VP9 in
    /// software.
    pub fn open(config: EncoderConfig) -> Result<Self, EncodeError> {
        LADDER
            .iter()
            .filter(|candidate| has_driver(candidate))
            .find_map(|candidate| Self::open_with(candidate, config))
            .ok_or(EncodeError::NoEncoder)
    }

    /// The encoder `candidate` names, if this ffmpeg has it and it opens here.
    fn open_with(candidate: &Candidate, config: EncoderConfig) -> Option<Self> {
        let codec = AVCodec::find_encoder_by_name(candidate.name)?;
        match open_candidate(&codec, candidate, config) {
            Ok((ctx, device)) => {
                let name = candidate.name.to_str().unwrap_or("encoder");
                info!(encoder = name, "video encoder ready");
                Some(Self {
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
                    device,
                    #[cfg(target_os = "linux")]
                    gpu: None,
                })
            }
            Err(e) => {
                debug!(encoder = ?candidate.name, "unusable: {e}");
                None
            }
        }
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
        self.submit(frame, picture.timestamp)
    }

    fn submit(
        &mut self,
        mut frame: AVFrame,
        timestamp: u32,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        frame.set_pts(i64::from(timestamp));
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
        let mut surface = self.surface()?;
        surface.hwframe_transfer_data(frame)?;
        Ok(surface)
    }

    /// A free surface from the encoder's pool.
    fn surface(&mut self) -> Result<AVFrame, EncodeError> {
        let mut surface = AVFrame::new();
        let Some(mut pool) = self.ctx.hw_frames_ctx_mut() else {
            return Err(EncodeError::NoSurfaces);
        };
        pool.get_buffer(&mut surface)?;
        Ok(surface)
    }

    /// Encodes a frame in GPU memory, scaled and converted on the GPU into a surface of the
    /// encoder's pool. Only VA-API encoders take them.
    #[cfg(target_os = "linux")]
    pub fn encode_dmabuf(&mut self, frame: &DmaBuf) -> Result<Vec<EncodedUnit>, EncodeError> {
        let source = (frame.width as usize, frame.height as usize);
        let region = rectangle(fitted(source, (self.width, self.height)));
        self.gpu()?;
        let surface = self.surface()?;
        let vpp = &self.gpu()?.vpp;
        let imported = vpp.import(frame).map_err(EncodeError::Import)?;
        vpp.convert(
            &imported,
            (frame.width, frame.height),
            surface_id(&surface),
            region,
        )
        .map_err(EncodeError::Gpu)?;
        drop(imported);
        self.submit(surface, frame.timestamp)
    }

    /// A copy of a frame in GPU memory scaled on the GPU to at most `width` wide, for
    /// showing the sharer what they send.
    #[cfg(target_os = "linux")]
    pub fn preview_dmabuf(
        &mut self,
        frame: &DmaBuf,
        width: usize,
    ) -> Result<VideoFrame, EncodeError> {
        let source = (frame.width as usize, frame.height as usize);
        let size = share_size(source, (width, width));
        let gpu = self.gpu()?;
        if gpu.previews.as_ref().is_none_or(|(made, _)| *made != size) {
            let pool = surface_pool(&gpu.device, size, PREVIEW_SURFACES)?;
            gpu.previews = Some((size, pool));
        }
        let Some((_, pool)) = gpu.previews.as_mut() else {
            return Err(EncodeError::NoSurfaces);
        };
        let mut surface = AVFrame::new();
        pool.get_buffer(&mut surface)?;
        let imported = gpu.vpp.import(frame).map_err(EncodeError::Import)?;
        let whole = rectangle((0, 0, size.0, size.1));
        gpu.vpp
            .convert(
                &imported,
                (frame.width, frame.height),
                surface_id(&surface),
                whole,
            )
            .map_err(EncodeError::Gpu)?;
        drop(imported);
        let mut downloaded = AVFrame::new();
        downloaded.set_format(ffi::AV_PIX_FMT_NV12);
        downloaded.hwframe_transfer_data(&surface)?;
        let mut preview = VideoFrame::blank(size.0, size.1);
        preview.timestamp = frame.timestamp;
        copy_out(&downloaded, &mut preview);
        Ok(preview)
    }

    #[cfg(target_os = "linux")]
    fn gpu(&mut self) -> Result<&mut GpuInput, EncodeError> {
        if self.gpu.is_none() {
            let device = self.device.as_ref().ok_or(EncodeError::NoGpuInput)?;
            // SAFETY: the display stays initialised while `device` lives, which the Vpp
            // keeps alive beside it.
            let vpp = unsafe { Vpp::new(display(device)) }.map_err(EncodeError::Gpu)?;
            self.gpu = Some(GpuInput {
                vpp,
                device: device.clone(),
                previews: None,
            });
        }
        self.gpu.as_mut().ok_or(EncodeError::NoGpuInput)
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

/// Whether the driver an encoder loads is installed. Only NVENC is checked, by its CUDA
/// library, so machines without NVIDIA's driver skip probing it.
fn has_driver(candidate: &Candidate) -> bool {
    !candidate.name.to_bytes().ends_with(b"_nvenc") || cuda_present()
}

#[cfg(target_os = "linux")]
fn cuda_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| {
        // SAFETY: dlopen takes a NUL-terminated name, and the handle it returns is closed once.
        unsafe {
            let handle = libc::dlopen(c"libcuda.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            !handle.is_null() && libc::dlclose(handle) == 0
        }
    })
}

#[cfg(not(target_os = "linux"))]
fn cuda_present() -> bool {
    true
}

/// The largest rectangle with the aspect ratio of `source`, centred in `target`, as
/// (x, y, width, height), all even.
fn fitted(source: (usize, usize), target: (usize, usize)) -> (usize, usize, usize, usize) {
    let (tw, th) = target;
    let (width, height) = fit_within(source, target);
    (
        ((tw - width) / 2) & !1,
        ((th - height) / 2) & !1,
        width,
        height,
    )
}

/// The VADisplay of a VA-API device context.
#[cfg(target_os = "linux")]
fn display(device: &AVHWDeviceContext) -> vaapi::Display {
    // SAFETY: a VA-API device context's hwctx is an AVVAAPIDeviceContext, which begins with
    // its VADisplay.
    unsafe {
        let context = device.data as *const ffi::AVHWDeviceContext;
        *((*context).hwctx as *const vaapi::Display)
    }
}

#[cfg(target_os = "linux")]
fn rectangle((x, y, width, height): (usize, usize, usize, usize)) -> Rectangle {
    Rectangle {
        x: x as i16,
        y: y as i16,
        width: width as u16,
        height: height as u16,
    }
}

/// The VA surface a VA-API frame stands for.
#[cfg(target_os = "linux")]
fn surface_id(frame: &AVFrame) -> vaapi::SurfaceId {
    frame.data[3] as usize as vaapi::SurfaceId
}

/// Copies a downloaded NV12 frame of the preview's size into it.
#[cfg(target_os = "linux")]
fn copy_out(frame: &AVFrame, into: &mut VideoFrame) {
    let width = into.width;
    let planes = [(0, &mut into.y), (1, &mut into.uv)];
    for (plane, out) in planes {
        let stride = frame.linesize[plane] as usize;
        if frame.data[plane].is_null() || stride < width {
            return;
        }
        for (row, out) in out.chunks_exact_mut(width).enumerate() {
            // SAFETY: the transfer gave the plane at least as many rows of `stride` bytes
            // as `out` has rows of `width`.
            let source =
                unsafe { std::slice::from_raw_parts(frame.data[plane].add(row * stride), width) };
            out.copy_from_slice(source);
        }
    }
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

/// A pool of `count` NV12 VA-API surfaces of `size` on `device`.
fn surface_pool(
    device: &AVHWDeviceContext,
    size: (usize, usize),
    count: i32,
) -> Result<AVHWFramesContext, RsmpegError> {
    let mut pool = device.hwframe_ctx_alloc();
    let surfaces = pool.data();
    surfaces.format = ffi::AV_PIX_FMT_VAAPI;
    surfaces.sw_format = ffi::AV_PIX_FMT_NV12;
    surfaces.width = size.0 as i32;
    surfaces.height = size.1 as i32;
    surfaces.initial_pool_size = count;
    pool.init()?;
    Ok(pool)
}

/// Opens `candidate`, and returns the VA-API device it encodes on, if any.
fn open_candidate(
    codec: &AVCodec,
    candidate: &Candidate,
    config: EncoderConfig,
) -> Result<(AVCodecContext, Option<AVHWDeviceContext>), RsmpegError> {
    let mut ctx = AVCodecContext::new(codec);
    ctx.set_width(config.width as i32);
    ctx.set_height(config.height as i32);
    ctx.set_time_base(ra(1, VIDEO_CLOCK_HZ as i32));
    ctx.set_framerate(ra(config.fps as i32, 1));
    ctx.set_pix_fmt(pixel_format(candidate.input));
    let mut device = None;
    if candidate.vaapi {
        let vaapi = AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_VAAPI, None, None, 0)?;
        let pool = surface_pool(&vaapi, (config.width, config.height), VAAPI_SURFACES)?;
        ctx.set_pix_fmt(ffi::AV_PIX_FMT_VAAPI);
        ctx.set_hw_frames_ctx(pool);
        device = Some(vaapi);
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
    // libvpx runs constant bitrate only when the minimum rate equals the bitrate.
    if candidate.codec == Codec::Vp9 {
        set_option(&mut ctx, c"minrate", &maxrate)?;
    }
    // rsmpeg 0.18 frees an options dictionary twice when opening fails after ffmpeg has
    // replaced it, which crashes the process; the options go straight onto the context.
    ctx.open(None)?;
    Ok((ctx, device))
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

#[cfg(test)]
mod vp9 {
    use super::super::decode::{Decoder, MAX_PIXELS};
    use super::super::frame::{VideoFrame, luma_psnr};
    use super::*;

    #[test]
    fn the_software_fallback_round_trips_through_vp9() {
        let Some(candidate) = LADDER.iter().find(|c| c.name == c"libvpx-vp9") else {
            return;
        };
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            kbps: 3000,
        };
        let Some(mut encoder) = Encoder::open_with(candidate, config) else {
            eprintln!("this ffmpeg has no libvpx; skipping");
            return;
        };
        let mut decoder = Decoder::new(Codec::Vp9, MAX_PIXELS).unwrap();
        let started = std::time::Instant::now();
        let mut shown = Vec::new();
        for n in 0..60u16 {
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            for unit in encoder.encode(&input).unwrap() {
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                    let number = output.read_test_number();
                    let psnr = luma_psnr(&output, &VideoFrame::test_pattern(1280, 720, number, 0));
                    shown.push((number, psnr));
                }
            }
        }
        let per_frame = started.elapsed() / 60;
        eprintln!(
            "libvpx-vp9: {} frames shown, {per_frame:?} a frame",
            shown.len()
        );
        assert!(
            shown.len() >= 55,
            "only {} of 60 frames came out",
            shown.len()
        );
        let worst = shown.iter().map(|(_, p)| *p).fold(f64::INFINITY, f64::min);
        assert!(worst > 25.0, "picture quality {worst:.1} dB");
    }
}

#[cfg(test)]
mod round_trip {
    use super::super::decode::{Decoder, MAX_PIXELS};
    use super::super::frame::{VideoFrame, luma_psnr};
    use super::*;

    /// Encodes 30 frames with `encoder` and decodes them. Returns whether decoding ran
    /// on hardware, or `None` when this machine can't open the encoder.
    fn round_trip(encoder: &CStr, codec: Codec) -> Option<bool> {
        let candidate = LADDER.iter().find(|c| c.name == encoder)?;
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            kbps: 4000,
        };
        let mut encoder = Encoder::open_with(candidate, config)?;
        let mut decoder = Decoder::new(codec, MAX_PIXELS).unwrap();
        let mut shown = Vec::new();
        for n in 0..30u16 {
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            for unit in encoder.encode(&input).unwrap() {
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                    let number = output.read_test_number();
                    let reference = VideoFrame::test_pattern(1280, 720, number, 0);
                    shown.push(luma_psnr(&output, &reference));
                }
            }
        }
        assert!(
            shown.len() >= 25,
            "only {} of 30 frames came out",
            shown.len()
        );
        let worst = shown.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(worst > 25.0, "picture quality {worst:.1} dB");
        Some(decoder.is_hardware())
    }

    #[test]
    fn every_codec_this_machine_encodes_decodes_back() {
        for (encoder, codec) in [
            (c"hevc_videotoolbox", Codec::Hevc),
            (c"h264_videotoolbox", Codec::H264),
            (c"hevc_vaapi", Codec::Hevc),
            (c"h264_vaapi", Codec::H264),
            (c"libvpx-vp9", Codec::Vp9),
        ] {
            if let Some(hardware) = round_trip(encoder, codec) {
                eprintln!("{encoder:?} decoded on hardware: {hardware}");
                if encoder.to_bytes().ends_with(b"videotoolbox") {
                    assert!(
                        hardware,
                        "{encoder:?} encodes on the GPU but decodes in software"
                    );
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gpu_input {
    use super::super::decode::{Decoder, MAX_PIXELS};
    use super::*;

    /// A VA-API encoder, or `None` on machines without one.
    fn vaapi_encoder(width: usize, height: usize) -> Option<Encoder> {
        let config = EncoderConfig {
            width,
            height,
            fps: 30,
            kbps: 4000,
        };
        LADDER
            .iter()
            .filter(|c| c.vaapi)
            .find_map(|c| Encoder::open_with(c, config))
    }

    /// A BGRx picture, red on the left half and blue on the right, in GPU memory shared as
    /// a DMA-BUF.
    fn shared_picture(encoder: &Encoder, width: usize, height: usize) -> DmaBuf {
        let device = encoder.device.as_ref().unwrap();
        let mut pool = device.hwframe_ctx_alloc();
        let surfaces = pool.data();
        surfaces.format = ffi::AV_PIX_FMT_VAAPI;
        surfaces.sw_format = ffi::AV_PIX_FMT_BGR0;
        surfaces.width = width as i32;
        surfaces.height = height as i32;
        surfaces.initial_pool_size = 1;
        pool.init().unwrap();
        let mut picture = AVFrame::new();
        picture.set_width(width as i32);
        picture.set_height(height as i32);
        picture.set_format(ffi::AV_PIX_FMT_BGR0);
        picture.alloc_buffer().unwrap();
        let stride = picture.linesize[0] as usize;
        for row in 0..height {
            // SAFETY: the buffer has `height` rows of `stride` bytes.
            let line = unsafe {
                std::slice::from_raw_parts_mut(picture.data[0].add(row * stride), width * 4)
            };
            for (col, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = if col < width / 2 {
                    [0, 0, 255, 0]
                } else {
                    [255, 0, 0, 0]
                };
            }
        }
        let mut surface = AVFrame::new();
        pool.get_buffer(&mut surface).unwrap();
        surface.hwframe_transfer_data(&picture).unwrap();
        let frame = vaapi::export(display(device), surface_id(&surface)).unwrap();
        eprintln!(
            "shared {}x{} as {:?} with modifier {:#x}",
            frame.width,
            frame.height,
            frame.fourcc.to_le_bytes().map(char::from),
            frame.modifier
        );
        frame
    }

    fn assert_near(what: &str, got: u8, want: u8) {
        assert!(got.abs_diff(want) <= 3, "{what}: {got}, want {want}");
    }

    fn assert_red_then_blue(frame: &VideoFrame, left: usize, right: usize) {
        let row = frame.height / 2;
        let at = |col: usize| {
            let uv = (row / 2) * frame.width + (col & !1);
            (
                frame.y[row * frame.width + col],
                frame.uv[uv],
                frame.uv[uv + 1],
            )
        };
        let (y, u, v) = at(left);
        assert_near("red Y", y, 54);
        assert_near("red U", u, 99);
        assert_near("red V", v, 255);
        let (y, u, v) = at(right);
        assert_near("blue Y", y, 19);
        assert_near("blue U", u, 255);
        assert_near("blue V", v, 116);
    }

    #[test]
    fn previews_are_scaled_and_converted_on_the_gpu() {
        let Some(mut encoder) = vaapi_encoder(1280, 720) else {
            eprintln!("no VA-API encoder here; skipping");
            return;
        };
        let mut frame = shared_picture(&encoder, 2560, 1440);
        frame.timestamp = 1234;
        let preview = encoder.preview_dmabuf(&frame, 960).unwrap();
        assert_eq!(
            (preview.width, preview.height, preview.timestamp),
            (960, 540, 1234)
        );
        assert_red_then_blue(&preview, 240, 720);
    }

    #[test]
    fn frames_in_gpu_memory_are_encoded_with_black_bars() {
        let Some(mut encoder) = vaapi_encoder(1280, 720) else {
            eprintln!("no VA-API encoder here; skipping");
            return;
        };
        let mut frame = shared_picture(&encoder, 1200, 900);
        let mut decoder = Decoder::new(encoder.codec(), MAX_PIXELS).unwrap();
        let mut last = None;
        for n in 0..10 {
            frame.timestamp = n * 3000;
            for unit in encoder.encode_dmabuf(&frame).unwrap() {
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                    last = Some(output);
                }
            }
        }
        let output = last.expect("a decoded picture");
        assert_eq!((output.width, output.height), (1280, 720));
        let luma = |x: usize| output.y[360 * output.width + x];
        assert!(luma(20) < 30, "left bar is {}", luma(20));
        assert!(luma(1260) < 30, "right bar is {}", luma(1260));
        assert_red_then_blue(&output, 400, 880);
    }

    #[test]
    fn a_software_encoder_takes_no_frames_in_gpu_memory() {
        let Some(mut vaapi) = vaapi_encoder(1280, 720) else {
            eprintln!("no VA-API encoder here; skipping");
            return;
        };
        let frame = shared_picture(&vaapi, 640, 360);
        assert!(vaapi.encode_dmabuf(&frame).is_ok());
        let Some(candidate) = LADDER.iter().find(|c| c.name == c"libvpx-vp9") else {
            return;
        };
        let config = EncoderConfig {
            width: 640,
            height: 360,
            fps: 30,
            kbps: 1000,
        };
        let Some(mut vp9) = Encoder::open_with(candidate, config) else {
            return;
        };
        assert!(matches!(
            vp9.encode_dmabuf(&frame),
            Err(EncodeError::NoGpuInput)
        ));
    }
}
