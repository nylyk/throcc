#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod desktop_audio;
mod egl;

use std::cell::{Cell, RefCell};
use std::io::Cursor;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::buffer::{ChunkFlags, Data, DataType};
use pw::spa::param::ParamType;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{
    Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes,
};
use pw::stream::{Stream, StreamBox, StreamFlags};
use throcc_proto::frame::{VIDEO_CLOCK_HZ, timestamp_before};
use tracing::warn;

use super::clock::{AudioChunker, ShareZero};
use super::{Capture, CaptureConfig, CaptureError};
use crate::media::audio::SAMPLE_RATE;
use crate::media::share_audio::STEREO_FRAME;
use crate::media::video::frame::VideoFrame;
use desktop_audio::DesktopAudio;

/// Frame buffers shared by the PipeWire thread and the share thread.
const POOL: usize = 4;
/// PipeWire buffers the share thread may hold at once: the ring's, the one it reads, and
/// one on its way back.
const LENT: usize = POOL + 2;
const MAX_PLANES: usize = 4;
/// Formats offered in GPU memory, with their DRM fourccs.
const DMABUF_FORMATS: [(VideoFormat, u32); 4] = [
    (VideoFormat::BGRx, fourcc(b"XR24")),
    (VideoFormat::BGRA, fourcc(b"AR24")),
    (VideoFormat::RGBx, fourcc(b"XB24")),
    (VideoFormat::RGBA, fourcc(b"AB24")),
];
const DMABUF_FOURCCS: [u32; 4] = [
    DMABUF_FORMATS[0].1,
    DMABUF_FORMATS[1].1,
    DMABUF_FORMATS[2].1,
    DMABUF_FORMATS[3].1,
];
const MAX_SIDE: u32 = 8192;
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

    fn next_dmabuf(&mut self) -> Option<&DmaBuf> {
        self.stream.next_dmabuf()
    }

    fn refuse_dmabuf(&mut self, modifier: Option<u64>) {
        self.stream.refuse_dmabuf(modifier);
    }

    fn dropped(&self) -> Arc<AtomicU64> {
        self.stream.dropped()
    }

    fn ended(&self) -> bool {
        self.stream.ended()
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
    ended: Arc<AtomicBool>,
    control: pw::channel::Sender<Control>,
    thread: Option<JoinHandle<()>>,
    /// The frame in GPU memory last handed out, held until the next one is asked for.
    current: Option<HeldDmaBuf>,
    audio: Option<DesktopAudio>,
    /// PipeWire time, in nanoseconds, that both media clocks count from.
    zero: ShareZero,
    chunker: AudioChunker,
    last_frame: Option<u32>,
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
        let counted = dropped.clone();
        let ended = Arc::new(AtomicBool::new(false));
        let stopped = ended.clone();
        let (control, control_rx) = pw::channel::channel();
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("throcc-pipewire".into())
            .spawn(move || {
                let state = StreamState {
                    layout: None,
                    free: free_rx,
                    filled: filled_tx,
                    spare: None,
                    dropped: counted,
                    lent: Rc::default(),
                };
                if let Err(e) = run(node, fd, config, state, stopped, control_rx, &ready_tx) {
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
                ended,
                control,
                thread: Some(thread),
                current: None,
                audio,
                zero: ShareZero::new(NANOS as i64),
                chunker: AudioChunker::default(),
                last_frame: None,
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

impl PipeWireCapture {
    /// Hands the frame in GPU memory last given out back to the PipeWire thread.
    fn give_back(&mut self) {
        if let Some(held) = self.current.take() {
            self.control.send(Control::Return(held.lease)).ok();
        }
    }

    fn next_is_dmabuf(&self) -> bool {
        self.filled.peek().is_ok_and(|slot| slot.dmabuf.is_some())
    }
}

impl Capture for PipeWireCapture {
    fn next_dmabuf(&mut self) -> Option<&DmaBuf> {
        self.give_back();
        while self.next_is_dmabuf() {
            let Ok(mut slot) = self.filled.pop() else {
                break;
            };
            let held = slot.dmabuf.take();
            self.free.push(slot).ok();
            let Some(mut held) = held else {
                continue;
            };
            let ticks = self.zero.ticks(held.at, VIDEO_CLOCK_HZ);
            if ticks < 0 {
                self.control.send(Control::Return(held.lease)).ok();
                continue;
            }
            held.frame.timestamp = strictly_after(self.last_frame, ticks as u32);
            self.last_frame = Some(held.frame.timestamp);
            return Some(&self.current.insert(held).frame);
        }
        None
    }

    fn refuse_dmabuf(&mut self, modifier: Option<u64>) {
        self.control.send(Control::Refuse(modifier)).ok();
    }

    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        self.give_back();
        while !self.next_is_dmabuf()
            && let Ok(mut slot) = self.filled.pop()
        {
            let picture = slot.picture.take();
            let stamped = picture.as_ref().and_then(|picture| {
                let ticks = self.zero.ticks(picture.at, VIDEO_CLOCK_HZ);
                (ticks >= 0).then_some((picture, ticks as u32))
            });
            if let Some((picture, timestamp)) = stamped {
                convert(&slot.bytes, picture, into);
                into.timestamp = strictly_after(self.last_frame, timestamp);
                self.last_frame = Some(into.timestamp);
            }
            if slot.bytes.len() < slot.needed {
                slot.bytes.resize(slot.needed, 0);
            }
            self.free.push(slot).ok();
            if stamped.is_some() {
                return true;
            }
        }
        false
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
                let (at, samples) = audio.pop()?;
                let ticks = self.zero.ticks(at, SAMPLE_RATE);
                self.chunker.push(ticks, samples);
            }
            if let Some(timestamp) = self.chunker.take(into) {
                return Some(timestamp);
            }
        }
    }
}

/// `timestamp`, or the tick after `last` when it isn't later.
fn strictly_after(last: Option<u32>, timestamp: u32) -> u32 {
    match last {
        Some(last) if !timestamp_before(last, timestamp) => last.wrapping_add(1),
        _ => timestamp,
    }
}

/// CLOCK_MONOTONIC in nanoseconds, the clock PipeWire's audio times are on.
fn monotonic_nanos() -> i64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime only writes the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec * NANOS as i64 + now.tv_nsec
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.control.send(Control::Quit).ok();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// Messages from the share thread to the PipeWire thread.
enum Control {
    Quit,
    /// The share thread is done with a lent buffer.
    Return(Lease),
    /// Stops offering one modifier, or all of them, and renegotiates.
    Refuse(Option<u64>),
}

fn run(
    node: u32,
    fd: Option<OwnedFd>,
    config: CaptureConfig,
    state: StreamState,
    ended: Arc<AtomicBool>,
    control: pw::channel::Receiver<Control>,
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
    let buffers = Buffers {
        dmabuf: serialize(buffers_param(1 << spa::sys::SPA_DATA_DmaBuf))?,
        shared: serialize(buffers_param(
            (1 << spa::sys::SPA_DATA_MemPtr) | (1 << spa::sys::SPA_DATA_MemFd),
        ))?,
    };
    let offers = if cfg!(feature = "ffmpeg") {
        dmabuf_offers(egl::dmabuf_modifiers)
    } else {
        Vec::new()
    };
    let lent = state.lent.clone();
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(move |_, _, _, new| {
            if matches!(
                new,
                pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
            ) {
                ended.store(true, Ordering::Relaxed);
            }
        })
        .param_changed(move |stream, state, id, param| {
            on_param_changed(stream, state, id, param, &buffers);
        })
        .process(on_process)
        .remove_buffer(|_, state, buffer| state.lent.forget(buffer))
        .register()
        .map_err(failed)?;
    let formats = format_params(config, &offers)?;
    stream
        .connect(
            Direction::Input,
            Some(node),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut pods(&formats)?,
        )
        .map_err(failed)?;
    let raw = NonNull::from(&*stream);
    let offers = RefCell::new(offers);
    let _control = control.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |message| {
            // SAFETY: this handler is detached when `_control` drops, before `stream`.
            let stream = unsafe { raw.as_ref() };
            match message {
                Control::Quit => mainloop.quit(),
                Control::Return(lease) => {
                    if let Some(buffer) = lent.take_back(lease) {
                        // SAFETY: the lease was live, so the buffer is still the stream's
                        // and dequeued.
                        unsafe { stream.queue_raw_buffer(buffer.as_ptr()) };
                    }
                }
                Control::Refuse(modifier) => {
                    if let Ok(mut offers) = offers.try_borrow_mut() {
                        renegotiate(stream, config, &mut offers, modifier);
                    }
                }
            }
        }
    });
    ready.send(Ok(())).ok();
    mainloop.run();
    Ok(())
}

fn renegotiate(
    stream: &Stream,
    config: CaptureConfig,
    offers: &mut Vec<Offer>,
    modifier: Option<u64>,
) {
    refuse(offers, modifier);
    warn!(
        ?modifier,
        "capturing in GPU memory failed; {} formats left there",
        offers.len()
    );
    let updated = format_params(config, offers)
        .and_then(|formats| stream.update_params(&mut pods(&formats)?).map_err(failed));
    if let Err(e) = updated {
        warn!("couldn't renegotiate the screen capture: {e}");
    }
}

/// A format offered in GPU memory and the modifiers it is offered with.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Offer {
    format: VideoFormat,
    modifiers: Vec<u64>,
}

/// One offer per format in GPU memory that `modifiers_of` gives modifiers for.
fn dmabuf_offers(modifiers_of: impl Fn(u32) -> Vec<u64>) -> Vec<Offer> {
    DMABUF_FORMATS
        .iter()
        .map(|&(format, fourcc)| Offer {
            format,
            modifiers: modifiers_of(fourcc),
        })
        .filter(|offer| !offer.modifiers.is_empty())
        .collect()
}

/// Drops `modifier` from every offer, or every offer without one.
fn refuse(offers: &mut Vec<Offer>, modifier: Option<u64>) {
    match modifier {
        Some(modifier) => {
            for offer in offers.iter_mut() {
                offer.modifiers.retain(|offered| *offered != modifier);
            }
            offers.retain(|offer| !offer.modifiers.is_empty());
        }
        None => offers.clear(),
    }
}

/// The formats to offer: each one in GPU memory first, then all of them in shared memory.
fn format_params(config: CaptureConfig, offers: &[Offer]) -> Result<Vec<Vec<u8>>, CaptureError> {
    offers
        .iter()
        .map(|offer| {
            let format = spa::pod::property!(FormatProperties::VideoFormat, Id, offer.format);
            format_param(config, format, Some(modifier_property(&offer.modifiers)))
        })
        .chain(std::iter::once(format_param(
            config,
            shared_formats(),
            None,
        )))
        .map(serialize)
        .collect()
}

fn pods(params: &[Vec<u8>]) -> Result<Vec<&Pod>, CaptureError> {
    params
        .iter()
        .map(|bytes| {
            Pod::from_bytes(bytes)
                .ok_or_else(|| CaptureError::Failed("bad format parameters".into()))
        })
        .collect()
}

/// NV12 first, then the packed RGB formats.
fn shared_formats() -> Property {
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
    )
}

/// The modifiers as a choice the producer must pick from and fixate itself.
fn modifier_property(modifiers: &[u64]) -> Property {
    let modifiers: Vec<i64> = modifiers.iter().map(|m| *m as i64).collect();
    Property {
        key: spa::sys::SPA_FORMAT_VIDEO_modifier,
        flags: PropertyFlags::MANDATORY
            | PropertyFlags::from_bits_retain(spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE),
        value: Value::Choice(ChoiceValue::Long(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: modifiers.first().copied().unwrap_or_default(),
                alternatives: modifiers,
            },
        ))),
    }
}

/// `format`, and `modifier` if given, at the configured size, capped at its fps.
fn format_param(config: CaptureConfig, format: Property, modifier: Option<Property>) -> Object {
    let size = Rectangle {
        width: u32::try_from(config.width).unwrap_or(MAX_SIDE),
        height: u32::try_from(config.height).unwrap_or(MAX_SIDE),
    };
    let fps = Fraction {
        num: config.fps.max(1),
        denom: 1,
    };
    let mut properties = vec![
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        format,
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
    ];
    properties.extend(modifier);
    Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    }
}

/// Asks for buffers of the given data types.
fn buffers_param(types: i32) -> Object {
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

/// The Buffers parameter for each kind of negotiated format, serialized ahead of time.
struct Buffers {
    dmabuf: Vec<u8>,
    shared: Vec<u8>,
}

struct StreamState {
    layout: Option<Layout>,
    free: rtrb::Consumer<Slot>,
    filled: rtrb::Producer<Slot>,
    spare: Option<Slot>,
    dropped: Arc<AtomicU64>,
    lent: Rc<Lent>,
}

impl StreamState {
    /// Copies or lends out the dequeued `buffer`. True when it was lent, so it must not be
    /// queued back yet.
    fn take(&mut self, buffer: NonNull<pw::sys::pw_buffer>) -> bool {
        let Some(layout) = self.layout else {
            return false;
        };
        // SAFETY: the buffer is dequeued, so its data is ours until it is queued back.
        let datas = unsafe { datas(buffer) };
        let Some(mut slot) = self.spare.take().or_else(|| self.free.pop().ok()) else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let at = monotonic_nanos();
        let in_gpu_memory = datas.first().is_some_and(|d| d.type_() == DataType::DmaBuf);
        let lent = match layout.drm {
            Some(drm) if in_gpu_memory => {
                let frame = dmabuf_of(datas, layout, drm);
                let Some((frame, lease)) = frame.zip(self.lent.lend(buffer)) else {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    self.spare = Some(slot);
                    return false;
                };
                slot.dmabuf = Some(HeldDmaBuf { lease, frame, at });
                true
            }
            _ => {
                match copy_buffer(datas, layout, &mut slot) {
                    Copied::Planes(planes) => {
                        slot.picture = Some(Picture { layout, planes, at });
                    }
                    Copied::TooSmall => {
                        slot.picture = None;
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Copied::Nothing => {
                        self.spare = Some(slot);
                        return false;
                    }
                }
                false
            }
        };
        if let Err(rtrb::PushError::Full(mut slot)) = self.filled.push(slot) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            if let Some(held) = slot.dmabuf.take() {
                self.lent.take_back(held.lease);
            }
            self.spare = Some(slot);
            return false;
        }
        lent
    }
}

/// # Safety
/// `buffer` must be dequeued from a stream and not queued back while the slice lives.
unsafe fn datas<'a>(buffer: NonNull<pw::sys::pw_buffer>) -> &'a mut [Data] {
    // SAFETY: a dequeued pw_buffer points at a spa_buffer with `n_datas` datas, and Data
    // is a transparent spa_data.
    unsafe {
        let buffer = buffer.as_ref().buffer;
        if buffer.is_null() || (*buffer).datas.is_null() {
            return &mut [];
        }
        std::slice::from_raw_parts_mut((*buffer).datas.cast::<Data>(), (*buffer).n_datas as usize)
    }
}

/// The planes of a buffer in GPU memory, with their file descriptors duplicated so they
/// outlive the buffer's removal from the stream.
fn dmabuf_of(datas: &[Data], layout: Layout, drm: Drm) -> Option<DmaBuf> {
    let mut planes = [const { None }; MAX_PLANES];
    let dmabufs = datas.iter().filter(|d| d.type_() == DataType::DmaBuf);
    for (data, plane) in dmabufs.zip(&mut planes) {
        let chunk = data.chunk();
        if chunk.flags().contains(ChunkFlags::CORRUPTED) {
            return None;
        }
        *plane = Some(DmaBufPlane {
            fd: duplicate(data.fd())?,
            offset: chunk.offset(),
            stride: u32::try_from(chunk.stride()).ok()?,
        });
    }
    planes.first()?.as_ref()?;
    Some(DmaBuf {
        width: u32::try_from(layout.width).ok()?,
        height: u32::try_from(layout.height).ok()?,
        fourcc: drm.fourcc,
        modifier: drm.modifier,
        planes,
        timestamp: 0,
    })
}

fn duplicate(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC only reads `fd`; a non-negative result is a new descriptor
    // nothing else owns.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    // SAFETY: see above.
    (copy >= 0).then(|| unsafe { OwnedFd::from_raw_fd(copy) })
}

/// PipeWire buffers lent to the share thread, so that only ones still in the stream are
/// queued back.
#[derive(Default)]
struct Lent {
    buffers: [Cell<Option<LentBuffer>>; LENT],
    next: Cell<u64>,
}

type LentBuffer = (NonNull<pw::sys::pw_buffer>, Lease);

/// Names one lending of a buffer, never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lease(u64);

impl Lent {
    /// Lends `buffer` out, unless as many as can be tracked already are.
    fn lend(&self, buffer: NonNull<pw::sys::pw_buffer>) -> Option<Lease> {
        let free = self.buffers.iter().find(|entry| entry.get().is_none())?;
        let lease = Lease(self.next.get());
        self.next.set(lease.0.wrapping_add(1));
        free.set(Some((buffer, lease)));
        Some(lease)
    }

    /// The buffer behind `lease`, if the stream still has it.
    fn take_back(&self, lease: Lease) -> Option<NonNull<pw::sys::pw_buffer>> {
        self.buffers.iter().find_map(|entry| match entry.get() {
            Some((buffer, lent)) if lent == lease => {
                entry.set(None);
                Some(buffer)
            }
            _ => None,
        })
    }

    /// Forgets `buffer`, which the stream removed.
    fn forget(&self, buffer: *mut pw::sys::pw_buffer) {
        for entry in &self.buffers {
            if entry.get().is_some_and(|(lent, _)| lent.as_ptr() == buffer) {
                entry.set(None);
            }
        }
    }
}

fn on_param_changed(
    stream: &Stream,
    state: &mut StreamState,
    id: u32,
    param: Option<&Pod>,
    buffers: &Buffers,
) {
    if id != ParamType::Format.as_raw() {
        return;
    }
    let mut info = VideoInfoRaw::new();
    state.layout = param
        .filter(|param| info.parse(param).is_ok())
        .and_then(|param| Layout::of(&info, has_modifier(param)));
    let Some(layout) = state.layout else {
        return;
    };
    let chosen = match layout.drm {
        Some(_) => &buffers.dmabuf,
        None => &buffers.shared,
    };
    if let Some(buffers) = Pod::from_bytes(chosen) {
        stream.update_params(&mut [buffers]).ok();
    }
}

/// Whether a negotiated format carries a modifier, which makes its buffers DMA-BUFs.
fn has_modifier(format: &Pod) -> bool {
    format.as_object().is_ok_and(|object| {
        object
            .find_prop(Id(spa::sys::SPA_FORMAT_VIDEO_modifier))
            .is_some()
    })
}

fn on_process(stream: &Stream, state: &mut StreamState) {
    // SAFETY: the buffer is queued back below unless it was lent; a lent one is queued back
    // when the share thread returns it.
    let Some(buffer) = NonNull::new(unsafe { stream.dequeue_raw_buffer() }) else {
        return;
    };
    if !state.take(buffer) {
        // SAFETY: the buffer came from this stream and is not lent.
        unsafe { stream.queue_raw_buffer(buffer.as_ptr()) };
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
    dmabuf: Option<HeldDmaBuf>,
}

impl Slot {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0; len],
            picture: None,
            needed: 0,
            dmabuf: None,
        }
    }
}

/// A captured picture still in GPU memory, as DRM PRIME planes.
#[derive(Debug)]
pub struct DmaBuf {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: [Option<DmaBufPlane>; MAX_PLANES],
    /// Media timestamp, 90 kHz.
    pub timestamp: u32,
}

impl DmaBuf {
    pub fn planes(&self) -> impl Iterator<Item = &DmaBufPlane> {
        self.planes.iter().flatten()
    }
}

#[derive(Debug)]
pub struct DmaBufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// A frame in GPU memory and the PipeWire buffer it lives in, lent to the share thread.
#[derive(Debug)]
struct HeldDmaBuf {
    lease: Lease,
    frame: DmaBuf,
    /// PipeWire time of the graph cycle that delivered the picture, in nanoseconds.
    at: i64,
}

/// The DRM format of a negotiated stream in GPU memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drm {
    fourcc: u32,
    modifier: u64,
}

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
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
    /// Set when the buffers are DMA-BUFs.
    drm: Option<Drm>,
}

impl Layout {
    fn of(info: &VideoInfoRaw, has_modifier: bool) -> Option<Self> {
        let format = match info.format() {
            VideoFormat::NV12 => Format::Nv12,
            VideoFormat::BGRx | VideoFormat::BGRA => Format::Bgr,
            VideoFormat::RGBx | VideoFormat::RGBA => Format::Rgb,
            _ => return None,
        };
        let drm = if has_modifier {
            let (_, fourcc) = DMABUF_FORMATS
                .iter()
                .find(|(offered, _)| *offered == info.format())?;
            Some(Drm {
                fourcc: *fourcc,
                modifier: info.modifier(),
            })
        } else {
            None
        };
        let size = info.size();
        Some(Self {
            format,
            width: usize::try_from(size.width).ok().filter(|w| *w > 0)?,
            height: usize::try_from(size.height).ok().filter(|h| *h > 0)?,
            drm,
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
        ..
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn frame_timestamps_only_move_forward() {
        assert_eq!(strictly_after(None, 0), 0);
        assert_eq!(strictly_after(Some(0), 0), 1);
        assert_eq!(strictly_after(Some(10), 4), 11);
        assert_eq!(strictly_after(Some(10), 1500), 1500);
        assert_eq!(strictly_after(Some(u32::MAX), 3), 3);
    }

    #[test]
    fn the_monotonic_clock_advances() {
        let before = monotonic_nanos();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(monotonic_nanos() - before >= 2_000_000);
    }

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
                drm: None,
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

    const CONFIG: CaptureConfig = CaptureConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        audio: false,
    };
    const TILED: u64 = 0x0200_0000_0000_0b01;

    fn object(bytes: &[u8]) -> Object {
        match spa::pod::deserialize::PodDeserializer::deserialize_any_from(bytes) {
            Ok((_, Value::Object(object))) => object,
            other => panic!("not an object: {other:?}"),
        }
    }

    fn modifier_of(object: &Object) -> Option<&Property> {
        object
            .properties
            .iter()
            .find(|p| p.key == spa::sys::SPA_FORMAT_VIDEO_modifier)
    }

    #[test]
    fn gpu_memory_is_offered_first_and_left_to_the_producer_to_fixate() {
        let offers = dmabuf_offers(|fourcc| match &fourcc.to_le_bytes() {
            b"XR24" => vec![0, TILED],
            b"AR24" => vec![TILED],
            _ => Vec::new(),
        });
        assert_eq!(
            offers,
            [
                Offer {
                    format: VideoFormat::BGRx,
                    modifiers: vec![0, TILED]
                },
                Offer {
                    format: VideoFormat::BGRA,
                    modifiers: vec![TILED]
                },
            ]
        );
        let params: Vec<Object> = format_params(CONFIG, &offers)
            .unwrap()
            .iter()
            .map(|bytes| object(bytes))
            .collect();
        assert_eq!(params.len(), 3);
        let modifier = modifier_of(&params[0]).expect("a modifier on the first format");
        assert!(modifier.flags.contains(PropertyFlags::MANDATORY));
        assert!(modifier.flags.bits() & spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE != 0);
        match &modifier.value {
            Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
                assert_eq!(alternatives, &[0, TILED as i64]);
            }
            other => panic!("modifiers as {other:?}"),
        }
        assert!(modifier_of(&params[1]).is_some());
        assert!(
            modifier_of(&params[2]).is_none(),
            "the shared memory fallback"
        );
    }

    #[test]
    fn a_refused_modifier_is_offered_no_more() {
        let mut offers = vec![
            Offer {
                format: VideoFormat::BGRx,
                modifiers: vec![0, TILED],
            },
            Offer {
                format: VideoFormat::BGRA,
                modifiers: vec![TILED],
            },
        ];
        refuse(&mut offers, Some(TILED));
        assert_eq!(
            offers,
            [Offer {
                format: VideoFormat::BGRx,
                modifiers: vec![0]
            }]
        );
        refuse(&mut offers, None);
        assert!(offers.is_empty());
        let params = format_params(CONFIG, &offers).unwrap();
        assert_eq!(params.len(), 1);
        assert!(modifier_of(&object(&params[0])).is_none());
    }

    fn negotiated(modifier: Option<u64>) -> Option<Layout> {
        let mut properties = vec![
            spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
            spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
            spa::pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::BGRx),
            spa::pod::property!(
                FormatProperties::VideoSize,
                Rectangle,
                Rectangle {
                    width: 2560,
                    height: 1440
                }
            ),
        ];
        properties.extend(modifier.map(|modifier| {
            Property::new(
                spa::sys::SPA_FORMAT_VIDEO_modifier,
                Value::Long(modifier as i64),
            )
        }));
        let bytes = serialize(Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties,
        })
        .unwrap();
        let pod = Pod::from_bytes(&bytes).unwrap();
        let mut info = VideoInfoRaw::new();
        info.parse(pod).unwrap();
        Layout::of(&info, has_modifier(pod))
    }

    #[test]
    fn a_format_with_a_modifier_means_dmabufs() {
        let layout = negotiated(Some(TILED)).unwrap();
        assert_eq!(
            layout.drm,
            Some(Drm {
                fourcc: fourcc(b"XR24"),
                modifier: TILED
            })
        );
        assert_eq!((layout.width, layout.height), (2560, 1440));
        assert_eq!(
            negotiated(Some(0)).unwrap().drm.map(|d| d.modifier),
            Some(0)
        );
        assert_eq!(negotiated(None).unwrap().drm, None);
    }

    #[test]
    fn only_buffers_still_lent_are_given_back() {
        let buffer = |n: usize| NonNull::new(std::ptr::without_provenance_mut(n * 64)).unwrap();
        let lent = Lent::default();
        let leases: Vec<Lease> = (1..=LENT).map(|n| lent.lend(buffer(n)).unwrap()).collect();
        assert_eq!(lent.lend(buffer(99)), None, "more than can be tracked");

        assert_eq!(lent.take_back(leases[0]), Some(buffer(1)));
        assert_eq!(lent.take_back(leases[0]), None, "already given back");

        lent.forget(buffer(2).as_ptr());
        assert_eq!(lent.take_back(leases[1]), None, "removed by the stream");

        let again = lent.lend(buffer(2)).unwrap();
        assert_ne!(again, leases[1]);
        assert_eq!(
            lent.take_back(leases[1]),
            None,
            "an old lease of a reused buffer"
        );
        assert_eq!(lent.take_back(again), Some(buffer(2)));
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
