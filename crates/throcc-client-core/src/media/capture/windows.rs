#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
    GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCapturePicker, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_POINTER, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_RGBA,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::UI::Shell::IInitializeWithWindow;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{IInspectable, Interface, Ref};

use super::{Capture, CaptureConfig, CaptureError};
use crate::media::video::frame::VideoFrame;
use throcc_proto::frame::VIDEO_CLOCK_HZ;

/// Captured frames waiting for the share thread. A full ring drops the newest frame.
const RING: usize = 2;
/// Textures in the frame pool: every ring slot, plus one being converted.
const POOL_BUFFERS: i32 = RING as i32 + 1;
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const TIME_SPAN_HZ: i128 = 10_000_000;

pub struct WinCapture {
    frames: rtrb::Consumer<(Direct3D11CaptureFrame, u32)>,
    dropped: Arc<AtomicU64>,
    converter: Converter,
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    pool_size: SizeInt32,
    session: GraphicsCaptureSession,
    arrived: i64,
}

// SAFETY: `device` only wraps the multithread-protected D3D11 device, and is used only to
// recreate the free-threaded frame pool.
unsafe impl Send for WinCapture {}

pub fn start(config: CaptureConfig) -> Result<WinCapture, CaptureError> {
    capture(&pick()?, config).map_err(failed)
}

fn capture(item: &GraphicsCaptureItem, config: CaptureConfig) -> windows::core::Result<WinCapture> {
    let d3d = d3d_device()?;
    let dxgi: IDXGIDevice = d3d.cast()?;
    // SAFETY: `dxgi` is a live DXGI device.
    let device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    let size = item.Size()?;
    let output = (config.width as u32, config.height as u32);
    let converter = Converter::new(
        d3d,
        (size.Width as u32, size.Height as u32),
        output,
        config.fps,
    )?;

    let pool =
        Direct3D11CaptureFramePool::CreateFreeThreaded(&device, POOL_FORMAT, POOL_BUFFERS, size)?;
    let session = pool.CreateCaptureSession(item)?;
    GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
        .and_then(|request| request.join())
        .ok();
    session.SetIsBorderRequired(false).ok();
    session.SetIsCursorCaptureEnabled(true).ok();
    session
        .SetMinUpdateInterval(TimeSpan {
            Duration: (TIME_SPAN_HZ / i128::from(config.fps.max(1))) as i64,
        })
        .ok();

    let (producer, frames) = rtrb::RingBuffer::new(RING);
    // FrameArrived can run on any thread-pool thread; `try_lock` never waits, and a
    // collision counts as a drop.
    let feed = Mutex::new(Feed {
        frames: producer,
        pacer: Pacer::new(config.fps),
    });
    let dropped = Arc::new(AtomicU64::new(0));
    let callback_dropped = dropped.clone();
    let arrived = pool.FrameArrived(&TypedEventHandler::<
        Direct3D11CaptureFramePool,
        IInspectable,
    >::new(
        move |pool: Ref<Direct3D11CaptureFramePool>, _| {
            let Some(frame) = pool.as_ref().and_then(|pool| pool.TryGetNextFrame().ok()) else {
                return Ok(());
            };
            let Ok(time) = frame.SystemRelativeTime() else {
                return Ok(());
            };
            let ticks =
                (i128::from(time.Duration) * i128::from(VIDEO_CLOCK_HZ) / TIME_SPAN_HZ) as i64;
            let queued = feed
                .try_lock()
                .is_ok_and(|mut feed| feed.offer(frame, ticks));
            if !queued {
                callback_dropped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        },
    ))?;
    session.StartCapture()?;
    Ok(WinCapture {
        frames,
        dropped,
        converter,
        device,
        pool,
        pool_size: size,
        session,
        arrived,
    })
}

/// Shows `GraphicsCapturePicker` over the throcc window and waits for the person's choice.
fn pick() -> Result<GraphicsCaptureItem, CaptureError> {
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return Err(CaptureError::Unsupported);
    }
    let window = throcc_window()?;
    let picker = GraphicsCapturePicker::new().map_err(failed)?;
    let init: IInitializeWithWindow = picker.cast().map_err(failed)?;
    // SAFETY: `window` is a top-level window of this process.
    unsafe { init.Initialize(window) }.map_err(failed)?;
    match picker.PickSingleItemAsync().and_then(|pick| pick.join()) {
        Ok(item) => Ok(item),
        Err(e) if e.code().is_ok() => Err(CaptureError::Cancelled),
        Err(e) => Err(failed(e)),
    }
}

fn throcc_window() -> Result<HWND, CaptureError> {
    let mut owner = 0;
    // SAFETY: both calls only read window manager state into a local.
    let window = unsafe {
        let window = GetForegroundWindow();
        GetWindowThreadProcessId(window, Some(&mut owner));
        window
    };
    if window.is_invalid() || owner != std::process::id() {
        return Err(CaptureError::Failed(
            "the throcc window has to be in front to share".into(),
        ));
    }
    Ok(window)
}

fn d3d_device() -> windows::core::Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: `device` outlives the call that fills it.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }?;
    let device: ID3D11Device = device.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
    let multithread: ID3D11Multithread = device.cast()?;
    // SAFETY: plain setter on a live device.
    let _was_protected = unsafe { multithread.SetMultithreadProtected(true) };
    Ok(device)
}

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

struct Feed {
    frames: rtrb::Producer<(Direct3D11CaptureFrame, u32)>,
    pacer: Pacer,
}

impl Feed {
    /// Queues `frame` if the pacer admits it. False only when the ring was full.
    fn offer(&mut self, frame: Direct3D11CaptureFrame, ticks: i64) -> bool {
        match self.pacer.admit(ticks) {
            Some(timestamp) => self.frames.push((frame, timestamp)).is_ok(),
            None => true,
        }
    }
}

/// Thins frames to the share's rate on average, and stamps each admitted frame with the
/// 90 kHz ticks since the first.
struct Pacer {
    start: Option<i64>,
    due: i64,
    interval: i64,
}

impl Pacer {
    fn new(fps: u32) -> Self {
        Self {
            start: None,
            due: 0,
            interval: i64::from(VIDEO_CLOCK_HZ / fps.max(1)),
        }
    }

    fn admit(&mut self, ticks: i64) -> Option<u32> {
        let since = ticks - *self.start.get_or_insert(ticks);
        if since < self.due - self.interval / 4 {
            return None;
        }
        self.due = if since - self.due > self.interval {
            since + self.interval
        } else {
            self.due + self.interval
        };
        Some(since as u32)
    }
}

impl Capture for WinCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        let Ok((frame, timestamp)) = self.frames.pop() else {
            return false;
        };
        match self.convert(frame, into) {
            Ok(()) => {
                into.timestamp = timestamp;
                true
            }
            Err(e) => {
                tracing::debug!("dropping a captured frame: {e}");
                false
            }
        }
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }
}

impl WinCapture {
    fn convert(
        &mut self,
        frame: Direct3D11CaptureFrame,
        into: &mut VideoFrame,
    ) -> windows::core::Result<()> {
        let content = frame.ContentSize()?;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface of a capture frame is a D3D11 texture.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` outlives the call that fills it.
        unsafe { texture.GetDesc(&mut desc) };
        let input = (desc.Width, desc.Height);
        if self.converter.input != input {
            self.converter = Converter::new(
                self.converter.device.clone(),
                input,
                self.converter.output,
                self.converter.fps,
            )?;
        }
        let shown = (
            (content.Width.max(0) as u32).min(input.0),
            (content.Height.max(0) as u32).min(input.1),
        );
        self.converter.process(&texture, shown)?;
        self.converter.read_back(into)?;
        frame.Close().ok();

        if content != self.pool_size && content.Width > 0 && content.Height > 0 {
            self.pool
                .Recreate(&self.device, POOL_FORMAT, POOL_BUFFERS, content)?;
            self.pool_size = content;
        }
        Ok(())
    }
}

/// Scales a BGRA texture into NV12 at the share's size on the GPU, then reads it back.
struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    target: ID3D11Texture2D,
    target_view: ID3D11VideoProcessorOutputView,
    staging: ID3D11Texture2D,
    input: (u32, u32),
    output: (u32, u32),
    fps: u32,
}

impl Converter {
    fn new(
        device: ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
    ) -> windows::core::Result<Self> {
        // SAFETY: plain getter on a live device.
        let context = unsafe { device.GetImmediateContext() }?;
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext1 = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let target = nv12_texture(&device, output, D3D11_USAGE_DEFAULT, true)?;
        let staging = nv12_texture(&device, output, D3D11_USAGE_STAGING, false)?;
        let view = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                RGBA: D3D11_VIDEO_COLOR_RGBA {
                    R: 0.0,
                    G: 0.0,
                    B: 0.0,
                    A: 1.0,
                },
            },
        };
        let mut target_view = None;
        // SAFETY: every pointer passed points at a local that outlives its call, and all
        // interfaces belong to `device`.
        let (enumerator, processor) = unsafe {
            let enumerator = video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            video_device.CreateVideoProcessorOutputView(
                &target,
                &enumerator,
                &view,
                Some(&mut target_view),
            )?;
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
            );
            video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            (enumerator, processor)
        };
        let target_view = target_view.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        Ok(Self {
            device,
            context,
            video_device,
            video_context,
            enumerator,
            processor,
            target,
            target_view,
            staging,
            input,
            output,
            fps,
        })
    }

    /// Draws the top-left `shown` pixels of `texture` into the target, fitted and centred.
    fn process(&self, texture: &ID3D11Texture2D, shown: (u32, u32)) -> windows::core::Result<()> {
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut view = None;
        // SAFETY: `view_desc` and `view` outlive the call, and `texture` is on `device`.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                texture,
                &self.enumerator,
                &view_desc,
                Some(&mut view),
            )
        }?;
        let source = RECT {
            left: 0,
            top: 0,
            right: shown.0 as i32,
            bottom: shown.1 as i32,
        };
        let dest = fit(shown, self.output);
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(view),
            ..Default::default()
        };
        // SAFETY: the rectangles and `stream` outlive the calls, and every view and the
        // processor belong to `device`.
        let blitted = unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                true,
                Some(&source),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &self.processor,
                0,
                true,
                Some(&dest),
            );
            self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.target_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        blitted
    }

    fn read_back(&self, into: &mut VideoFrame) -> windows::core::Result<()> {
        let (width, height) = (self.output.0 as usize, self.output.1 as usize);
        if into.width != width || into.height != height {
            *into = VideoFrame::blank(width, height);
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are on `device`, and `mapped` outlives the call.
        unsafe {
            self.context.CopyResource(&self.staging, &self.target);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        // SAFETY: a mapped NV12 texture holds `height` luma rows, then `height / 2` chroma
        // rows, each `RowPitch` bytes, and stays mapped until `Unmap` below.
        let data = unsafe {
            std::slice::from_raw_parts(
                mapped.pData.cast::<u8>().cast_const(),
                pitch * (height + height / 2),
            )
        };
        copy_rows(data, pitch, width, &mut into.y);
        copy_rows(
            data.get(pitch * height..).unwrap_or_default(),
            pitch,
            width,
            &mut into.uv,
        );
        // SAFETY: `staging` was mapped above, and `data` is not used past here.
        unsafe { self.context.Unmap(&self.staging, 0) };
        Ok(())
    }
}

fn nv12_texture(
    device: &ID3D11Device,
    (width, height): (u32, u32),
    usage: D3D11_USAGE,
    render_target: bool,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: usage,
        BindFlags: if render_target {
            D3D11_BIND_RENDER_TARGET.0 as u32
        } else {
            0
        },
        CPUAccessFlags: if render_target {
            0
        } else {
            D3D11_CPU_ACCESS_READ.0 as u32
        },
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: `desc` and `texture` outlive the call.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    texture.ok_or_else(|| windows::core::Error::from(E_POINTER))
}

/// Copies rows of `row_bytes` from a buffer with `pitch` bytes a row into a tightly packed one.
fn copy_rows(src: &[u8], pitch: usize, row_bytes: usize, out: &mut [u8]) {
    if row_bytes == 0 || pitch < row_bytes {
        return;
    }
    for (dst, src) in out.chunks_exact_mut(row_bytes).zip(src.chunks(pitch)) {
        if let Some(src) = src.get(..row_bytes) {
            dst.copy_from_slice(src);
        }
    }
}

/// The largest rectangle with the aspect ratio of `source`, centred in `target`.
fn fit(source: (u32, u32), target: (u32, u32)) -> RECT {
    let (sw, sh) = (u64::from(source.0.max(1)), u64::from(source.1.max(1)));
    let (tw, th) = (u64::from(target.0), u64::from(target.1));
    let (w, h) = if sw * th > tw * sh {
        (tw, tw * sh / sw)
    } else {
        (th * sw / sh, th)
    };
    let (left, top) = ((tw - w) / 2, (th - h) / 2);
    RECT {
        left: left as i32,
        top: top as i32,
        right: (left + w) as i32,
        bottom: (top + h) as i32,
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        self.pool.RemoveFrameArrived(self.arrived).ok();
        self.session.Close().ok();
        self.pool.Close().ok();
    }
}

/// Captures the primary monitor without asking, for tests on real hardware.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn primary_monitor(config: CaptureConfig) -> WinCapture {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .expect("the capture item factory");
    // SAFETY: plain lookups with valid arguments.
    let item: GraphicsCaptureItem = unsafe {
        interop
            .CreateForMonitor(MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY))
            .expect("a capture item for the primary monitor")
    };
    capture(&item, config).expect("capture to start")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn admitted(pacer: &mut Pacer, input_hz: i64, seconds: i64) -> Vec<u32> {
        let step = i64::from(VIDEO_CLOCK_HZ) / input_hz;
        (0..input_hz * seconds)
            .filter_map(|n| pacer.admit(1_000_000 + n * step))
            .collect()
    }

    #[test]
    fn pacer_thins_a_fast_display_to_the_share_rate() {
        let kept = admitted(&mut Pacer::new(60), 144, 2);
        assert!((118..=122).contains(&kept.len()), "{} frames", kept.len());
        assert_eq!(kept[0], 0);
    }

    #[test]
    fn pacer_keeps_every_frame_at_or_below_the_share_rate() {
        assert_eq!(admitted(&mut Pacer::new(60), 60, 2).len(), 120);
        assert_eq!(admitted(&mut Pacer::new(60), 30, 2).len(), 60);
    }

    #[test]
    fn pacer_tolerates_jitter() {
        let mut pacer = Pacer::new(60);
        let kept = (0..120)
            .filter_map(|n: i64| pacer.admit(n * 1500 + if n % 2 == 0 { 150 } else { -150 }))
            .count();
        assert_eq!(kept, 120);
    }

    #[test]
    fn fit_letterboxes_and_pillarboxes() {
        let wide = fit((3840, 1200), (1920, 1080));
        assert_eq!((wide.left, wide.right), (0, 1920));
        assert_eq!(wide.bottom - wide.top, 600);
        let tall = fit((1080, 1920), (1920, 1080));
        assert_eq!((tall.top, tall.bottom), (0, 1080));
        assert_eq!(tall.right - tall.left, 607);
        let same = fit((2560, 1440), (1920, 1080));
        assert_eq!(
            (same.left, same.top, same.right, same.bottom),
            (0, 0, 1920, 1080)
        );
    }

    #[test]
    fn copy_rows_drops_the_pitch_padding() {
        let src = [1, 2, 0, 0, 3, 4, 0, 0];
        let mut out = [9; 4];
        copy_rows(&src, 4, 2, &mut out);
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    #[ignore = "needs a display and a GPU"]
    fn primary_monitor_delivers_frames() {
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 30,
            audio: false,
        };
        let mut capture = primary_monitor(config);
        let mut frame = VideoFrame::blank(1280, 720);
        let mut timestamps = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && timestamps.len() < 30 {
            if capture.next_frame(&mut frame) {
                timestamps.push(frame.timestamp);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(
            timestamps.len() >= 10,
            "only {} frames in 3 s; move something on screen",
            timestamps.len()
        );
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]));
        assert!(
            frame.y.iter().any(|&v| v != frame.y[0]),
            "the picture is uniform"
        );
    }
}
