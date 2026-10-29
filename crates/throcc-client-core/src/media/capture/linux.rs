#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod desktop_audio;

use std::collections::VecDeque;
use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::buffer::{ChunkFlags, Data};
use pw::spa::param::ParamType;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Rectangle, SpaTypes};
use pw::stream::{Stream, StreamBox, StreamFlags};
use throcc_proto::frame::VIDEO_CLOCK_HZ;
use tracing::warn;

use super::{Capture, CaptureConfig, CaptureError};
use crate::media::audio::SAMPLE_RATE;
use crate::media::jitter::FRAME;
use crate::media::share_audio::STEREO_FRAME;
use crate::media::video::frame::VideoFrame;
use desktop_audio::DesktopAudio;

/// Frame buffers shared by the PipeWire thread and the share thread.
const POOL: usize = 4;
const MAX_SIDE: u32 = 8192;
/// An audio buffer further than this from where the previous one ended starts afresh.
const AUDIO_SLIP: i64 = FRAME as i64 / 2;
const NANOS: i128 = 1_000_000_000;

/// A portal screen cast: the session the person picked and the PipeWire stream reading it.
pub struct LinuxCapture {
    stream: PipeWireCapture,
    session: Session<Screencast>,
}

pub fn start(config: CaptureConfig) -> Result<LinuxCapture, CaptureError> {
    let token_file = token_file();
    let token = token_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let shared = async_io::block_on(share(token.as_deref()))?;
    if let (Some(path), Some(token)) = (&token_file, &shared.token) {
        save_token(path, token);
    }
    match PipeWireCapture::start(shared.node, Some(shared.fd), config) {
        Ok(stream) => Ok(LinuxCapture {
            stream,
            session: shared.session,
        }),
        Err(e) => {
            async_io::block_on(shared.session.close()).ok();
            Err(e)
        }
    }
}

impl Capture for LinuxCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        self.stream.next_frame(into)
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        self.stream.dropped()
    }

    fn captures_audio(&self) -> bool {
        self.stream.captures_audio()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        self.stream.next_audio(into)
    }
}

impl Drop for LinuxCapture {
    fn drop(&mut self) {
        async_io::block_on(self.session.close()).ok();
    }
}

struct Shared {
    session: Session<Screencast>,
    node: u32,
    fd: OwnedFd,
    token: Option<String>,
}

/// Shows the portal's picker, or reuses the choice behind `token`, and opens its stream.
async fn share(token: Option<&str>) -> Result<Shared, CaptureError> {
    let portal = Screencast::new().await.map_err(portal_error)?;
    let session = portal
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;
    match pick(&portal, &session, token).await {
        Ok((node, fd, token)) => Ok(Shared {
            session,
            node,
            fd,
            token,
        }),
        Err(e) => {
            session.close().await.ok();
            Err(e)
        }
    }
}

async fn pick(
    portal: &Screencast,
    session: &Session<Screencast>,
    token: Option<&str>,
) -> Result<(u32, OwnedFd, Option<String>), CaptureError> {
    let sources = portal
        .available_source_types()
        .await
        .map_or(SourceType::Monitor | SourceType::Window, |available| {
            available & (SourceType::Monitor | SourceType::Window)
        });
    let cursor = portal
        .available_cursor_modes()
        .await
        .ok()
        .filter(|modes| modes.contains(CursorMode::Embedded))
        .map(|_| CursorMode::Embedded);
    let options = SelectSourcesOptions::default()
        .set_sources(sources)
        .set_cursor_mode(cursor)
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(token);
    portal
        .select_sources(session, options)
        .await
        .and_then(|request| request.response())
        .map_err(portal_error)?;
    let streams = portal
        .start(session, None, Default::default())
        .await
        .and_then(|request| request.response())
        .map_err(portal_error)?;
    let node = streams
        .streams()
        .first()
        .map(|stream| stream.pipe_wire_node_id())
        .ok_or_else(|| CaptureError::Failed("the portal shared nothing".into()))?;
    let fd = portal
        .open_pipe_wire_remote(session, Default::default())
        .await
        .map_err(portal_error)?;
    Ok((node, fd, streams.restore_token().map(str::to_owned)))
}

fn portal_error(e: ashpd::Error) -> CaptureError {
    match e {
        ashpd::Error::Response(ResponseError::Cancelled) => CaptureError::Cancelled,
        e => failed(e),
    }
}

fn token_file() -> Option<PathBuf> {
    crate::keystore::default_dir()
        .ok()
        .map(|dir| dir.join("screencast-restore-token"))
}

fn save_token(path: &std::path::Path, token: &str) {
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(path, token));
    if let Err(e) = saved {
        warn!("couldn't keep the screen share choice: {e}");
    }
}

fn failed(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Failed(e.to_string())
}

/// A PipeWire video stream read on its own thread and main loop, with the desktop's audio
/// when the config asks for it.
pub(crate) struct PipeWireCapture {
    filled: rtrb::Consumer<Slot>,
    free: rtrb::Producer<Slot>,
    dropped: Arc<AtomicU64>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    audio: Option<DesktopAudio>,
    /// PipeWire time, in nanoseconds, that both media clocks count from.
    zero: Option<i64>,
    /// Captured audio not yet taken, left and right interleaved.
    pending: VecDeque<f32>,
    /// Timestamp of the first pending sample, in 48 kHz ticks from `zero`.
    pending_at: i64,
}

impl PipeWireCapture {
    /// Connects to `node`, on the remote behind `fd` or on the default PipeWire daemon.
    pub(crate) fn start(
        node: u32,
        fd: Option<OwnedFd>,
        config: CaptureConfig,
    ) -> Result<Self, CaptureError> {
        let (mut free, free_rx) = rtrb::RingBuffer::new(POOL);
        let (filled_tx, filled) = rtrb::RingBuffer::new(POOL);
        for _ in 0..POOL {
            free.push(Slot::new(config.width * config.height * 4)).ok();
        }
        let dropped = Arc::new(AtomicU64::new(0));
        let state = StreamState {
            layout: None,
            free: free_rx,
            filled: filled_tx,
            spare: None,
            dropped: dropped.clone(),
        };
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("throcc-pipewire".into())
            .spawn(move || {
                if let Err(e) = run(node, fd, config, state, quit_rx, &ready_tx) {
                    ready_tx.send(Err(e)).ok();
                }
            })
            .map_err(failed)?;
        let audio = config
            .audio
            .then(|| DesktopAudio::start(dropped.clone()))
            .and_then(|audio| {
                audio
                    .inspect_err(|e| warn!("sharing without sound: {e}"))
                    .ok()
            });
        match ready.recv() {
            Ok(Ok(())) => Ok(Self {
                filled,
                free,
                dropped,
                quit,
                thread: Some(thread),
                audio,
                zero: None,
                pending: VecDeque::new(),
                pending_at: 0,
            }),
            Ok(Err(e)) => {
                thread.join().ok();
                Err(e)
            }
            Err(_) => {
                thread.join().ok();
                Err(CaptureError::Failed("the PipeWire thread stopped".into()))
            }
        }
    }
}

impl Capture for PipeWireCapture {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        while let Ok(mut slot) = self.filled.pop() {
            let picture = slot.picture.take();
            if let Some(picture) = &picture {
                convert(&slot.bytes, picture, into);
                into.timestamp = since_zero(&mut self.zero, picture.at, VIDEO_CLOCK_HZ) as u32;
            }
            if slot.bytes.len() < slot.needed {
                slot.bytes.resize(slot.needed, 0);
            }
            self.free.push(slot).ok();
            if picture.is_some() {
                return true;
            }
        }
        false
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }

    fn captures_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        while self.pending.len() < STEREO_FRAME {
            let (at, samples) = self.audio.as_mut()?.pop()?;
            let ticks = since_zero(&mut self.zero, at, SAMPLE_RATE);
            let expected = self.pending_at + (self.pending.len() / 2) as i64;
            if self.pending.is_empty() || (ticks - expected).abs() > AUDIO_SLIP {
                self.pending.clear();
                self.pending_at = ticks;
            }
            self.pending.extend(samples);
        }
        if self.pending_at < 0 {
            self.pending.drain(..STEREO_FRAME);
            self.pending_at += FRAME as i64;
            return None;
        }
        for (slot, sample) in into.iter_mut().zip(self.pending.drain(..STEREO_FRAME)) {
            *slot = sample;
        }
        let timestamp = self.pending_at as u32;
        self.pending_at += FRAME as i64;
        Some(timestamp)
    }
}

/// Ticks of a `rate` Hz clock from `zero`, which the first frame or audio buffer taken sets,
/// to `at`, both in nanoseconds of PipeWire time.
fn since_zero(zero: &mut Option<i64>, at: i64, rate: u32) -> i64 {
    let zero = *zero.get_or_insert(at);
    (i128::from(at - zero) * i128::from(rate) / NANOS) as i64
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.quit.send(()).ok();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn run(
    node: u32,
    fd: Option<OwnedFd>,
    config: CaptureConfig,
    state: StreamState,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), CaptureError>>,
) -> Result<(), CaptureError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failed)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failed)?;
    let core = match fd {
        Some(fd) => context.connect_fd_rc(fd, None),
        None => context.connect_rc(None),
    }
    .map_err(failed)?;
    let stream = StreamBox::new(
        &core,
        "throcc-screen",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(failed)?;
    let buffers = serialize(buffers_param())?;
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(move |stream, state, id, param| {
            on_param_changed(stream, state, id, param, &buffers);
        })
        .process(on_process)
        .register()
        .map_err(failed)?;
    let format = serialize(format_param(config))?;
    let format = Pod::from_bytes(&format)
        .ok_or_else(|| CaptureError::Failed("bad format parameters".into()))?;
    stream
        .connect(
            Direction::Input,
            Some(node),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [format],
        )
        .map_err(failed)?;
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    ready.send(Ok(())).ok();
    mainloop.run();
    Ok(())
}

/// Offers NV12 first, then the packed RGB formats, at the configured size, capped at its fps.
fn format_param(config: CaptureConfig) -> Object {
    let size = Rectangle {
        width: u32::try_from(config.width).unwrap_or(MAX_SIDE),
        height: u32::try_from(config.height).unwrap_or(MAX_SIDE),
    };
    let fps = Fraction {
        num: config.fps.max(1),
        denom: 1,
    };
    spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::NV12,
            VideoFormat::NV12,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            size,
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: MAX_SIDE,
                height: MAX_SIDE
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            fps,
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            fps,
            Fraction { num: 1, denom: 1 },
            fps
        ),
    )
}

/// Asks for buffers in shared memory the stream can map.
fn buffers_param() -> Object {
    let types = (1 << spa::sys::SPA_DATA_MemPtr) | (1 << spa::sys::SPA_DATA_MemFd);
    spa::pod::object!(
        SpaTypes::ObjectParamBuffers,
        ParamType::Buffers,
        Property::new(spa::sys::SPA_PARAM_BUFFERS_dataType, data_types(types)),
    )
}

/// The buffer data types as a flags choice, which PipeWire intersects with the peer's.
fn data_types(types: i32) -> Value {
    Value::Choice(ChoiceValue::Int(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Flags {
            default: types,
            flags: Vec::new(),
        },
    )))
}

fn serialize(object: Object) -> Result<Vec<u8>, CaptureError> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(bytes, _)| bytes.into_inner())
        .map_err(|e| CaptureError::Failed(format!("PipeWire parameters: {e:?}")))
}

struct StreamState {
    layout: Option<Layout>,
    free: rtrb::Consumer<Slot>,
    filled: rtrb::Producer<Slot>,
    spare: Option<Slot>,
    dropped: Arc<AtomicU64>,
}

fn on_param_changed(
    stream: &Stream,
    state: &mut StreamState,
    id: u32,
    param: Option<&Pod>,
    buffers: &[u8],
) {
    if id != ParamType::Format.as_raw() {
        return;
    }
    let mut info = VideoInfoRaw::new();
    state.layout = param
        .filter(|param| info.parse(param).is_ok())
        .and_then(|_| Layout::of(&info));
    if state.layout.is_some()
        && let Some(buffers) = Pod::from_bytes(buffers)
    {
        stream.update_params(&mut [buffers]).ok();
    }
}

fn on_process(stream: &Stream, state: &mut StreamState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(layout) = state.layout else {
        return;
    };
    let Ok(time) = stream.time() else {
        return;
    };
    let Some(mut slot) = state.spare.take().or_else(|| state.free.pop().ok()) else {
        state.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match copy_buffer(buffer.datas_mut(), layout, &mut slot) {
        Copied::Planes(planes) => {
            slot.picture = Some(Picture {
                layout,
                planes,
                at: time.now(),
            });
        }
        Copied::TooSmall => {
            slot.picture = None;
            state.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Copied::Nothing => {
            state.spare = Some(slot);
            return;
        }
    }
    if let Err(rtrb::PushError::Full(slot)) = state.filled.push(slot) {
        state.spare = Some(slot);
    }
}

enum Copied {
    Planes([Plane; 2]),
    TooSmall,
    Nothing,
}

/// Copies the buffer's planes into `slot`, back to back. Sets `slot.needed` to their size.
fn copy_buffer(datas: &mut [Data], layout: Layout, slot: &mut Slot) -> Copied {
    let used = if layout.format == Format::Nv12 { 2 } else { 1 };
    let mut sources = [(0usize, 0usize, 0usize); 2];
    let mut needed = 0usize;
    let mut count = 0;
    for (data, source) in datas.iter().take(used).zip(&mut sources) {
        let chunk = data.chunk();
        if chunk.flags().contains(ChunkFlags::CORRUPTED) || chunk.size() == 0 {
            return Copied::Nothing;
        }
        let stride = usize::try_from(chunk.stride())
            .ok()
            .filter(|stride| *stride > 0)
            .unwrap_or(layout.packed_stride());
        *source = (chunk.offset() as usize, chunk.size() as usize, stride);
        needed = needed.saturating_add(chunk.size() as usize);
        count += 1;
    }
    if count == 0 {
        return Copied::Nothing;
    }
    slot.needed = needed;
    if slot.bytes.len() < needed {
        return Copied::TooSmall;
    }
    let mut planes = [Plane::default(); 2];
    let mut end = 0usize;
    let copies = datas.iter_mut().take(count).zip(sources).zip(&mut planes);
    for ((data, (offset, size, stride)), plane) in copies {
        let Some(src) = data
            .data()
            .and_then(|mapped| mapped.get(offset..offset.saturating_add(size)))
        else {
            return Copied::Nothing;
        };
        let Some(dst) = slot.bytes.get_mut(end..end.saturating_add(size)) else {
            return Copied::Nothing;
        };
        dst.copy_from_slice(src);
        *plane = Plane {
            offset: end,
            stride,
        };
        end = end.saturating_add(size);
    }
    if layout.format == Format::Nv12 && count == 1 {
        let [y, _] = planes;
        planes = [
            y,
            Plane {
                offset: y
                    .offset
                    .saturating_add(y.stride.saturating_mul(layout.height)),
                stride: y.stride,
            },
        ];
    }
    Copied::Planes(planes)
}

struct Slot {
    bytes: Vec<u8>,
    picture: Option<Picture>,
    needed: usize,
}

impl Slot {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0; len],
            picture: None,
            needed: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Picture {
    layout: Layout,
    planes: [Plane; 2],
    /// PipeWire time of the graph cycle that delivered the picture, in nanoseconds.
    at: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Plane {
    offset: usize,
    stride: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    format: Format,
    width: usize,
    height: usize,
}

impl Layout {
    fn of(info: &VideoInfoRaw) -> Option<Self> {
        let format = match info.format() {
            VideoFormat::NV12 => Format::Nv12,
            VideoFormat::BGRx | VideoFormat::BGRA => Format::Bgr,
            VideoFormat::RGBx | VideoFormat::RGBA => Format::Rgb,
            _ => return None,
        };
        let size = info.size();
        Some(Self {
            format,
            width: usize::try_from(size.width).ok().filter(|w| *w > 0)?,
            height: usize::try_from(size.height).ok().filter(|h| *h > 0)?,
        })
    }

    fn packed_stride(self) -> usize {
        match self.format {
            Format::Nv12 => self.width,
            Format::Bgr | Format::Rgb => self.width.saturating_mul(4),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Nv12,
    /// BGRx or BGRA.
    Bgr,
    /// RGBx or RGBA.
    Rgb,
}

fn convert(bytes: &[u8], picture: &Picture, into: &mut VideoFrame) {
    let Layout {
        format,
        width,
        height,
    } = picture.layout;
    if into.width != width || into.height != height {
        *into = VideoFrame::blank(width, height);
    }
    let [y, uv] = picture
        .planes
        .map(|plane| Some((bytes.get(plane.offset..)?, plane.stride)));
    let Some((y, y_stride)) = y else {
        return;
    };
    match format {
        Format::Nv12 => {
            copy_rows(y, y_stride, width, &mut into.y);
            if let Some((uv, uv_stride)) = uv {
                copy_rows(uv, uv_stride, width, &mut into.uv);
            }
        }
        Format::Bgr => rgb_to_nv12(y, y_stride, into, |[b, g, r, _]| [r, g, b]),
        Format::Rgb => rgb_to_nv12(y, y_stride, into, |[r, g, b, _]| [r, g, b]),
    }
}

/// Copies rows of `row_bytes` from `src`, `stride` apart, into the tightly packed `dst`.
fn copy_rows(src: &[u8], stride: usize, row_bytes: usize, dst: &mut [u8]) {
    if stride < row_bytes || row_bytes == 0 {
        return;
    }
    for (src, dst) in src.chunks(stride).zip(dst.chunks_exact_mut(row_bytes)) {
        if let Some(src) = src.get(..row_bytes) {
            dst.copy_from_slice(src);
        }
    }
}

/// Converts 4-byte pixels to full-range BT.709 NV12; `rgb` picks red, green and blue.
fn rgb_to_nv12(
    src: &[u8],
    stride: usize,
    into: &mut VideoFrame,
    rgb: impl Fn([u8; 4]) -> [u8; 3] + Copy,
) {
    let width = into.width;
    let row_bytes = width.saturating_mul(4);
    if width == 0 || stride < row_bytes {
        return;
    }
    let mut rows = src.chunks(stride).map(|row| {
        row.get(..row_bytes)
            .map_or(&[][..], |row| row.as_chunks::<4>().0)
    });
    let mut y_rows = into.y.chunks_exact_mut(width);
    for uv_row in into.uv.chunks_exact_mut(width) {
        let (Some(top), Some(top_y)) = (rows.next(), y_rows.next()) else {
            return;
        };
        luma_row(top, top_y, rgb);
        let bottom = match (rows.next(), y_rows.next()) {
            (Some(bottom), Some(bottom_y)) => {
                luma_row(bottom, bottom_y, rgb);
                bottom
            }
            _ => top,
        };
        chroma_row(top, bottom, uv_row, rgb);
    }
}

fn luma_row(pixels: &[[u8; 4]], out: &mut [u8], rgb: impl Fn([u8; 4]) -> [u8; 3]) {
    for (pixel, y) in pixels.iter().zip(out) {
        let [r, g, b] = rgb(*pixel).map(u32::from);
        *y = ((54 * r + 183 * g + 19 * b + 128) >> 8) as u8;
    }
}

/// Writes one interleaved U, V pair per 2×2 block of `top` and `bottom`.
fn chroma_row(
    top: &[[u8; 4]],
    bottom: &[[u8; 4]],
    out: &mut [u8],
    rgb: impl Fn([u8; 4]) -> [u8; 3],
) {
    let pairs = top.chunks(2).zip(bottom.chunks(2));
    for ((top, bottom), uv) in pairs.zip(out.as_chunks_mut::<2>().0) {
        let mut sum = [0i32; 3];
        let mut count = 0i32;
        for pixel in top.iter().chain(bottom) {
            for (sum, value) in sum.iter_mut().zip(rgb(*pixel)) {
                *sum += i32::from(value);
            }
            count += 1;
        }
        let [r, g, b] = sum.map(|sum| (sum + count / 2) / count.max(1));
        let chroma = |kr: i32, kg: i32, kb: i32| {
            ((kr * r + kg * g + kb * b + 128) >> 8)
                .saturating_add(128)
                .clamp(0, 255) as u8
        };
        *uv = [chroma(-29, -99, 128), chroma(128, -116, -12)];
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn bgrx_frame(width: usize, height: usize, stride: usize, pixel: [u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0xee; stride * height];
        for row in bytes.chunks_mut(stride) {
            for px in row[..width * 4].chunks_mut(4) {
                px.copy_from_slice(&pixel);
            }
        }
        bytes
    }

    fn picture(format: Format, width: usize, height: usize, planes: [Plane; 2]) -> Picture {
        Picture {
            layout: Layout {
                format,
                width,
                height,
            },
            planes,
            at: 7,
        }
    }

    #[test]
    fn primaries_convert_to_full_range_bt709() {
        for (bgrx, y, u, v) in [
            ([0, 0, 0, 0], 0, 128, 128),
            ([255, 255, 255, 0], 255, 128, 128),
            ([0, 0, 255, 0], 54, 99, 255),
            ([0, 255, 0, 0], 182, 29, 12),
            ([255, 0, 0, 0], 19, 255, 116),
        ] {
            let bytes = bgrx_frame(4, 2, 20, bgrx);
            let plane = Plane {
                offset: 0,
                stride: 20,
            };
            let mut frame = VideoFrame::blank(4, 2);
            convert(&bytes, &picture(Format::Bgr, 4, 2, [plane; 2]), &mut frame);
            for got in &frame.y {
                assert!(got.abs_diff(y) <= 1, "{bgrx:?}: Y {got}, want {y}");
            }
            for uv in frame.uv.chunks(2) {
                assert!(uv[0].abs_diff(u) <= 1, "{bgrx:?}: U {}, want {u}", uv[0]);
                assert!(uv[1].abs_diff(v) <= 1, "{bgrx:?}: V {}, want {v}", uv[1]);
            }
        }
    }

    #[test]
    fn rgb_and_bgr_orders_match() {
        let bgr = bgrx_frame(2, 2, 8, [10, 20, 200, 0]);
        let rgb = bgrx_frame(2, 2, 8, [200, 20, 10, 0]);
        let plane = Plane {
            offset: 0,
            stride: 8,
        };
        let mut a = VideoFrame::blank(2, 2);
        let mut b = VideoFrame::blank(2, 2);
        convert(&bgr, &picture(Format::Bgr, 2, 2, [plane; 2]), &mut a);
        convert(&rgb, &picture(Format::Rgb, 2, 2, [plane; 2]), &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn strided_nv12_is_packed_and_resized() {
        let (width, height, stride) = (4, 3, 6);
        let mut bytes = Vec::new();
        for row in 0..height {
            bytes.extend((0..stride).map(|col| (row * 10 + col) as u8));
        }
        let uv_at = bytes.len();
        for row in 0..2 {
            bytes.extend((0..stride).map(|col| (100 + row * 10 + col) as u8));
        }
        let planes = [
            Plane { offset: 0, stride },
            Plane {
                offset: uv_at,
                stride,
            },
        ];
        let mut frame = VideoFrame::blank(1280, 720);
        convert(
            &bytes,
            &picture(Format::Nv12, width, height, planes),
            &mut frame,
        );
        assert_eq!((frame.width, frame.height), (4, 3));
        assert_eq!(frame.y, [0, 1, 2, 3, 10, 11, 12, 13, 20, 21, 22, 23]);
        assert_eq!(frame.uv, [100, 101, 102, 103, 110, 111, 112, 113]);
    }

    #[test]
    fn odd_sizes_and_short_buffers_do_not_panic() {
        let plane = Plane {
            offset: 0,
            stride: 28,
        };
        for bytes in [bgrx_frame(7, 5, 28, [1, 2, 3, 0]), vec![0; 30], Vec::new()] {
            let mut frame = VideoFrame::blank(7, 5);
            convert(&bytes, &picture(Format::Bgr, 7, 5, [plane; 2]), &mut frame);
            convert(&bytes, &picture(Format::Nv12, 7, 5, [plane; 2]), &mut frame);
        }
    }

    /// A PipeWire video source that sends solid BGRx frames of one colour.
    struct TestSource {
        node: u32,
        quit: pw::channel::Sender<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestSource {
        fn start(width: u32, height: u32, bgrx: [u8; 4]) -> Self {
            let (quit, quit_rx) = pw::channel::channel::<()>();
            let (node_tx, node_rx) = mpsc::channel();
            let thread = std::thread::spawn(move || {
                pw::init();
                let mainloop = pw::main_loop::MainLoopRc::new(None).unwrap();
                let context = pw::context::ContextRc::new(&mainloop, None).unwrap();
                let core = context.connect_rc(None).unwrap();
                let stream = StreamBox::new(
                    &core,
                    "throcc-test-source",
                    properties! {
                        *pw::keys::MEDIA_CLASS => "Video/Source",
                        *pw::keys::MEDIA_TYPE => "Video",
                        *pw::keys::MEDIA_CATEGORY => "Source",
                    },
                )
                .unwrap();
                let stride = width * 4;
                let buffers = serialize(spa::pod::object!(
                    SpaTypes::ObjectParamBuffers,
                    ParamType::Buffers,
                    Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, Value::Int(4)),
                    Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
                    Property::new(
                        spa::sys::SPA_PARAM_BUFFERS_size,
                        Value::Int((stride * height) as i32)
                    ),
                    Property::new(
                        spa::sys::SPA_PARAM_BUFFERS_stride,
                        Value::Int(stride as i32)
                    ),
                    Property::new(
                        spa::sys::SPA_PARAM_BUFFERS_dataType,
                        data_types(1 << spa::sys::SPA_DATA_MemFd)
                    ),
                ))
                .unwrap();
                let _listener = stream
                    .add_local_listener_with_user_data(())
                    .state_changed(move |stream, _, _, new| {
                        if matches!(new, pw::stream::StreamState::Paused) {
                            node_tx.send(stream.node_id()).ok();
                        }
                    })
                    .param_changed(move |stream, _, id, param| {
                        if id == ParamType::Format.as_raw() && param.is_some() {
                            let pod = Pod::from_bytes(&buffers).unwrap();
                            stream.update_params(&mut [pod]).unwrap();
                        }
                    })
                    .process(move |stream, _| {
                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };
                        let Some(data) = buffer.datas_mut().first_mut() else {
                            return;
                        };
                        if let Some(bytes) = data.data() {
                            bytes.as_chunks_mut::<4>().0.fill(bgrx);
                        }
                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = stride as i32;
                        *chunk.size_mut() = stride * height;
                    })
                    .register()
                    .unwrap();
                let format = serialize(spa::pod::object!(
                    SpaTypes::ObjectParamFormat,
                    ParamType::EnumFormat,
                    spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
                    spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
                    spa::pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::BGRx),
                    spa::pod::property!(
                        FormatProperties::VideoSize,
                        Rectangle,
                        Rectangle { width, height }
                    ),
                    spa::pod::property!(
                        FormatProperties::VideoFramerate,
                        Fraction,
                        Fraction { num: 30, denom: 1 }
                    ),
                ))
                .unwrap();
                stream
                    .connect(
                        Direction::Output,
                        None,
                        StreamFlags::MAP_BUFFERS,
                        &mut [Pod::from_bytes(&format).unwrap()],
                    )
                    .unwrap();
                let _quit = quit_rx.attach(mainloop.loop_(), {
                    let mainloop = mainloop.clone();
                    move |()| mainloop.quit()
                });
                mainloop.run();
            });
            let node = node_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the test source to get a node");
            Self {
                node,
                quit,
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestSource {
        fn drop(&mut self) {
            self.quit.send(()).ok();
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
        }
    }

    fn frames_within(capture: &mut dyn Capture, frame: &mut VideoFrame, want: usize) -> Vec<u32> {
        let mut timestamps = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && timestamps.len() < want {
            if capture.next_frame(frame) {
                timestamps.push(frame.timestamp);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        timestamps
    }

    #[test]
    #[ignore = "needs a PipeWire daemon and a session manager"]
    fn pipewire_source_delivers_converted_frames() {
        let source = TestSource::start(64, 48, [0, 0, 255, 0]);
        let config = CaptureConfig {
            width: 32,
            height: 24,
            fps: 30,
            audio: false,
        };
        let mut capture = PipeWireCapture::start(source.node, None, config).unwrap();
        let mut frame = VideoFrame::blank(32, 24);

        let timestamps = frames_within(&mut capture, &mut frame, 10);
        assert!(timestamps.len() >= 10, "only {} frames", timestamps.len());
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]), "{timestamps:?}");
        assert_eq!((frame.width, frame.height), (64, 48));
        assert!(frame.y.iter().all(|y| y.abs_diff(54) <= 1));
        assert!(
            frame
                .uv
                .chunks(2)
                .all(|uv| uv[0].abs_diff(99) <= 1 && uv[1] == 255)
        );
        let resized = capture.dropped().load(Ordering::Relaxed);
        assert!(resized > 0, "a 32×24 pool fits no 64×48 frame");

        std::thread::sleep(Duration::from_millis(500));
        assert!(capture.dropped().load(Ordering::Relaxed) > resized);
        assert!(frames_within(&mut capture, &mut frame, 10).len() >= 10);
        drop(capture);
    }

    const OTHER_HZ: f32 = 1000.0;
    const OURS_HZ: f32 = 300.0;

    fn tone(hz: f32, n: u32) -> f32 {
        let t = (n % SAMPLE_RATE) as f32 / SAMPLE_RATE as f32;
        0.3 * (std::f32::consts::TAU * hz * t).sin()
    }

    /// A playback stream in this process, so its pid is ours, playing a sine tone.
    struct TestTone {
        cycles: Arc<AtomicU64>,
        quit: pw::channel::Sender<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestTone {
        fn start(hz: f32) -> Self {
            let (quit, quit_rx) = pw::channel::channel::<()>();
            let cycles = Arc::new(AtomicU64::new(0));
            let counted = cycles.clone();
            let thread = std::thread::spawn(move || {
                pw::init();
                let mainloop = pw::main_loop::MainLoopRc::new(None).unwrap();
                let context = pw::context::ContextRc::new(&mainloop, None).unwrap();
                let core = context.connect_rc(None).unwrap();
                let stream = StreamBox::new(
                    &core,
                    "throcc-test-tone",
                    properties! {
                        *pw::keys::MEDIA_TYPE => "Audio",
                        *pw::keys::MEDIA_CATEGORY => "Playback",
                        *pw::keys::MEDIA_ROLE => "Music",
                    },
                )
                .unwrap();
                let _listener = stream
                    .add_local_listener_with_user_data(0u32)
                    .process(move |stream, played| {
                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };
                        let Some(data) = buffer.datas_mut().first_mut() else {
                            return;
                        };
                        let Some(bytes) = data.data() else {
                            return;
                        };
                        let frames = bytes.as_chunks_mut::<8>().0;
                        let count = frames.len().min(1024);
                        for frame in &mut frames[..count] {
                            let sample = tone(hz, *played).to_le_bytes();
                            frame[..4].copy_from_slice(&sample);
                            frame[4..].copy_from_slice(&sample);
                            *played = played.wrapping_add(1);
                        }
                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = 8;
                        *chunk.size_mut() = (count * 8) as u32;
                        counted.fetch_add(1, Ordering::Relaxed);
                    })
                    .register()
                    .unwrap();
                let format = serialize(desktop_audio::format_param()).unwrap();
                stream
                    .connect(
                        Direction::Output,
                        None,
                        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                        &mut [Pod::from_bytes(&format).unwrap()],
                    )
                    .unwrap();
                let _quit = quit_rx.attach(mainloop.loop_(), {
                    let mainloop = mainloop.clone();
                    move |()| mainloop.quit()
                });
                mainloop.run();
            });
            Self {
                cycles,
                quit,
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestTone {
        fn drop(&mut self) {
            self.quit.send(()).ok();
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
        }
    }

    /// Writes `seconds` of a stereo 16-bit 48 kHz tone as a WAV file.
    fn write_tone_wav(path: &std::path::Path, hz: f32, seconds: u32) {
        let frames = SAMPLE_RATE * seconds;
        let data_len = frames * 4;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(SAMPLE_RATE.to_le_bytes());
        bytes.extend((SAMPLE_RATE * 4).to_le_bytes());
        bytes.extend(4u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for n in 0..frames {
            let sample = ((tone(hz, n) * 32767.0) as i16).to_le_bytes();
            bytes.extend(sample);
            bytes.extend(sample);
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// The amplitude of the `hz` component of mono samples spanning whole cycles of it.
    fn level(samples: impl Iterator<Item = f32>, hz: f32) -> f32 {
        let (mut sin, mut cos, mut count) = (0.0f32, 0.0f32, 0usize);
        for (n, sample) in samples.enumerate() {
            let phase = std::f32::consts::TAU * hz * n as f32 / SAMPLE_RATE as f32;
            sin += sample * phase.sin();
            cos += sample * phase.cos();
            count += 1;
        }
        2.0 * sin.hypot(cos) / count.max(1) as f32
    }

    #[test]
    #[ignore = "needs a PipeWire daemon, a session manager, an audio sink and pw-play"]
    fn desktop_audio_has_other_apps_but_not_ours() {
        let source = TestSource::start(64, 48, [0, 0, 255, 0]);
        let ours = TestTone::start(OURS_HZ);
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("other.wav");
        write_tone_wav(&wav, OTHER_HZ, 10);
        let config = CaptureConfig {
            width: 64,
            height: 48,
            fps: 30,
            audio: true,
        };
        let mut capture = PipeWireCapture::start(source.node, None, config).unwrap();
        assert!(capture.captures_audio());
        let mut frame = VideoFrame::blank(64, 48);
        let mut sound = [0.0f32; STEREO_FRAME];

        let mut other = std::process::Command::new("pw-play")
            .arg(&wav)
            .spawn()
            .unwrap();
        let (mut timestamps, mut other_level, mut our_level) = (Vec::new(), 0.0f32, 0.0f32);
        let mut worst_skew = 0.0f64;
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if capture.next_frame(&mut frame) {
                while let Some(timestamp) = capture.next_audio(&mut sound) {
                    timestamps.push(timestamp);
                    let left = || sound.iter().step_by(2).copied();
                    other_level = other_level.max(level(left(), OTHER_HZ));
                    our_level = our_level.max(level(left(), OURS_HZ));
                    let skew = f64::from(frame.timestamp) / f64::from(VIDEO_CLOCK_HZ)
                        - f64::from(timestamp) / f64::from(SAMPLE_RATE);
                    worst_skew = worst_skew.max(skew.abs());
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        other.kill().ok();
        other.wait().ok();
        let tone_cycles = ours.cycles.load(Ordering::Relaxed);
        drop(capture);

        println!(
            "{} audio frames, other app {other_level:.3}, ours {our_level:.4}, \
             worst skew {:.0} ms, our tone ran {tone_cycles} cycles",
            timestamps.len(),
            worst_skew * 1000.0
        );
        assert!(tone_cycles > 50, "our own tone didn't play");
        assert!(
            timestamps.len() > 100,
            "{} audio frames in 4 s",
            timestamps.len()
        );
        assert!(timestamps.windows(2).all(|w| w[1] > w[0]), "{timestamps:?}");
        let steady = timestamps.windows(2).filter(|w| w[1] - w[0] == 960).count();
        assert!(
            steady * 10 >= (timestamps.len() - 1) * 9,
            "only {steady} of {} steps are 960 ticks",
            timestamps.len() - 1
        );
        assert!(other_level > 0.1, "the other app's tone: {other_level}");
        assert!(our_level < 0.03, "our own tone leaked in: {our_level}");
        assert!(worst_skew < 0.2, "audio and video {worst_skew} s apart");
    }

    #[test]
    #[ignore = "needs a desktop session with xdg-desktop-portal and someone to pick a screen"]
    fn portal_share_delivers_frames_and_keeps_the_choice() {
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 30,
            audio: false,
        };
        for share in ["first", "second"] {
            let mut capture = start(config).unwrap();
            let mut frame = VideoFrame::blank(1280, 720);
            let timestamps = frames_within(&mut capture, &mut frame, 10);
            assert!(
                timestamps.len() >= 10,
                "{share} share: only {} frames in 5 s",
                timestamps.len()
            );
            assert!(token_file().unwrap().exists(), "no restore token kept");
            println!("{share} share: {}×{}", frame.width, frame.height);
        }
    }
}
