#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
    GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCapturePicker, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_FAIL, E_POINTER, HANDLE, HMODULE, HWND, RECT, WAIT_OBJECT_0};
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
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::UI::Shell::IInitializeWithWindow;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{HRESULT, IInspectable, Interface, Owned, Ref, implement};

use super::clock::{AudioChunker, ShareZero};
use super::{Capture, CaptureConfig, CaptureError};
use crate::media::audio::SAMPLE_RATE;
use crate::media::jitter::FRAME;
use crate::media::share_audio::STEREO_FRAME;
use crate::media::video::frame::{VideoFrame, share_size};
use throcc_proto::frame::VIDEO_CLOCK_HZ;

/// Captured frames waiting for the share thread. A full ring drops the newest frame.
const RING: usize = 2;
/// Textures in the frame pool: every ring slot, plus one being converted.
const POOL_BUFFERS: i32 = RING as i32 + 1;
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const TIME_SPAN_HZ: i128 = 10_000_000;
/// Captured audio packets of up to 20 ms waiting for the share thread. A full ring drops
/// the newest.
const AUDIO_RING: usize = 32;
/// How much audio WASAPI holds for the capture thread, in 100 ns units.
const AUDIO_BUFFER: i64 = 1_000_000;
/// How often the audio thread checks for stop while no audio arrives, in milliseconds.
const AUDIO_WAIT_MS: u32 = 100;
const AUDIO_SETUP_TIMEOUT: Duration = Duration::from_secs(3);

pub struct WinCapture {
    /// Paced frames with their `SystemRelativeTime`, in 100 ns QPC units.
    frames: rtrb::Consumer<(Direct3D11CaptureFrame, i64)>,
    audio: Option<LoopbackAudio>,
    zero: ShareZero,
    chunker: AudioChunker,
    dropped: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
    converter: Converter,
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    pool_size: SizeInt32,
    session: GraphicsCaptureSession,
    arrived: i64,
    item: GraphicsCaptureItem,
    closed: i64,
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
    let source = (size.Width.max(0) as usize, size.Height.max(0) as usize);
    let (width, height) = share_size(source, (config.width, config.height));
    let output = (width as u32, height as u32);
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
            let queued = feed
                .try_lock()
                .is_ok_and(|mut feed| feed.offer(frame, time.Duration));
            if !queued {
                callback_dropped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        },
    ))?;
    let ended = Arc::new(AtomicBool::new(false));
    let stopped = ended.clone();
    let closed = item.Closed(
        &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            stopped.store(true, Ordering::Relaxed);
            Ok(())
        }),
    )?;
    let audio = if config.audio {
        LoopbackAudio::start(dropped.clone())
            .inspect_err(|e| tracing::warn!("sharing without audio: {e}"))
            .ok()
    } else {
        None
    };
    session.StartCapture()?;
    Ok(WinCapture {
        frames,
        audio,
        zero: ShareZero::new(TIME_SPAN_HZ as i64),
        chunker: AudioChunker::default(),
        dropped,
        ended,
        converter,
        device,
        pool,
        pool_size: size,
        session,
        arrived,
        item: item.clone(),
        closed,
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
    frames: rtrb::Producer<(Direct3D11CaptureFrame, i64)>,
    pacer: Pacer,
}

impl Feed {
    /// Queues `frame`, taken at `time` in 100 ns units, if the pacer admits it. False only
    /// when the ring was full.
    fn offer(&mut self, frame: Direct3D11CaptureFrame, time: i64) -> bool {
        let ticks = (i128::from(time) * i128::from(VIDEO_CLOCK_HZ) / TIME_SPAN_HZ) as i64;
        !self.pacer.admit(ticks) || self.frames.push((frame, time)).is_ok()
    }
}

/// Thins frames, given their 90 kHz capture times, to the share's rate on average.
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

    fn admit(&mut self, ticks: i64) -> bool {
        let since = ticks - *self.start.get_or_insert(ticks);
        if since < self.due - self.interval / 4 {
            return false;
        }
        self.due = if since - self.due > self.interval {
            since + self.interval
        } else {
            self.due + self.interval
        };
        true
    }
}

impl Capture for WinCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        let Ok((frame, time)) = self.frames.pop() else {
            return false;
        };
        let ticks = self.zero.ticks(time, VIDEO_CLOCK_HZ);
        if ticks < 0 {
            return false;
        }
        let timestamp = ticks as u32;
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

    fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    fn captures_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        let audio = self.audio.as_mut()?;
        loop {
            while self.chunker.needs_more() {
                let packet = audio.packets.pop().ok()?;
                let ticks = self.zero.ticks(packet.at, SAMPLE_RATE);
                self.chunker.push(ticks, packet.samples().iter().copied());
            }
            if let Some(timestamp) = self.chunker.take(into) {
                return Some(timestamp);
            }
        }
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

/// Everything played on the machine except throcc's own process tree, captured by
/// process loopback on a thread of its own.
struct LoopbackAudio {
    packets: rtrb::Consumer<AudioPacket>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackAudio {
    /// Starts the capture thread and waits until the stream runs or fails to open.
    fn start(dropped: Arc<AtomicU64>) -> Result<Self, String> {
        let (mut producer, packets) = rtrb::RingBuffer::new(AUDIO_RING);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, opened) = mpsc::sync_channel(1);
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("throcc-share-audio".into())
            .spawn(move || {
                // SAFETY: called once on this thread, paired with `CoUninitialize` below.
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                match Loopback::open() {
                    Ok(stream) => {
                        if ready.send(Ok(())).is_ok() {
                            stream.run(&mut producer, &thread_stop, &dropped);
                        }
                    }
                    Err(e) => {
                        ready.send(Err(e.to_string())).ok();
                    }
                }
                if com.is_ok() {
                    // SAFETY: COM was initialised on this thread above, and every COM
                    // object of this thread is dropped by now.
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|e| e.to_string())?;
        let mut audio = Self {
            packets,
            stop,
            thread: Some(thread),
        };
        match opened.recv_timeout(AUDIO_SETUP_TIMEOUT) {
            Ok(Ok(())) => Ok(audio),
            Ok(Err(e)) => Err(format!("process loopback: {e}")),
            Err(_) => {
                // Activation can hang, and joining the thread would hang with it.
                audio.thread.take();
                Err("process loopback didn't start".into())
            }
        }
    }
}

impl Drop for LoopbackAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// Up to 20 ms of interleaved stereo, taken at `at` in 100 ns QPC units.
struct AudioPacket {
    at: i64,
    frames: usize,
    samples: [f32; STEREO_FRAME],
}

impl AudioPacket {
    fn samples(&self) -> &[f32] {
        self.samples.get(..self.frames * 2).unwrap_or_default()
    }
}

/// Queues `frames` frames of interleaved stereo taken at `at` as packets of at most 20 ms.
/// Missing `samples` queue silence. Returns how many packets the full ring dropped.
fn queue_audio(
    producer: &mut rtrb::Producer<AudioPacket>,
    at: i64,
    frames: usize,
    samples: Option<&[f32]>,
) -> u64 {
    let mut dropped = 0;
    for start in (0..frames).step_by(FRAME) {
        let count = (frames - start).min(FRAME);
        let mut packet = AudioPacket {
            at: at + (start as i128 * TIME_SPAN_HZ / i128::from(SAMPLE_RATE)) as i64,
            frames: count,
            samples: [0.0; STEREO_FRAME],
        };
        let source = samples.and_then(|s| s.get(start * 2..(start + count) * 2));
        if let (Some(source), Some(target)) = (source, packet.samples.get_mut(..count * 2)) {
            target.copy_from_slice(source);
        }
        if producer.push(packet).is_err() {
            dropped += 1;
        }
    }
    dropped
}

struct Loopback {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Owned<HANDLE>,
}

impl Loopback {
    /// Opens an event-driven 48 kHz stereo float capture of every process but this one's tree.
    fn open() -> windows::core::Result<Self> {
        let client = activate_loopback()?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: 2,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        // SAFETY: `format` outlives the call, and the event handle stays open as long as
        // the client, which `Loopback` owns alongside it.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                AUDIO_BUFFER,
                0,
                &format,
                None,
            )?;
            let event = Owned::new(CreateEventW(None, false, false, None)?);
            client.SetEventHandle(*event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self {
                client,
                capture,
                event,
            })
        }
    }

    /// Moves captured audio into `producer` until `stop` is set or the stream fails.
    fn run(
        &self,
        producer: &mut rtrb::Producer<AudioPacket>,
        stop: &AtomicBool,
        dropped: &AtomicU64,
    ) {
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: the event handle is open for as long as `self`.
            if unsafe { WaitForSingleObject(*self.event, AUDIO_WAIT_MS) } != WAIT_OBJECT_0 {
                continue;
            }
            if let Err(e) = self.drain(producer, dropped) {
                tracing::warn!("share audio stopped: {e}");
                return;
            }
        }
    }

    fn drain(
        &self,
        producer: &mut rtrb::Producer<AudioPacket>,
        dropped: &AtomicU64,
    ) -> windows::core::Result<()> {
        // SAFETY: every out-pointer is a local that outlives its call, and each buffer is
        // read only between its `GetBuffer` and `ReleaseBuffer`.
        unsafe {
            while self.capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let count = frames as usize;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                // SAFETY: the stream was opened as 32-bit float stereo, so a buffer of
                // `frames` frames holds `2 * frames` aligned floats.
                let samples = (!silent && !data.is_null())
                    .then(|| std::slice::from_raw_parts(data.cast::<f32>(), count * 2));
                if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 {
                    let lost = queue_audio(producer, qpc as i64, count, samples);
                    dropped.fetch_add(lost, Ordering::Relaxed);
                }
                self.capture.ReleaseBuffer(frames)?;
            }
        }
        Ok(())
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        // SAFETY: plain call on a live client.
        unsafe { self.client.Stop() }.ok();
    }
}

/// Activates an `IAudioClient` on the process loopback device, excluding this process tree.
fn activate_loopback() -> windows::core::Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let blob = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&raw const params).cast_mut().cast(),
                    },
                },
            }),
        },
    };
    let (done, activated) = mpsc::sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done).into();
    // SAFETY: `blob` points at `params`, and both live until activation has completed.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&blob),
            &handler,
        )
    }?;
    activated
        .recv()
        .map_err(|_| windows::core::Error::from(E_FAIL))?;
    let mut result = HRESULT(0);
    let mut client = None;
    // SAFETY: both out-pointers are locals that outlive the call.
    unsafe { operation.GetActivateResult(&mut result, &mut client) }?;
    result.ok()?;
    client
        .ok_or_else(|| windows::core::Error::from(E_POINTER))?
        .cast()
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        self.0.try_send(()).ok();
        Ok(())
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        self.pool.RemoveFrameArrived(self.arrived).ok();
        self.item.RemoveClosed(self.closed).ok();
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

    fn admitted(pacer: &mut Pacer, input_hz: i64, seconds: i64) -> Vec<i64> {
        let step = i64::from(VIDEO_CLOCK_HZ) / input_hz;
        (0..input_hz * seconds)
            .map(|n| 1_000_000 + n * step)
            .filter(|&ticks| pacer.admit(ticks))
            .collect()
    }

    #[test]
    fn pacer_thins_a_fast_display_to_the_share_rate() {
        let kept = admitted(&mut Pacer::new(60), 144, 2);
        assert!((118..=122).contains(&kept.len()), "{} frames", kept.len());
        assert_eq!(kept[0], 1_000_000);
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
            .filter(|n: &i64| pacer.admit(n * 1500 + if n % 2 == 0 { 150 } else { -150 }))
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
    fn audio_is_queued_in_packets_of_at_most_20_ms() {
        let (mut producer, mut packets) = rtrb::RingBuffer::new(4);
        let samples: Vec<f32> = (0..2 * 1200).map(|i| i as f32).collect();
        assert_eq!(
            queue_audio(&mut producer, 5_000_000, 1200, Some(&samples)),
            0
        );
        let first = packets.pop().unwrap();
        let second = packets.pop().unwrap();
        assert_eq!((first.at, first.frames), (5_000_000, 960));
        assert_eq!((second.at, second.frames), (5_200_000, 240));
        assert_eq!(second.samples()[0], 1920.0);
        assert!(packets.pop().is_err());
    }

    #[test]
    fn silent_audio_is_queued_as_zeros_and_a_full_ring_drops() {
        let (mut producer, mut packets) = rtrb::RingBuffer::new(1);
        assert_eq!(queue_audio(&mut producer, 0, 1920, None), 1);
        let packet = packets.pop().unwrap();
        assert_eq!(packet.samples().len(), STEREO_FRAME);
        assert!(packet.samples().iter().all(|&s| s == 0.0));
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

    #[test]
    #[ignore = "needs a display, a GPU and speakers"]
    fn share_audio_arrives_in_order_with_sound() {
        let config = CaptureConfig {
            width: 640,
            height: 360,
            fps: 30,
            audio: true,
        };
        let mut capture = primary_monitor(config);
        assert!(capture.captures_audio());
        let mut player = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()",
            ])
            .spawn()
            .unwrap();
        let mut frame = VideoFrame::blank(640, 360);
        let mut sound = [0.0f32; STEREO_FRAME];
        let (mut timestamps, mut loudest) = (Vec::new(), 0.0f32);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            capture.next_frame(&mut frame);
            match capture.next_audio(&mut sound) {
                Some(timestamp) => {
                    timestamps.push(timestamp);
                    loudest = sound.iter().fold(loudest, |m, s| m.max(s.abs()));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        player.kill().ok();
        player.wait().ok();
        assert!(
            timestamps.len() > 100,
            "{} audio frames in 4 s",
            timestamps.len()
        );
        let steps = timestamps.windows(2).filter(|w| w[1] - w[0] == 960).count();
        assert!(
            steps * 10 >= (timestamps.len() - 1) * 9,
            "only {steps} of {} steps are 960 ticks: {timestamps:?}",
            timestamps.len() - 1
        );
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]));
        assert!(loudest > 0.01, "loudest sample {loudest}");
    }
}
