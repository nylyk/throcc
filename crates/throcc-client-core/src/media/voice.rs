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
use super::jitter::{JitterBuffer, PULL};
use super::share_audio::Playout;

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
    /// Plays a share's stereo audio and reports where its playback is in `playout`.
    ListenShare {
        user: UserId,
        units: mpsc::Receiver<Received>,
        playout: Arc<Playout>,
    },
    ForgetShare(UserId),
    ForgetAll,
    /// Plays the processed mic back locally, for the settings screen's mic test.
    Loopback(bool),
}

/// What the UI shows about audio, refreshed every 100 ms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Levels {
    pub mic_dbfs: f32,
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
        let worker = Worker::new(
            devices,
            chain,
            settings.output_volume,
            engine,
            levels.clone(),
        );
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

struct ShareAudio {
    units: mpsc::Receiver<Received>,
    jitter: JitterBuffer,
    playout: Arc<Playout>,
}

struct Worker {
    devices: Devices,
    chain: CaptureChain,
    mixer: Mixer,
    remotes: HashMap<UserId, Remote>,
    shares: HashMap<UserId, ShareAudio>,
    engine: Arc<MediaEngine>,
    levels: Arc<Mutex<Levels>>,
    /// Left and right resamplers to the output device's rate.
    to_device: Option<[PushSincResampler; 2]>,
    from_device: Option<PushSincResampler>,
    device_in: Vec<f32>,
    /// One pull at the output device's rate, left and right interleaved.
    device_out: Vec<f32>,
    device_channel: [Vec<f32>; 2],
    mic_track: Option<MediaId>,
    muted: bool,
    push_to_talk: bool,
    deafened: bool,
    loopback: bool,
    output_volume: f32,
    /// Samples played, the local 48 kHz clock that packet arrivals are stamped with.
    played: u64,
}

impl Worker {
    fn new(
        devices: Devices,
        chain: CaptureChain,
        output_volume: f32,
        engine: Arc<MediaEngine>,
        levels: Arc<Mutex<Levels>>,
    ) -> Self {
        let per_pull = |rate: u32| (rate / 100) as usize;
        let resampler = |from: u32, to: u32| {
            (from != to).then(|| PushSincResampler::new(per_pull(from), per_pull(to)))
        };
        let out_pull = per_pull(devices.output_rate);
        Self {
            to_device: (devices.output_rate != SAMPLE_RATE)
                .then(|| std::array::from_fn(|_| PushSincResampler::new(PULL, out_pull))),
            from_device: resampler(devices.input_rate, SAMPLE_RATE),
            device_in: vec![0.0; per_pull(devices.input_rate)],
            device_out: vec![0.0; out_pull * 2],
            device_channel: [vec![0.0; out_pull], vec![0.0; out_pull]],
            devices,
            chain,
            mixer: Mixer::default(),
            remotes: HashMap::new(),
            shares: HashMap::new(),
            engine,
            levels,
            mic_track: None,
            muted: true,
            push_to_talk: false,
            deafened: false,
            loopback: false,
            output_volume,
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
            VoiceCommand::Settings(settings) => {
                self.chain.apply(&settings);
                self.output_volume = settings.output_volume;
            }
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
            VoiceCommand::ListenShare {
                user,
                units,
                playout,
            } => match JitterBuffer::new(ropus::Channels::Stereo) {
                Ok(jitter) => {
                    self.shares.insert(
                        user,
                        ShareAudio {
                            units,
                            jitter,
                            playout,
                        },
                    );
                }
                Err(e) => warn!("share audio decoder: {e}"),
            },
            VoiceCommand::ForgetShare(user) => {
                if let Some(share) = self.shares.remove(&user) {
                    share.playout.set(None);
                }
            }
            VoiceCommand::ForgetAll => {
                self.remotes.clear();
                self.mixer.clear();
                for share in self.shares.values() {
                    share.playout.set(None);
                }
                self.shares.clear();
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
        for share in self.shares.values_mut() {
            while let Ok(unit) = share.units.try_recv() {
                share.jitter.insert(unit.timestamp, &unit.data, self.played);
            }
        }
    }

    /// Mixes 10 ms of voices and shares, feeds it to the echo canceller and queues it for
    /// the speakers.
    fn play(&mut self, loopback: &[f32; PULL]) {
        let mut voices = [0.0f32; PULL];
        let mut stereo = [0.0f32; PULL * 2];
        self.mix_shares(&mut stereo);
        if self.deafened {
            stereo.fill(0.0);
        } else {
            self.mixer.mix(&mut voices);
        }
        if self.loopback {
            for (m, l) in voices.iter_mut().zip(loopback) {
                *m = (*m + l).clamp(-1.0, 1.0);
            }
        }
        for (pair, voice) in stereo.as_chunks_mut::<2>().0.iter_mut().zip(voices) {
            for sample in pair {
                *sample = ((*sample + voice) * self.output_volume).clamp(-1.0, 1.0);
            }
        }
        let mut reference = [0.0f32; PULL];
        for (r, [left, right]) in reference.iter_mut().zip(stereo.as_chunks::<2>().0) {
            *r = (left + right) * 0.5;
        }
        if let Err(e) = self.chain.render(&reference) {
            warn!("echo canceller reference: {e}");
        }
        self.played += PULL as u64;
        let out: &[f32] = match &mut self.to_device {
            Some(resamplers) => {
                for (channel, (resampler, out)) in resamplers
                    .iter_mut()
                    .zip(&mut self.device_channel)
                    .enumerate()
                {
                    let input: [f32; PULL] = std::array::from_fn(|i| stereo[i * 2 + channel]);
                    resampler.resample(&input, out);
                }
                let [left, right] = &self.device_channel;
                for ((pair, l), r) in self
                    .device_out
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(left)
                    .zip(right)
                {
                    *pair = [*l, *r];
                }
                &self.device_out
            }
            None => &stereo,
        };
        if self.devices.playback.slots() < out.len() {
            return;
        }
        for sample in out {
            self.devices.playback.push(*sample).ok();
        }
    }

    /// Adds every share's next 10 ms into `out`, and publishes the audio each share has
    /// reaching the speakers once what is queued ahead of it has played.
    fn mix_shares(&mut self, out: &mut [f32; PULL * 2]) {
        let queued = (self.queued_playback() * PULL) as u32;
        let mut pull = [0.0f32; PULL * 2];
        for share in self.shares.values_mut() {
            share.jitter.pull(&mut pull);
            for (o, s) in out.iter_mut().zip(&pull) {
                *o += s;
            }
            let heard = share
                .jitter
                .playout_timestamp()
                .map(|ts| ts.wrapping_sub(queued));
            share.playout.set(heard);
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
