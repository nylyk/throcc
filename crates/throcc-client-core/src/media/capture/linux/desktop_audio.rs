use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::client::{Client, ClientListener};
use pw::core::CoreRc;
use pw::link::Link;
use pw::properties::properties;
use pw::registry::GlobalObject;
use pw::spa;
use pw::spa::buffer::ChunkFlags;
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::{Object, Pod};
use pw::spa::utils::dict::DictRef;
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::{Stream, StreamBox, StreamFlags};
use pw::types::ObjectType;

use super::{failed, serialize};
use crate::media::audio::SAMPLE_RATE;
use crate::media::capture::CaptureError;

/// Captured samples waiting for the share thread: one second of stereo.
const SAMPLE_RING: usize = SAMPLE_RATE as usize * 2;
/// Captured buffers waiting for the share thread.
const BUFFER_RING: usize = 64;

/// Everything the desktop plays except this process's own playback, mixed to 48 kHz stereo.
pub(super) struct DesktopAudio {
    buffers: rtrb::Consumer<Captured>,
    samples: rtrb::Consumer<f32>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

/// One buffer of interleaved stereo in the sample ring, stamped with its graph cycle's
/// PipeWire time in nanoseconds.
#[derive(Clone, Copy)]
struct Captured {
    at: i64,
    frames: usize,
}

impl DesktopAudio {
    /// Counts buffers the process callback drops into `dropped`.
    pub(super) fn start(dropped: Arc<AtomicU64>) -> Result<Self, CaptureError> {
        let (buffers_tx, buffers) = rtrb::RingBuffer::new(BUFFER_RING);
        let (samples_tx, samples) = rtrb::RingBuffer::new(SAMPLE_RING);
        let state = AudioState {
            buffers: buffers_tx,
            samples: samples_tx,
            dropped,
        };
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("throcc-pw-audio".into())
            .spawn(move || {
                if let Err(e) = run(state, quit_rx, &ready_tx) {
                    ready_tx.send(Err(e)).ok();
                }
            })
            .map_err(failed)?;
        match ready.recv() {
            Ok(Ok(())) => Ok(Self {
                buffers,
                samples,
                quit,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                thread.join().ok();
                Err(e)
            }
            Err(_) => {
                thread.join().ok();
                Err(CaptureError::Failed(
                    "the PipeWire audio thread stopped".into(),
                ))
            }
        }
    }

    /// The oldest captured buffer, interleaved stereo, with its PipeWire time in nanoseconds.
    pub(super) fn pop(&mut self) -> Option<(i64, rtrb::chunks::ReadChunk<'_, f32>)> {
        let Captured { at, frames } = self.buffers.pop().ok()?;
        let samples = self.samples.read_chunk(frames * 2).ok()?;
        Some((at, samples))
    }
}

impl Drop for DesktopAudio {
    fn drop(&mut self) {
        self.quit.send(()).ok();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

struct AudioState {
    buffers: rtrb::Producer<Captured>,
    samples: rtrb::Producer<f32>,
    dropped: Arc<AtomicU64>,
}

fn run(
    state: AudioState,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), CaptureError>>,
) -> Result<(), CaptureError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failed)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failed)?;
    let core = context.connect_rc(None).map_err(failed)?;
    let name = format!("throcc-share-audio-{}", std::process::id());
    let stream = StreamBox::new(
        &core,
        &name,
        properties! {
            *pw::keys::NODE_NAME => name.as_str(),
            *pw::keys::NODE_AUTOCONNECT => "false",
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(failed)?;
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .process(on_process)
        .register()
        .map_err(failed)?;
    let format = serialize(format_param())?;
    let format = Pod::from_bytes(&format)
        .ok_or_else(|| CaptureError::Failed("bad audio format parameters".into()))?;
    stream
        .connect(
            Direction::Input,
            None,
            StreamFlags::MAP_BUFFERS,
            &mut [format],
        )
        .map_err(failed)?;

    let graph = Rc::new(RefCell::new(Graph::new(name)));
    let registry = core.get_registry_rc().map_err(failed)?;
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let graph = Rc::downgrade(&graph);
            let registry = registry.downgrade();
            let core = core.downgrade();
            move |global| {
                if let (Some(graph), Some(registry), Some(core)) =
                    (graph.upgrade(), registry.upgrade(), core.upgrade())
                {
                    on_global(&graph, global, &registry, &core);
                }
            }
        })
        .global_remove({
            let graph = Rc::downgrade(&graph);
            move |id| {
                if let Some(graph) = graph.upgrade() {
                    graph.borrow_mut().remove(id);
                }
            }
        })
        .register();
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    ready.send(Ok(())).ok();
    mainloop.run();
    Ok(())
}

/// Asks for 48 kHz stereo float; PipeWire converts whatever is linked in.
pub(super) fn format_param() -> Object {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(2);
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    }
}

fn on_process(stream: &Stream, state: &mut AudioState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Ok(time) = stream.time() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let chunk = data.chunk();
    if chunk.flags().contains(ChunkFlags::CORRUPTED) {
        return;
    }
    let offset = chunk.offset() as usize;
    let size = chunk.size() as usize;
    let Some(bytes) = data
        .data()
        .and_then(|mapped| mapped.get(offset..offset.saturating_add(size)))
    else {
        return;
    };
    let frames = bytes.len() / 8;
    if frames == 0 {
        return;
    }
    let slots = match state.samples.write_chunk_uninit(frames * 2) {
        Ok(slots) if !state.buffers.is_full() => slots,
        _ => {
            state.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let samples = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b));
    let written = slots.fill_from_iter(samples);
    state
        .buffers
        .push(Captured {
            at: time.now(),
            frames: written / 2,
        })
        .ok();
}

/// The stereo side a playback port's channel is mixed into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Both,
}

impl Side {
    fn of(channel: Option<&str>) -> Option<Self> {
        match channel {
            Some("FL" | "SL" | "RL") => Some(Self::Left),
            Some("FR" | "SR" | "RR") => Some(Self::Right),
            Some("LFE" | "LFE2") => None,
            _ => Some(Self::Both),
        }
    }

    fn feeds(self, input: Self) -> bool {
        self == Self::Both || self == input
    }
}

struct Port {
    node: u32,
    output: bool,
    side: Option<Side>,
}

/// The playback streams in the PipeWire graph and the links from them into our capture node.
struct Graph {
    own_name: String,
    own_node: Option<u32>,
    /// Clients by id, bound to read the process id in their info.
    clients: HashMap<u32, (Client, ClientListener)>,
    /// Whether each client whose info arrived belongs to this process.
    client_is_ours: HashMap<u32, bool>,
    /// Playback stream nodes by id, with the client that made them.
    playback: HashMap<u32, Option<u32>>,
    ports: HashMap<u32, Port>,
    /// Links by output port and input port, kept alive by their proxies.
    links: HashMap<(u32, u32), Link>,
}

impl Graph {
    fn new(own_name: String) -> Self {
        Self {
            own_name,
            own_node: None,
            clients: HashMap::new(),
            client_is_ours: HashMap::new(),
            playback: HashMap::new(),
            ports: HashMap::new(),
            links: HashMap::new(),
        }
    }

    fn remove(&mut self, id: u32) {
        if self.own_node == Some(id) {
            self.own_node = None;
        }
        self.clients.remove(&id);
        self.client_is_ours.remove(&id);
        self.playback.remove(&id);
        self.ports.remove(&id);
        self.links
            .retain(|&(out, input), _| out != id && input != id);
    }

    /// Whether `node` is a playback stream of another process.
    fn is_source(&self, node: u32) -> bool {
        match self.playback.get(&node) {
            Some(Some(client)) => self.client_is_ours.get(client) == Some(&false),
            Some(None) => true,
            None => false,
        }
    }

    fn relink(&mut self, core: &CoreRc) {
        let Some(own) = self.own_node else {
            return;
        };
        let outputs: Vec<(u32, u32, Side)> = self
            .ports
            .iter()
            .filter_map(|(id, port)| {
                let side = port.side?;
                (port.output && self.is_source(port.node)).then_some((*id, port.node, side))
            })
            .collect();
        let inputs: Vec<(u32, Side)> = self
            .ports
            .iter()
            .filter_map(|(id, port)| {
                let side = port.side?;
                (!port.output && port.node == own).then_some((*id, side))
            })
            .collect();
        for (out, out_node, side) in outputs {
            for &(input, input_side) in &inputs {
                if !side.feeds(input_side) || self.links.contains_key(&(out, input)) {
                    continue;
                }
                let link = core.create_object::<Link>(
                    "link-factory",
                    &properties! {
                        *pw::keys::LINK_OUTPUT_NODE => out_node.to_string(),
                        *pw::keys::LINK_OUTPUT_PORT => out.to_string(),
                        *pw::keys::LINK_INPUT_NODE => own.to_string(),
                        *pw::keys::LINK_INPUT_PORT => input.to_string(),
                        *pw::keys::OBJECT_LINGER => "false",
                    },
                );
                if let Ok(link) = link {
                    self.links.insert((out, input), link);
                }
            }
        }
    }
}

fn on_global(
    graph: &Rc<RefCell<Graph>>,
    global: &GlobalObject<&DictRef>,
    registry: &pw::registry::RegistryRc,
    core: &CoreRc,
) {
    let Some(props) = global.props else {
        return;
    };
    let mut graph_ref = graph.borrow_mut();
    match global.type_ {
        ObjectType::Client => {
            if let Ok(client) = registry.bind::<Client, _>(global) {
                let listener = watch_client(&client, global.id, Rc::downgrade(graph), core);
                graph_ref.clients.insert(global.id, (client, listener));
            }
        }
        ObjectType::Node => {
            if props.get(*pw::keys::NODE_NAME) == Some(graph_ref.own_name.as_str()) {
                graph_ref.own_node = Some(global.id);
            } else if props.get(*pw::keys::MEDIA_CLASS) == Some("Stream/Output/Audio") {
                let client = props.get(*pw::keys::CLIENT_ID).and_then(|c| c.parse().ok());
                graph_ref.playback.insert(global.id, client);
            }
            graph_ref.relink(core);
        }
        ObjectType::Port => {
            let (Some(node), Some(direction)) = (
                props.get(*pw::keys::NODE_ID).and_then(|n| n.parse().ok()),
                props.get(*pw::keys::PORT_DIRECTION),
            ) else {
                return;
            };
            let port = Port {
                node,
                output: direction == "out",
                side: Side::of(props.get(*pw::keys::AUDIO_CHANNEL)),
            };
            graph_ref.ports.insert(global.id, port);
            graph_ref.relink(core);
        }
        _ => {}
    }
}

/// Records whether client `id` belongs to this process once its info arrives.
fn watch_client(
    client: &Client,
    id: u32,
    graph: Weak<RefCell<Graph>>,
    core: &CoreRc,
) -> ClientListener {
    let core = core.downgrade();
    let own_pid = std::process::id().to_string();
    client
        .add_listener_local()
        .info(move |info| {
            let Some(props) = info.props() else {
                return;
            };
            let ours = props.get(*pw::keys::APP_PROCESS_ID) == Some(own_pid.as_str());
            if let (Some(graph), Some(core)) = (graph.upgrade(), core.upgrade()) {
                let mut graph = graph.borrow_mut();
                graph.client_is_ours.insert(id, ours);
                graph.relink(&core);
            }
        })
        .register()
}
