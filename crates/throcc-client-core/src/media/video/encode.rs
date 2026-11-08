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
use super::decode::to_nv12;
#[cfg(target_os = "linux")]
use super::frame::share_size;
use super::frame::{VideoFrame, fit_within};
#[cfg(target_os = "linux")]
use super::vaapi::{self, Rectangle, VaError, Vpp};
#[cfg(target_os = "linux")]
use crate::media::capture::DmaBuf;
#[cfg(target_os = "macos")]
use crate::media::capture::PixelBuffer;

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
    #[error("the hardware encoder has no frame pool")]
    NoSurfaces,
    #[error("this encoder takes no frames in GPU memory")]
    NoGpuInput,
    #[cfg(target_os = "macos")]
    #[error("the captured picture is {0}×{1}, not the encoder's size")]
    OtherSize(usize, usize),
    #[cfg(target_os = "linux")]
    #[error("importing a frame in GPU memory: {0}")]
    Import(VaError),
    #[cfg(target_os = "linux")]
    #[error("GPU video processing: {0}")]
    Gpu(VaError),
    #[cfg(target_os = "linux")]
    #[error("the preview came back in pixel format {0}, which throcc doesn't present")]
    PreviewFormat(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub kbps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    Nv12,
    Yuv420p,
}

/// The family of encoders a rung belongs to, which decides everything but its codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Nvenc,
    MediaFoundation,
    /// Takes VA-API surfaces, uploaded from the NV12 picture or converted on the GPU.
    Vaapi,
    /// Takes CoreVideo pixel buffers, uploaded from the NV12 picture or captured straight
    /// into.
    VideoToolbox,
    Libvpx,
}

impl Backend {
    fn options(self) -> &'static [(&'static CStr, &'static CStr)] {
        match self {
            Backend::Nvenc => NVENC,
            Backend::MediaFoundation => MEDIA_FOUNDATION,
            Backend::Vaapi => VAAPI,
            Backend::VideoToolbox => VIDEOTOOLBOX,
            Backend::Libvpx => LIBVPX,
        }
    }

    fn input(self) -> Input {
        match self {
            Backend::Libvpx => Input::Yuv420p,
            _ => Input::Nv12,
        }
    }

    /// Whether frames go to the encoder in GPU memory, uploaded from the picture.
    fn uploads(self) -> bool {
        matches!(self, Backend::Vaapi | Backend::VideoToolbox)
    }

    /// The hardware API or software library doing the work, as people see it named.
    fn engine(self) -> &'static str {
        match self {
            Backend::Nvenc => "NVENC",
            Backend::MediaFoundation => "Media Foundation",
            Backend::Vaapi => "VA-API",
            Backend::VideoToolbox => "VideoToolbox",
            Backend::Libvpx => "libvpx (software)",
        }
    }
}

/// One rung of the encoder ladder.
struct Candidate {
    name: &'static CStr,
    codec: Codec,
    backend: Backend,
}

const fn rung(name: &'static CStr, codec: Codec, backend: Backend) -> Candidate {
    Candidate {
        name,
        codec,
        backend,
    }
}

const LADDER: &[Candidate] = &[
    rung(c"av1_nvenc", Codec::Av1, Backend::Nvenc),
    rung(c"av1_vaapi", Codec::Av1, Backend::Vaapi),
    rung(c"hevc_nvenc", Codec::Hevc, Backend::Nvenc),
    rung(c"hevc_mf", Codec::Hevc, Backend::MediaFoundation),
    rung(c"hevc_vaapi", Codec::Hevc, Backend::Vaapi),
    rung(c"hevc_videotoolbox", Codec::Hevc, Backend::VideoToolbox),
    rung(c"h264_nvenc", Codec::H264, Backend::Nvenc),
    rung(c"h264_mf", Codec::H264, Backend::MediaFoundation),
    rung(c"h264_vaapi", Codec::H264, Backend::Vaapi),
    rung(c"h264_videotoolbox", Codec::H264, Backend::VideoToolbox),
    rung(c"libvpx-vp9", Codec::Vp9, Backend::Libvpx),
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
    backend: Backend,
    width: usize,
    height: usize,
    idr_next: bool,
    /// The frame pictures are copied into, reused once the encoder lets go of it.
    staging: Option<AVFrame>,
    /// Fits pictures of another size into the encoder's.
    fitter: Option<Fitter>,
    /// The VA-API device of a VA-API encoder.
    #[cfg(target_os = "linux")]
    device: Option<AVHWDeviceContext>,
    #[cfg(target_os = "linux")]
    gpu: Option<GpuInput>,
}

/// Video processing on a VA-API encoder's device, and the previews it draws.
#[cfg(target_os = "linux")]
struct GpuInput {
    vpp: Vpp,
    previews: Previews,
}

/// What encoding a frame in GPU memory gave.
#[cfg(target_os = "linux")]
pub struct GpuEncoded {
    pub units: Vec<EncodedUnit>,
    /// The preview asked for, or why there is none.
    pub preview: Option<Result<VideoFrame, EncodeError>>,
}

/// Small NV12 copies of frames in GPU memory, drawn on the GPU and downloaded.
#[cfg(target_os = "linux")]
struct Previews {
    device: AVHWDeviceContext,
    /// Surfaces of the size last drawn.
    pool: Option<((usize, usize), AVHWFramesContext)>,
}

#[cfg(target_os = "linux")]
impl Previews {
    /// `imported`, captured at `timestamp`, scaled to at most `width` wide.
    fn draw(
        &mut self,
        vpp: &Vpp,
        imported: &vaapi::Surface<'_>,
        timestamp: u32,
        width: usize,
    ) -> Result<VideoFrame, EncodeError> {
        let source = (imported.size.0 as usize, imported.size.1 as usize);
        let size = share_size(source, (width, width));
        let pool = match &mut self.pool {
            Some((made, pool)) if *made == size => pool,
            slot => {
                let pool =
                    surface_pool(&self.device, ffi::AV_PIX_FMT_NV12, size, PREVIEW_SURFACES)?;
                &mut slot.insert((size, pool)).1
            }
        };
        let mut surface = AVFrame::new();
        pool.get_buffer(&mut surface)?;
        let whole = rectangle((0, 0, size.0, size.1));
        vpp.convert(imported, surface_id(&surface), whole)
            .map_err(EncodeError::Gpu)?;
        let mut downloaded = AVFrame::new();
        downloaded.set_format(ffi::AV_PIX_FMT_NV12);
        downloaded.hwframe_transfer_data(&surface)?;
        to_nv12(&downloaded, timestamp, None).ok_or(EncodeError::PreviewFormat(downloaded.format))
    }
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
                #[cfg(not(target_os = "linux"))]
                drop(device);
                let name = candidate.name.to_str().unwrap_or("encoder");
                info!(encoder = name, "video encoder ready");
                Some(Self {
                    ctx,
                    codec: candidate.codec,
                    name,
                    backend: candidate.backend,
                    width: config.width,
                    height: config.height,
                    idr_next: true,
                    staging: None,
                    fitter: None,
                    #[cfg(target_os = "linux")]
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

    /// The hardware API or software library doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        self.backend.engine()
    }

    /// Asks for recovery: an IDR, or for `Refresh`, whatever the encoder's intra refresh
    /// does on its own. Only NVENC has intra refresh; the others treat `Refresh` as `Idr`.
    pub fn recover(&mut self, kind: Recovery) {
        if kind == Recovery::Idr || self.backend != Backend::Nvenc {
            self.idr_next = true;
        }
    }

    pub fn encode(&mut self, picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        let input = self.backend.input();
        let mut frame = match self.staging.take() {
            Some(frame) if frame.is_writable()? => frame,
            _ => new_frame(self.width, self.height, input)?,
        };
        if (picture.width, picture.height) == (self.width, self.height) {
            copy_into(&mut frame, picture, input);
        } else {
            self.fit(picture, &mut frame)?;
        }
        let submitted = if self.backend.uploads() {
            self.upload(&frame)?
        } else {
            frame.clone()
        };
        self.staging = Some(frame);
        self.submit(submitted, picture.timestamp)
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

    /// Copies an NV12 frame into a surface from the encoder's pool.
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
    /// encoder's pool. With `preview_width`, also draws a preview at most that wide from the
    /// same import. Only VA-API encoders take them.
    #[cfg(target_os = "linux")]
    pub fn encode_dmabuf(
        &mut self,
        frame: &DmaBuf,
        preview_width: Option<usize>,
    ) -> Result<GpuEncoded, EncodeError> {
        let source = (frame.width as usize, frame.height as usize);
        let region = rectangle(fitted(source, (self.width, self.height)));
        // First, so an encoder that takes no GPU input says so rather than lacking surfaces.
        self.gpu()?;
        let surface = self.surface()?;
        let GpuInput { vpp, previews } = self.gpu()?;
        let imported = vpp.import(frame).map_err(EncodeError::Import)?;
        vpp.convert(&imported, surface_id(&surface), region)
            .map_err(EncodeError::Gpu)?;
        let preview =
            preview_width.map(|width| previews.draw(vpp, &imported, frame.timestamp, width));
        drop(imported);
        let units = self.submit(surface, frame.timestamp)?;
        Ok(GpuEncoded { units, preview })
    }

    /// Whether [`Encoder::encode_pixel_buffer`] takes captured pixel buffers.
    #[cfg(target_os = "macos")]
    pub fn takes_pixel_buffers(&self) -> bool {
        self.backend == Backend::VideoToolbox
    }

    /// Encodes a captured picture by handing its pixel buffer to VideoToolbox, uncopied.
    /// Only VideoToolbox encoders take them, and only at the encoder's size.
    #[cfg(target_os = "macos")]
    pub fn encode_pixel_buffer(
        &mut self,
        picture: &PixelBuffer,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        if !self.takes_pixel_buffers() {
            return Err(EncodeError::NoGpuInput);
        }
        let size = (picture.buffer.width(), picture.buffer.height());
        if size != (self.width, self.height) {
            return Err(EncodeError::OtherSize(size.0, size.1));
        }
        let mut frame = AVFrame::new();
        frame.set_format(ffi::AV_PIX_FMT_VIDEOTOOLBOX);
        frame.set_width(size.0 as i32);
        frame.set_height(size.1 as i32);
        let raw = picture.buffer.as_ptr().cast::<u8>();
        let held = Box::into_raw(Box::new(picture.buffer.clone()));
        // SAFETY: the AVBufferRef owns `held` and frees it once, in `release_pixel_buffer`.
        let buffer =
            unsafe { ffi::av_buffer_create(raw, 1, Some(release_pixel_buffer), held.cast(), 0) };
        if buffer.is_null() {
            // SAFETY: `held` came from Box::into_raw above and nothing else took it.
            drop(unsafe { Box::from_raw(held) });
            return Err(RsmpegError::AVError(ffi::AVERROR(ffi::ENOMEM)).into());
        }
        // SAFETY: a VideoToolbox frame carries its CVPixelBufferRef in data[3], kept alive by
        // buf[0], which the frame owns from here.
        unsafe {
            let frame = frame.as_mut_ptr();
            (*frame).buf[0] = buffer;
            (*frame).data[3] = raw;
        }
        self.submit(frame, picture.timestamp)
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
                previews: Previews {
                    device: device.clone(),
                    pool: None,
                },
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
        let fitter = match &mut self.fitter {
            Some(fitter) if fitter.source_size == source_size => fitter,
            slot => slot.insert(Fitter::new(
                source_size,
                (self.width, self.height),
                self.backend.input(),
            )?),
        };
        fitter.fit(picture, frame)
    }
}

/// Scales NV12 pictures of one size into frames of another, keeping the aspect ratio,
/// with black bars around them.
struct Fitter {
    source_size: (usize, usize),
    input: Input,
    /// Where the picture lands in the target frame, as (x, y, width, height).
    rectangle: (usize, usize, usize, usize),
    scaler: SwsContext,
    /// sws reads and writes whole SIMD words past the end of rows, which only buffers
    /// FFmpeg allocated are padded for. So it scales from a copy of the picture into a
    /// frame of the rectangle's size, which is then copied into place.
    source: AVFrame,
    scaled: AVFrame,
}

impl Fitter {
    fn new(
        source_size: (usize, usize),
        target_size: (usize, usize),
        input: Input,
    ) -> Result<Self, EncodeError> {
        let rectangle = fitted(source_size, target_size);
        let (_, _, width, height) = rectangle;
        let scaler = SwsContext::get_context(
            source_size.0 as i32,
            source_size.1 as i32,
            ffi::AV_PIX_FMT_NV12,
            width as i32,
            height as i32,
            pixel_format(input),
            ffi::SWS_BILINEAR,
            None,
            None,
            None,
        )
        .ok_or(EncodeError::Scaler)?;
        let mut source = new_frame(source_size.0, source_size.1, Input::Nv12)?;
        // An odd-width picture lacks the V of its last chroma column; it stays neutral.
        fill_black(&mut source, Input::Nv12);
        Ok(Self {
            source_size,
            input,
            rectangle,
            scaler,
            source,
            scaled: new_frame(width, height, input)?,
        })
    }

    /// Scales `picture`, of the fitter's source size, into `frame`, of its target size.
    fn fit(&mut self, picture: &VideoFrame, frame: &mut AVFrame) -> Result<(), EncodeError> {
        copy_into(&mut self.source, picture, Input::Nv12);
        let rows = self.source_size.1 as i32;
        self.scaler
            .scale_frame(&self.source, 0, rows, &mut self.scaled)?;
        for area in plane_areas(frame, self.input, self.rectangle) {
            let (from_stride, to_stride) = (
                self.scaled.linesize[area.plane] as usize,
                frame.linesize[area.plane] as usize,
            );
            for row in 0..area.rows {
                // SAFETY: `plane_areas` keeps every row within the plane's width and every
                // row index within its height, and `scaled` holds the rectangle's size in
                // this plane's layout.
                unsafe {
                    let line = frame.data[area.plane].add(row * to_stride);
                    if !(area.y..area.y + area.height).contains(&row) {
                        std::ptr::write_bytes(line, area.black, area.row_bytes);
                        continue;
                    }
                    std::ptr::write_bytes(line, area.black, area.x);
                    std::ptr::copy_nonoverlapping(
                        self.scaled.data[area.plane].add((row - area.y) * from_stride),
                        line.add(area.x),
                        area.width,
                    );
                    let right = area.x + area.width;
                    std::ptr::write_bytes(line.add(right), area.black, area.row_bytes - right);
                }
            }
        }
        Ok(())
    }
}

/// A frame's plane, and where a fitted rectangle lies in it, in bytes and rows.
struct PlaneArea {
    plane: usize,
    /// The plane's value for full-range black.
    black: u8,
    row_bytes: usize,
    rows: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// Each plane of `frame` with the part `rectangle` covers. `fitted` makes the rectangle's
/// offsets and size even, so it halves exactly in the chroma planes.
fn plane_areas(
    frame: &AVFrame,
    input: Input,
    (x, y, width, height): (usize, usize, usize, usize),
) -> Vec<PlaneArea> {
    let (frame_width, frame_height) = (frame.width as usize, frame.height as usize);
    let luma = PlaneArea {
        plane: 0,
        black: 0,
        row_bytes: frame_width,
        rows: frame_height,
        x,
        y,
        width,
        height,
    };
    let chroma = |plane, row_bytes, x, width| PlaneArea {
        plane,
        black: 128,
        row_bytes,
        rows: frame_height.div_ceil(2),
        x,
        y: y / 2,
        width,
        height: height / 2,
    };
    match input {
        Input::Nv12 => vec![luma, chroma(1, frame_width.div_ceil(2) * 2, x, width)],
        Input::Yuv420p => {
            let half = frame_width.div_ceil(2);
            vec![
                luma,
                chroma(1, half, x / 2, width / 2),
                chroma(2, half, x / 2, width / 2),
            ]
        }
    }
}

/// Whether the first encoder [`Encoder::open`] tries here takes frames in GPU memory, so
/// the capture can offer them before the encoder exists.
#[cfg(target_os = "linux")]
pub fn tries_gpu_input_first() -> bool {
    first_tried(|candidate| {
        has_driver(candidate) && AVCodec::find_encoder_by_name(candidate.name).is_some()
    })
    .is_some_and(|candidate| candidate.backend == Backend::Vaapi)
}

#[cfg(target_os = "linux")]
fn first_tried(available: impl Fn(&Candidate) -> bool) -> Option<&'static Candidate> {
    LADDER.iter().find(|candidate| available(candidate))
}

/// Whether the driver an encoder loads is installed. Only NVENC is checked, by its CUDA
/// library, so machines without NVIDIA's driver skip probing it.
fn has_driver(candidate: &Candidate) -> bool {
    candidate.backend != Backend::Nvenc || cuda_present()
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
fn fill_black(frame: &mut AVFrame, input: Input) {
    let height = frame.height as usize;
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

/// A pool of `count` VA-API surfaces of `size` holding `format` pictures, on `device`.
fn surface_pool(
    device: &AVHWDeviceContext,
    format: ffi::AVPixelFormat,
    size: (usize, usize),
    count: i32,
) -> Result<AVHWFramesContext, RsmpegError> {
    let mut pool = device.hwframe_ctx_alloc();
    let surfaces = pool.data();
    surfaces.format = ffi::AV_PIX_FMT_VAAPI;
    surfaces.sw_format = format;
    surfaces.width = size.0 as i32;
    surfaces.height = size.1 as i32;
    surfaces.initial_pool_size = count;
    pool.init()?;
    Ok(pool)
}

/// Frees the pixel buffer held by an AVBufferRef from [`Encoder::encode_pixel_buffer`].
#[cfg(target_os = "macos")]
unsafe extern "C" fn release_pixel_buffer(held: *mut std::ffi::c_void, _data: *mut u8) {
    // SAFETY: `held` is the boxed CVPixelBuffer the AVBufferRef was created with.
    drop(unsafe { Box::from_raw(held.cast::<screencapturekit::cv::CVPixelBuffer>()) });
}

/// A pool of full-range NV12 CoreVideo pixel buffers of `size`, as ScreenCaptureKit fills.
fn pixel_buffer_pool(size: (usize, usize)) -> Result<AVHWFramesContext, RsmpegError> {
    let device = AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, None, None, 0)?;
    let mut pool = device.hwframe_ctx_alloc();
    let buffers = pool.data();
    buffers.format = ffi::AV_PIX_FMT_VIDEOTOOLBOX;
    buffers.sw_format = ffi::AV_PIX_FMT_NV12;
    buffers.width = size.0 as i32;
    buffers.height = size.1 as i32;
    // SAFETY: a VideoToolbox frames context's hwctx is an AVVTFramesContext, whose only
    // field is its colour range.
    unsafe { *(buffers.hwctx as *mut ffi::AVColorRange) = ffi::AVCOL_RANGE_JPEG };
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
    ctx.set_pix_fmt(pixel_format(candidate.backend.input()));
    let mut device = None;
    match candidate.backend {
        Backend::Vaapi => {
            let vaapi = AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_VAAPI, None, None, 0)?;
            let size = (config.width, config.height);
            let pool = surface_pool(&vaapi, ffi::AV_PIX_FMT_NV12, size, VAAPI_SURFACES)?;
            ctx.set_pix_fmt(ffi::AV_PIX_FMT_VAAPI);
            ctx.set_hw_frames_ctx(pool);
            device = Some(vaapi);
        }
        Backend::VideoToolbox => {
            let pool = pixel_buffer_pool((config.width, config.height))?;
            ctx.set_pix_fmt(ffi::AV_PIX_FMT_VIDEOTOOLBOX);
            // SAFETY: `ctx` owns a live context, and sw_pix_fmt is a plain field of it.
            unsafe { (*ctx.as_mut_ptr()).sw_pix_fmt = ffi::AV_PIX_FMT_NV12 };
            ctx.set_hw_frames_ctx(pool);
        }
        Backend::Nvenc | Backend::MediaFoundation | Backend::Libvpx => {}
    }
    let bitrate = i64::from(config.kbps) * 1000;
    ctx.set_bit_rate(bitrate);
    ctx.set_max_b_frames(0);
    ctx.set_gop_size(config.fps as i32 * GOP_SECONDS);
    // VideoToolbox's low-latency mode holds 2 to 4 frames when the height isn't a multiple
    // of 16, where its normal mode holds one.
    if candidate.backend != Backend::VideoToolbox || config.height.is_multiple_of(16) {
        ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
    }
    let maxrate = cstring(&bitrate.to_string());
    let bufsize = cstring(&(bitrate * 2 / i64::from(config.fps.max(1))).to_string());
    let common = [
        (c"maxrate", maxrate.as_c_str()),
        (c"bufsize", bufsize.as_c_str()),
        (c"color_range", c"pc"),
        (c"colorspace", c"bt709"),
    ];
    for (key, value) in common
        .into_iter()
        .chain(candidate.backend.options().iter().copied())
    {
        set_option(&mut ctx, key, value)?;
    }
    // libvpx runs constant bitrate only when the minimum rate equals the bitrate.
    if candidate.backend == Backend::Libvpx {
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

/// Copies an NV12 picture into a frame of the encoder's input format, allocated at the
/// picture's size.
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

/// The ladder's rung for the encoder called `name`.
#[cfg(test)]
fn candidate(name: &CStr) -> Option<&'static Candidate> {
    LADDER.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Odd and unaligned sizes too: run under valgrind, this catches sws reading past the
    /// picture's planes.
    #[test]
    fn pictures_of_any_size_are_fitted_inside_black_bars() {
        let target = (1280, 720);
        let sizes = [
            (641, 359),
            (1000, 1000),
            (17, 9),
            (3, 3),
            (1279, 721),
            (2560, 1440),
        ];
        for input in [Input::Nv12, Input::Yuv420p] {
            for source in sizes {
                let mut picture = VideoFrame::blank(source.0, source.1);
                picture.y.fill(200);
                let mut fitter = Fitter::new(source, target, input).unwrap();
                let mut frame = new_frame(target.0, target.1, input).unwrap();
                for (plane, rows) in [(0, target.1), (1, target.1 / 2), (2, target.1 / 2)] {
                    if !frame.data[plane].is_null() {
                        let bytes = frame.linesize[plane] as usize * rows;
                        // SAFETY: each plane holds `rows` rows of `linesize` bytes.
                        unsafe { std::ptr::write_bytes(frame.data[plane], 77, bytes) };
                    }
                }
                fitter.fit(&picture, &mut frame).unwrap();
                fitter.fit(&picture, &mut frame).unwrap();

                let (x, y, width, height) = fitted(source, target);
                let sample = |plane: usize, col: usize, row: usize| {
                    // SAFETY: every (col, row) asked for is inside the plane.
                    unsafe { *frame.data[plane].add(row * frame.linesize[plane] as usize + col) }
                };
                let luma = |col, row| sample(0, col, row);
                let (centre_x, centre_y) = (x + width / 2, y + height / 2);
                let centre = luma(centre_x, centre_y);
                assert!(centre.abs_diff(200) < 4, "{input:?} {source:?}: {centre}");
                let chroma = sample(1, centre_x / 2, centre_y / 2);
                assert!(
                    chroma.abs_diff(128) < 4,
                    "{input:?} {source:?}: chroma {chroma}"
                );
                let (right, bottom) = (x + width, y + height);
                for (col, row) in [(x.wrapping_sub(1), centre_y), (right, centre_y)]
                    .into_iter()
                    .chain([(centre_x, y.wrapping_sub(1)), (centre_x, bottom)])
                    .filter(|&(col, row)| col < target.0 && row < target.1)
                {
                    assert_eq!(luma(col, row), 0, "{input:?} {source:?} at {col},{row}");
                }
                let in_picture = |&(col, row): &(usize, usize)| {
                    col >= x && col < right && row >= y && row < bottom
                };
                for (col, row) in [(0, 0), (target.0 - 1, target.1 - 1)]
                    .into_iter()
                    .filter(|corner| !in_picture(corner))
                {
                    assert_eq!(luma(col, row), 0, "{input:?} {source:?} corner");
                    let chroma_col = match input {
                        Input::Nv12 => col / 2 * 2,
                        Input::Yuv420p => col / 2,
                    };
                    let chroma = sample(1, chroma_col, row / 2);
                    assert_eq!(chroma, 128, "{input:?} {source:?} corner chroma");
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod first_tried {
    use super::*;

    fn first_of(names: &[&CStr]) -> Option<&'static CStr> {
        first_tried(|candidate| names.contains(&candidate.name)).map(|candidate| candidate.name)
    }

    #[test]
    fn nvenc_comes_before_vaapi_and_vaapi_before_software() {
        let everything = [c"av1_nvenc", c"av1_vaapi", c"h264_vaapi", c"libvpx-vp9"];
        assert_eq!(first_of(&everything), Some(c"av1_nvenc"));
        assert_eq!(first_of(&everything[1..]), Some(c"av1_vaapi"));
        assert_eq!(
            first_of(&[c"h264_vaapi", c"libvpx-vp9"]),
            Some(c"h264_vaapi")
        );
        assert_eq!(first_of(&[c"libvpx-vp9"]), Some(c"libvpx-vp9"));
        assert_eq!(first_of(&[]), None);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod failed_open {
    use super::*;

    #[test]
    fn an_encoder_that_fails_to_open_is_cleaned_up_safely() {
        let (Some(codec), Some(candidate)) = (
            AVCodec::find_encoder_by_name(c"hevc_videotoolbox"),
            candidate(c"hevc_videotoolbox"),
        ) else {
            eprintln!("skipped: no VideoToolbox HEVC encoder in this ffmpeg");
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
mod round_trip {
    use super::super::decode::{Decoder, MaxSize};
    use super::super::frame::{VideoFrame, luma_psnr};
    use super::*;

    /// Encodes 30 frames with `encoder` and decodes them. Returns whether decoding ran
    /// on hardware, or `None` when this machine can't open the encoder.
    fn round_trip(encoder: &CStr, codec: Codec) -> Option<bool> {
        let candidate = candidate(encoder)?;
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            kbps: 4000,
        };
        let mut encoder = Encoder::open_with(candidate, config)?;
        let mut decoder = Decoder::new(codec, MaxSize::ANY).unwrap();
        let mut shown = Vec::new();
        for n in 0..30u16 {
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            for unit in encoder.encode(&input).unwrap() {
                if let Some(output) = decoder
                    .decode(&unit.data, unit.timestamp)
                    .unwrap()
                    .map(|frame| frame.copied_to_memory())
                {
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

    /// The rate `candidate` sends at for 6 Mbps of 1080p60. `noisy` makes every frame
    /// expensive, and `idr_every_second` asks for a keyframe each second.
    fn sent_kbps(candidate: &Candidate, noisy: bool, idr_every_second: bool) -> Option<u32> {
        let config = EncoderConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            kbps: 6000,
        };
        let seconds = 3;
        let mut encoder = Encoder::open_with(candidate, config)?;
        let mut bytes = 0;
        let mut noise = 0x2545_f491_4f6c_dd1du64;
        for n in 0..config.fps * (seconds + 1) {
            let mut input = VideoFrame::test_pattern(1920, 1080, n as u16, n * 1500);
            if noisy {
                for samples in input.y.as_chunks_mut::<8>().0 {
                    noise ^= noise << 13;
                    noise ^= noise >> 7;
                    noise ^= noise << 17;
                    // Adds up to 63 to each of the eight samples, each wrapping on its own:
                    // the low seven bits can't carry into the next sample.
                    let picture = u64::from_le_bytes(*samples);
                    let sum = ((picture & 0x7f7f_7f7f_7f7f_7f7f) + (noise & 0x3f3f_3f3f_3f3f_3f3f))
                        ^ (picture & 0x8080_8080_8080_8080);
                    *samples = sum.to_le_bytes();
                }
            }
            if idr_every_second && n % config.fps == config.fps / 2 {
                encoder.recover(Recovery::Idr);
            }
            let units = encoder.encode(&input).unwrap();
            if n >= config.fps {
                bytes += units.iter().map(|u| u.data.len()).sum::<usize>();
            }
        }
        Some(bytes as u32 * 8 / 1000 / seconds)
    }

    #[test]
    fn every_encoder_this_machine_opens_keeps_to_its_bitrate() {
        for candidate in LADDER {
            for (what, noisy, idr_every_second) in
                [("noise", true, false), ("a keyframe a second", false, true)]
            {
                let Some(kbps) = sent_kbps(candidate, noisy, idr_every_second) else {
                    eprintln!("skipped: {:?} doesn't open here", candidate.name);
                    continue;
                };
                eprintln!("{:?} with {what}: {kbps} kbps for 6000 set", candidate.name);
                assert!(
                    kbps <= 7500,
                    "{:?} with {what} sends {kbps} kbps for 6000 set",
                    candidate.name
                );
            }
        }
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
            let Some(hardware) = round_trip(encoder, codec) else {
                eprintln!("skipped: {encoder:?} doesn't open here");
                continue;
            };
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

#[cfg(all(test, target_os = "linux"))]
mod gpu_input {
    use super::super::decode::{Decoder, MaxSize};
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
            .filter(|c| c.backend == Backend::Vaapi)
            .find_map(|c| Encoder::open_with(c, config))
    }

    /// A BGRx picture, red on the left half and blue on the right, in GPU memory shared as
    /// a DMA-BUF.
    fn shared_picture(encoder: &Encoder, width: usize, height: usize) -> DmaBuf {
        let device = encoder.device.as_ref().unwrap();
        let mut pool = surface_pool(device, ffi::AV_PIX_FMT_BGR0, (width, height), 1).unwrap();
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
            eprintln!("skipped: no VA-API encoder here");
            return;
        };
        let mut frame = shared_picture(&encoder, 2560, 1440);
        frame.timestamp = 1234;
        let encoded = encoder.encode_dmabuf(&frame, Some(960)).unwrap();
        let preview = encoded.preview.unwrap().unwrap();
        assert_eq!(
            (preview.width, preview.height, preview.timestamp),
            (960, 540, 1234)
        );
        assert_red_then_blue(&preview, 240, 720);
    }

    #[test]
    fn frames_in_gpu_memory_are_encoded_with_black_bars() {
        let Some(mut encoder) = vaapi_encoder(1280, 720) else {
            eprintln!("skipped: no VA-API encoder here");
            return;
        };
        let mut frame = shared_picture(&encoder, 1200, 900);
        let mut decoder = Decoder::new(encoder.codec(), MaxSize::ANY).unwrap();
        let mut last = None;
        for n in 0..10 {
            frame.timestamp = n * 3000;
            for unit in encoder.encode_dmabuf(&frame, None).unwrap().units {
                if let Some(output) = decoder
                    .decode(&unit.data, unit.timestamp)
                    .unwrap()
                    .map(|frame| frame.copied_to_memory())
                {
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
            eprintln!("skipped: no VA-API encoder here");
            return;
        };
        let frame = shared_picture(&vaapi, 640, 360);
        assert!(vaapi.encode_dmabuf(&frame, None).is_ok());
        let config = EncoderConfig {
            width: 640,
            height: 360,
            fps: 30,
            kbps: 1000,
        };
        let Some(mut vp9) = candidate(c"libvpx-vp9").and_then(|c| Encoder::open_with(c, config))
        else {
            eprintln!("skipped: no libvpx-vp9 encoder here");
            return;
        };
        assert!(matches!(
            vp9.encode_dmabuf(&frame, None),
            Err(EncodeError::NoGpuInput)
        ));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod pixel_buffers {
    use super::super::Frame;
    use super::super::decode::{Decoder, MaxSize};
    use super::super::frame::luma_psnr;
    use super::*;

    fn opened(name: &CStr) -> Option<Encoder> {
        let candidate = candidate(name)?;
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            kbps: 4000,
        };
        Encoder::open_with(candidate, config)
    }

    #[test]
    fn captured_pixel_buffers_encode_uncopied_and_decode_back_into_pixel_buffers() {
        let Some(mut encoder) = opened(c"hevc_videotoolbox") else {
            eprintln!("skipped: no VideoToolbox HEVC encoder here");
            return;
        };
        let mut decoder = Decoder::new(Codec::Hevc, MaxSize::ANY).unwrap();
        let mut shown = Vec::new();
        let mut in_gpu_memory = true;
        for n in 0..30u16 {
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            let picture = PixelBuffer::holding(&input);
            for unit in encoder.encode_pixel_buffer(&picture).unwrap() {
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                    in_gpu_memory &= matches!(output, Frame::PixelBuffer(_));
                    let output = output.copied_to_memory();
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
        assert!(
            in_gpu_memory || !decoder.is_hardware(),
            "VideoToolbox pictures were copied out of their pixel buffers"
        );
    }

    #[test]
    fn a_corrupt_unit_keeps_hardware_decoding_until_the_next_keyframe() {
        let Some(mut encoder) = opened(c"hevc_videotoolbox") else {
            eprintln!("skipped: no VideoToolbox HEVC encoder here");
            return;
        };
        let mut decoder = Decoder::new(Codec::Hevc, MaxSize::ANY).unwrap();
        if !decoder.is_hardware() {
            eprintln!("skipped: no hardware HEVC decoder here");
            return;
        }
        let (mut failed, mut asked, mut shown_after) = (false, false, 0);
        for n in 0..40u16 {
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            for mut unit in encoder.encode(&input).unwrap() {
                if n == 15 {
                    let mut bytes = unit.data.to_vec();
                    let len = bytes.len();
                    for byte in &mut bytes[len / 4..len * 3 / 4] {
                        *byte ^= 0x5a;
                    }
                    unit.data = bytes.into();
                }
                match decoder.decode(&unit.data, unit.timestamp) {
                    Ok(Some(_)) => {
                        asked = false;
                        shown_after += usize::from(failed);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        failed = true;
                        if !asked {
                            encoder.recover(Recovery::Idr);
                            asked = true;
                        }
                    }
                }
            }
        }
        assert!(failed, "the corrupt unit decoded");
        assert!(decoder.is_hardware(), "fell back to software");
        assert!(
            shown_after >= 15,
            "only {shown_after} pictures after the corrupt unit"
        );
    }

    #[test]
    fn a_picture_of_another_size_or_a_software_encoder_is_refused() {
        let picture = PixelBuffer::holding(&VideoFrame::test_pattern(640, 360, 0, 0));
        if let Some(mut videotoolbox) = opened(c"h264_videotoolbox") {
            assert!(matches!(
                videotoolbox.encode_pixel_buffer(&picture),
                Err(EncodeError::OtherSize(640, 360))
            ));
        }
        if let Some(mut vp9) = opened(c"libvpx-vp9") {
            assert!(!vp9.takes_pixel_buffers());
            assert!(matches!(
                vp9.encode_pixel_buffer(&picture),
                Err(EncodeError::NoGpuInput)
            ));
        }
    }
}
