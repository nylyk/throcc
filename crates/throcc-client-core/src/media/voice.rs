use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use sonora_common_audio::push_sinc_resampler::PushSincResampler;
use throcc_proto::ids::{MediaId, UserId};
use tokio::sync::mpsc;
use tracing::warn;

use super::audio::{CaptureChain, Mixer, SAMPLE_RATE, VoiceSettings};
use super::devices::{self, Devices};
use super::engine::{Kind, MediaEngine, Outgoing, Received};
use super::jitter::PULL;

/// Playback kept queued in the device ring, in 10 ms pulls: enough to ride out
/// scheduling hiccups, little enough to keep mouth-to-ear delay low.
const PLAYBACK_AHEAD: usize = 4;
const IDLE: Duration = Duration::from_millis(2);
const LEVELS_EVERY: u32 = 10;

pub enum VoiceCommand {
    Settings(VoiceSettings),
    /// Sends the mic on this track, or nowhere. A new track restarts its media clock.
    SendOn(Option<MediaId>),
    Muted(bool),
    PushToTalk(bool),
    Deafened(bool),
    Volume(UserId, f32),
    Listen {
        user: UserId,
        units: mpsc::Receiver<Received>,
    },
    Forget(UserId),
    ForgetAll,
    /// Plays the processed mic back locally, for the settings screen's mic test.
    Loopback(bool),
}

/// What the UI shows about audio, refreshed every 100 ms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Levels {
    pub mic_dbfs: f32,
    pub voice_probability: f32,
    pub transmitting: bool,
    pub speaking: HashMap<UserId, bool>,
    pub overruns: u64,
    pub underruns: u64,
}

/// The audio worker: one thread that mixes, plays, captures, cancels echo and encodes.
/// Device callbacks only touch its rings.
pub struct VoiceEngine {
    commands: std_mpsc::Sender<VoiceCommand>,
    pub levels: Arc<Mutex<Levels>>,
    thread: Option<JoinHandle<()>>,
}

impl VoiceEngine {
    pub fn start(
        engine: Arc<MediaEngine>,
        settings: VoiceSettings,
        input: Option<&str>,
        output: Option<&str>,
    ) -> anyhow::Result<Self> {
        let devices = devices::open(input, output)?;
        let chain = CaptureChain::new(&settings)?;
        let (commands, rx) = std_mpsc::channel();
        let levels: Arc<Mutex<Levels>> = Arc::default();
        let worker = Worker::new(devices, chain, engine, levels.clone());
        let thread = std::thread::Builder::new()
            .name("throcc-audio".into())
            .spawn(move || worker.run(rx))?;
        Ok(Self {
            commands,
            levels,
            thread: Some(thread),
        })
    }

    pub fn command(&self, command: VoiceCommand) {
        self.commands.send(command).ok();
    }

    pub fn levels(&self) -> Levels {
        self.levels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        let (closed, _) = std_mpsc::channel();
        self.commands = closed;
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

struct Remote {
    units: mpsc::Receiver<Received>,
}

struct Worker {
    devices: Devices,
    chain: CaptureChain,
    mixer: Mixer,
    remotes: HashMap<UserId, Remote>,
    engine: Arc<MediaEngine>,
    levels: Arc<Mutex<Levels>>,
    to_device: Option<PushSincResampler>,
    from_device: Option<PushSincResampler>,
    device_in: Vec<f32>,
    device_out: Vec<f32>,
    mic_track: Option<MediaId>,
    muted: bool,
    push_to_talk: bool,
    deafened: bool,
    loopback: bool,
    /// Samples played, the local 48 kHz clock that packet arrivals are stamped with.
    played: u64,
}

impl Worker {
    fn new(
        devices: Devices,
        chain: CaptureChain,
        engine: Arc<MediaEngine>,
        levels: Arc<Mutex<Levels>>,
    ) -> Self {
        let per_pull = |rate: u32| (rate / 100) as usize;
        let resampler = |from: u32, to: u32| {
            (from != to).then(|| PushSincResampler::new(per_pull(from), per_pull(to)))
        };
        Self {
            to_device: resampler(SAMPLE_RATE, devices.output_rate),
            from_device: resampler(devices.input_rate, SAMPLE_RATE),
            device_in: vec![0.0; per_pull(devices.input_rate)],
            device_out: vec![0.0; per_pull(devices.output_rate)],
            devices,
            chain,
            mixer: Mixer::default(),
            remotes: HashMap::new(),
            engine,
            levels,
            mic_track: None,
            muted: true,
            push_to_talk: false,
            deafened: false,
            loopback: false,
            played: 0,
        }
    }

    fn run(mut self, commands: std_mpsc::Receiver<VoiceCommand>) {
        let mut pulls = 0u32;
        let mut loopback = [0.0f32; PULL];
        loop {
            loop {
                match commands.try_recv() {
                    Ok(command) => self.apply(command),
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => return,
                }
            }
            self.receive();
            let mut busy = false;
            if self.devices.playback.slots() >= self.device_out.len() * 2
                && self.queued_playback() < PLAYBACK_AHEAD
            {
                self.play(&loopback);
                busy = true;
                pulls += 1;
            }
            if self.devices.capture.slots() >= self.device_in.len() {
                loopback = self.capture();
                busy = true;
            }
            if pulls >= LEVELS_EVERY {
                pulls = 0;
                self.publish_levels();
            }
            if !busy {
                std::thread::sleep(IDLE);
            }
        }
    }

    fn queued_playback(&self) -> usize {
        let capacity = self.devices.playback.buffer().capacity();
        (capacity - self.devices.playback.slots()) / self.device_out.len().max(1)
    }

    fn apply(&mut self, command: VoiceCommand) {
        match command {
            VoiceCommand::Settings(settings) => self.chain.apply(&settings),
            VoiceCommand::SendOn(track) => {
                if track != self.mic_track {
                    self.chain.reset_clock();
                }
                self.mic_track = track;
            }
            VoiceCommand::Muted(muted) => self.muted = muted,
            VoiceCommand::PushToTalk(down) => self.push_to_talk = down,
            VoiceCommand::Deafened(deafened) => self.deafened = deafened,
            VoiceCommand::Volume(user, volume) => self.mixer.set_volume(user, volume),
            VoiceCommand::Listen { user, units } => {
                self.remotes.insert(user, Remote { units });
            }
            VoiceCommand::Forget(user) => {
                self.remotes.remove(&user);
                self.mixer.remove(user);
            }
            VoiceCommand::ForgetAll => {
                self.remotes.clear();
                self.mixer.clear();
            }
            VoiceCommand::Loopback(on) => self.loopback = on,
        }
    }

    fn receive(&mut self) {
        for (user, remote) in &mut self.remotes {
            while let Ok(unit) = remote.units.try_recv() {
                if let Err(e) = self
                    .mixer
                    .insert(*user, unit.timestamp, &unit.data, self.played)
                {
                    warn!("dropping audio from user {}: {e}", user.0);
                }
            }
        }
    }

    /// Mixes 10 ms, feeds it to the echo canceller and queues it for the speakers.
    fn play(&mut self, loopback: &[f32; PULL]) {
        let mut mixed = [0.0f32; PULL];
        if !self.deafened {
            self.mixer.mix(&mut mixed);
        }
        if self.loopback {
            for (m, l) in mixed.iter_mut().zip(loopback) {
                *m = (*m + l).clamp(-1.0, 1.0);
            }
        }
        if let Err(e) = self.chain.render(&mixed) {
            warn!("echo canceller reference: {e}");
        }
        self.played += PULL as u64;
        let out: &[f32] = match &mut self.to_device {
            Some(resampler) => {
                resampler.resample(&mixed, &mut self.device_out);
                &self.device_out
            }
            None => &mixed,
        };
        for sample in out {
            if self.devices.playback.push(*sample).is_err() {
                break;
            }
        }
    }

    /// Takes 10 ms of mic, processes and encodes it. Returns the processed audio.
    fn capture(&mut self) -> [f32; PULL] {
        for slot in self.device_in.iter_mut() {
            *slot = self.devices.capture.pop().unwrap_or(0.0);
        }
        let mut input = [0.0f32; PULL];
        match &mut self.from_device {
            Some(resampler) => {
                resampler.resample(&self.device_in, &mut input);
            }
            None => input.copy_from_slice(&self.device_in[..PULL]),
        }
        let transmit = !self.muted && self.mic_track.is_some();
        self.chain.set_muted(!transmit);
        match self.chain.capture(&input, self.push_to_talk) {
            Ok(Some(packet)) => {
                if let Some(media_id) = self.mic_track {
                    let unit = Outgoing {
                        media_id,
                        data: Bytes::copy_from_slice(packet.data),
                        timestamp: packet.timestamp,
                        keyframe: false,
                        discardable: false,
                        enqueued: Instant::now(),
                    };
                    self.engine.send(Kind::Audio, unit);
                }
            }
            Ok(None) => {}
            Err(e) => warn!("mic processing: {e}"),
        }
        *self.chain.processed()
    }

    fn publish_levels(&self) {
        let mut levels = self.levels.lock().unwrap_or_else(PoisonError::into_inner);
        levels.mic_dbfs = self.chain.level_dbfs;
        levels.voice_probability = self.chain.voice_probability;
        levels.transmitting = self.chain.transmitting();
        levels.speaking = self.mixer.speaking().collect();
        levels.overruns = self
            .devices
            .counters
            .overruns
            .load(std::sync::atomic::Ordering::Relaxed);
        levels.underruns = self
            .devices
            .counters
            .underruns
            .load(std::sync::atomic::Ordering::Relaxed);
    }
}
