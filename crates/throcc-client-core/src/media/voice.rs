use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use sonora_common_audio::push_sinc_resampler::PushSincResampler;
use throcc_proto::ids::{MediaId, UserId};
use tokio::sync::mpsc;
use tracing::warn;

use super::audio::{CaptureChain, Gains, Mixer, PULL, SAMPLE_RATE};
use super::cues::{Cue, CuePlayer};
use super::devices::{self, DeviceError, Devices};
use super::engine::{Kind, MediaEngine, Outgoing, Received};
use super::jitter::JitterBuffer;
use super::share_audio::Playout;
use crate::lock;
use crate::settings::VoiceSettings;

/// Playback kept queued in the device ring, in 10 ms pulls: enough to ride out
/// scheduling hiccups, little enough to keep mouth-to-ear delay low.
const PLAYBACK_AHEAD: usize = 4;
/// One pull of audio.
const PULL_TIME: Duration = Duration::from_millis(10);
const LEAST_WAIT: Duration = Duration::from_millis(1);
const LEVELS_EVERY: Duration = Duration::from_millis(100);
/// How long the mic is processed outside a call after the UI last read its level.
const METER_LINGER: Duration = Duration::from_secs(1);
const DEVICE_COUNTERS_EVERY: Duration = Duration::from_secs(10);

/// What a server session tells the audio thread about its call.
pub(crate) enum CallCommand {
    /// Sends the mic through this media engine on this track, or nowhere. A new track
    /// restarts its media clock.
    SendOn(Option<(Arc<MediaEngine>, MediaId)>),
    Muted(bool),
    Deafened(bool),
    Volume(UserId, f32),
    /// Sets the gain of `user`'s share audio, kept for shares they start later.
    ShareVolume(UserId, f32),
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
}

enum Command {
    Settings(VoiceSettings),
    /// Closes the open devices, then opens these, `None` being the system default.
    OpenDevices {
        input: Option<String>,
        output: Option<String>,
        opened: std_mpsc::Sender<Result<(), DeviceError>>,
    },
    /// Plays the processed mic back locally, for the settings screen's mic test.
    MicTest(bool),
    PushToTalk(bool),
    /// Plays a cue at the cue volume, heard even while deafened.
    Cue(Cue),
    Call(u64, CallCommand),
    EndCall(u64),
}

/// The processed mic level, and whether the transmit gate lets it through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicLevel {
    pub dbfs: f32,
    pub gate_open: bool,
}

impl Default for MicLevel {
    fn default() -> Self {
        Self {
            dbfs: f32::NEG_INFINITY,
            gate_open: false,
        }
    }
}

/// What a call shows about its audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CallLevels {
    /// Whether our last 20 ms went out on the call.
    pub transmitting: bool,
    pub speaking: HashMap<UserId, bool>,
}

#[derive(Default)]
struct Levels {
    mic: MicLevel,
    call: CallLevels,
    mic_read_at: Option<Instant>,
}

/// The app-wide audio thread: mic processing, the mix and playback, with or without a call.
/// A server session plays and sends through a [`CallAudio`].
pub struct AudioEngine {
    commands: std_mpsc::Sender<Command>,
    levels: Arc<Mutex<Levels>>,
    calls: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

impl AudioEngine {
    /// Starts the audio thread with no devices open.
    pub fn start(settings: VoiceSettings) -> anyhow::Result<Self> {
        let chain = CaptureChain::new(&settings)?;
        let (commands, rx) = std_mpsc::channel();
        let levels: Arc<Mutex<Levels>> = Arc::default();
        let worker = Worker::new(chain, settings, levels.clone());
        let thread = std::thread::Builder::new()
            .name("throcc-audio".into())
            .spawn(move || worker.run(rx))?;
        Ok(Self {
            commands,
            levels,
            calls: AtomicU64::new(0),
            thread: Some(thread),
        })
    }

    /// Plays and captures through these devices, `None` being the system default, once the
    /// ones open before are closed. On an error the engine runs without devices.
    pub fn open_devices(&self, input: Option<&str>, output: Option<&str>) -> anyhow::Result<()> {
        let (opened, result) = std_mpsc::channel();
        self.send(Command::OpenDevices {
            input: input.map(str::to_owned),
            output: output.map(str::to_owned),
            opened,
        });
        Ok(result.recv()??)
    }

    pub fn apply(&self, settings: VoiceSettings) {
        self.send(Command::Settings(settings));
    }

    pub fn mic_test(&self, on: bool) {
        self.send(Command::MicTest(on));
    }

    pub fn push_to_talk(&self, down: bool) {
        self.send(Command::PushToTalk(down));
    }

    pub fn cue(&self, cue: Cue) {
        self.send(Command::Cue(cue));
    }

    /// The mic level and gate, refreshed every 100 ms while a mic is open. Outside a call
    /// the mic is processed only while this keeps being called.
    pub fn mic_level(&self) -> MicLevel {
        let mut levels = lock(&self.levels);
        levels.mic_read_at = Some(Instant::now());
        levels.mic
    }

    /// A handle for a new call, which takes the call audio over from any earlier one.
    pub(crate) fn call(self: &Arc<Self>) -> CallAudio {
        CallAudio {
            id: self.calls.fetch_add(1, Ordering::Relaxed) + 1,
            engine: self.clone(),
        }
    }

    fn send(&self, command: Command) {
        self.commands.send(command).ok();
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let (closed, _) = std_mpsc::channel();
        self.commands = closed;
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// One server session's way into the audio thread. Commands from a call that a newer one
/// took over are ignored, and dropping the handle ends its call's audio.
pub(crate) struct CallAudio {
    engine: Arc<AudioEngine>,
    id: u64,
}

impl CallAudio {
    pub(crate) fn command(&self, command: CallCommand) {
        self.engine.send(Command::Call(self.id, command));
    }

    pub(crate) fn cue(&self, cue: Cue) {
        self.engine.cue(cue);
    }

    pub(crate) fn levels(&self) -> CallLevels {
        lock(&self.engine.levels).call.clone()
    }

    /// A handle for the next call on the same engine.
    pub(crate) fn renew(&self) -> CallAudio {
        self.engine.call()
    }
}

impl Drop for CallAudio {
    fn drop(&mut self) {
        self.engine.send(Command::EndCall(self.id));
    }
}

struct ShareAudio {
    units: mpsc::Receiver<Received>,
    jitter: JitterBuffer,
    playout: Arc<Playout>,
}

impl Drop for ShareAudio {
    fn drop(&mut self) {
        self.playout.set(None);
    }
}

/// Everything the audio thread holds for the current call.
struct CallState {
    id: u64,
    mic: Option<(Arc<MediaEngine>, MediaId)>,
    muted: bool,
    deafened: bool,
    remotes: HashMap<UserId, mpsc::Receiver<Received>>,
    mixer: Mixer,
    shares: HashMap<UserId, ShareAudio>,
    share_volumes: Gains,
}

impl CallState {
    fn new(id: u64) -> Self {
        Self {
            id,
            mic: None,
            muted: true,
            deafened: false,
            remotes: HashMap::new(),
            mixer: Mixer::default(),
            shares: HashMap::new(),
            share_volumes: Gains::default(),
        }
    }
}

/// The open devices, with resamplers between their rates and [`SAMPLE_RATE`].
struct DeviceIo {
    devices: Devices,
    /// Left and right resamplers to the output device's rate.
    to_device: Option<[PushSincResampler; 2]>,
    from_device: Option<PushSincResampler>,
    device_in: Vec<f32>,
    /// One pull at the output device's rate, left and right interleaved.
    device_out: Vec<f32>,
    device_channel: [Vec<f32>; 2],
    /// The device counters already logged: stream errors, overruns and underruns.
    counters_logged: [u64; 3],
    counters_logged_at: Option<Instant>,
}

impl DeviceIo {
    fn new(devices: Devices) -> Self {
        let per_pull = |rate: u32| (rate / 100) as usize;
        let out_pull = per_pull(devices.output_rate);
        Self {
            to_device: (devices.output_rate != SAMPLE_RATE)
                .then(|| std::array::from_fn(|_| PushSincResampler::new(PULL, out_pull))),
            from_device: (devices.input_rate != SAMPLE_RATE)
                .then(|| PushSincResampler::new(per_pull(devices.input_rate), PULL)),
            device_in: vec![0.0; per_pull(devices.input_rate)],
            device_out: vec![0.0; out_pull * 2],
            device_channel: [vec![0.0; out_pull], vec![0.0; out_pull]],
            devices,
            counters_logged: [0; 3],
            counters_logged_at: None,
        }
    }

    fn queued_playback(&self) -> usize {
        let capacity = self.devices.playback.buffer().capacity();
        (capacity - self.devices.playback.slots()) / self.device_out.len().max(1)
    }

    fn wants_playback(&self) -> bool {
        self.devices.playback.slots() >= self.device_out.len() * 2
            && self.queued_playback() < PLAYBACK_AHEAD
    }

    fn has_capture(&self) -> bool {
        self.devices.capture.slots() >= self.device_in.len()
    }

    /// Roughly how long until the mic has 10 ms ready or the speakers have room for 10 ms.
    fn next_pull_in(&self) -> Duration {
        let wait = |samples: usize, per_pull: usize| {
            PULL_TIME.mul_f64(samples as f64 / per_pull.max(1) as f64)
        };
        let captured = self.devices.capture.slots();
        let capture = wait(
            self.device_in.len().saturating_sub(captured),
            self.device_in.len(),
        );
        let queued = self.devices.playback.buffer().capacity() - self.devices.playback.slots();
        let over_ahead = (queued + 1).saturating_sub(PLAYBACK_AHEAD * self.device_out.len());
        // The speakers drain the queue one device period at a time, so the queue sits just
        // over PLAYBACK_AHEAD pulls until the next period.
        let playback = wait(over_ahead, self.device_out.len()).max(PULL_TIME / 2);
        capture.min(playback)
    }

    /// Drops 10 ms from the mic unprocessed.
    fn discard_capture(&mut self) {
        if let Ok(chunk) = self.devices.capture.read_chunk(self.device_in.len()) {
            chunk.commit_all();
        }
    }

    /// Takes 10 ms from the mic at [`SAMPLE_RATE`].
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
        input
    }

    /// Queues 10 ms of stereo at [`SAMPLE_RATE`] for the speakers.
    fn play(&mut self, stereo: &[f32; PULL * 2]) {
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
            None => stereo,
        };
        if self.devices.playback.slots() < out.len() {
            return;
        }
        for sample in out {
            self.devices.playback.push(*sample).ok();
        }
    }

    /// Logs new device counter values, at most every [`DEVICE_COUNTERS_EVERY`].
    fn log_counters(&mut self) {
        if self
            .counters_logged_at
            .is_some_and(|at| at.elapsed() < DEVICE_COUNTERS_EVERY)
        {
            return;
        }
        let counters = &self.devices.counters;
        let watched = [
            (&counters.stream_errors, "stream errors"),
            (&counters.overruns, "mic samples dropped on a full ring"),
            (&counters.underruns, "speaker frames played as silence"),
        ];
        for ((counter, what), logged) in watched.into_iter().zip(&mut self.counters_logged) {
            let count = counter.load(Ordering::Relaxed);
            if count > *logged {
                warn!(
                    "the audio devices reported {} {what} ({count} in all)",
                    count - *logged
                );
                *logged = count;
                self.counters_logged_at = Some(Instant::now());
            }
        }
    }
}

struct Worker {
    io: Option<DeviceIo>,
    chain: CaptureChain,
    settings: VoiceSettings,
    cues: CuePlayer,
    call: CallState,
    levels: Arc<Mutex<Levels>>,
    levels_at: Instant,
    push_to_talk: bool,
    mic_test: bool,
    /// Whether the UI read the mic level within [`METER_LINGER`].
    meter_shown: bool,
    /// The last 10 ms of processed mic, which the mic test plays back.
    last_capture: [f32; PULL],
    /// Samples played, the local 48 kHz clock that packet arrivals are stamped with.
    played: u64,
}

impl Worker {
    fn new(chain: CaptureChain, settings: VoiceSettings, levels: Arc<Mutex<Levels>>) -> Self {
        Self {
            io: None,
            chain,
            settings,
            cues: CuePlayer::default(),
            call: CallState::new(0),
            levels,
            levels_at: Instant::now(),
            push_to_talk: false,
            mic_test: false,
            meter_shown: false,
            last_capture: [0.0; PULL],
            played: 0,
        }
    }

    fn run(mut self, commands: std_mpsc::Receiver<Command>) {
        loop {
            loop {
                match commands.try_recv() {
                    Ok(command) => self.apply(command),
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => return,
                }
            }
            self.receive();
            let busy = match self.io.take() {
                Some(mut io) => {
                    let busy = self.play_and_capture(&mut io);
                    self.io = Some(io);
                    busy
                }
                None => false,
            };
            if self.levels_at.elapsed() >= LEVELS_EVERY {
                self.levels_at = Instant::now();
                self.publish_levels();
                if let Some(io) = &mut self.io {
                    io.log_counters();
                }
            }
            if busy {
                continue;
            }
            let wait = match &self.io {
                Some(io) => io.next_pull_in().clamp(LEAST_WAIT, PULL_TIME),
                None => LEVELS_EVERY,
            };
            match commands.recv_timeout(wait) {
                Ok(command) => self.apply(command),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Settings(settings) => {
                self.chain.apply(&settings);
                self.settings = settings;
            }
            Command::OpenDevices {
                input,
                output,
                opened,
            } => {
                self.io = None;
                self.chain.reset_levels();
                self.last_capture = [0.0; PULL];
                let result = devices::open(input.as_deref(), output.as_deref());
                let result = result.map(|devices| self.io = Some(DeviceIo::new(devices)));
                opened.send(result).ok();
            }
            Command::MicTest(on) => self.mic_test = on,
            Command::PushToTalk(down) => self.push_to_talk = down,
            Command::Cue(cue) => {
                if self.settings.cue_volume > 0.0 {
                    self.cues.play(cue, self.settings.cue_volume);
                }
            }
            Command::Call(id, command) => {
                if id < self.call.id {
                    return;
                }
                if id > self.call.id {
                    self.call = CallState::new(id);
                }
                self.apply_call(command);
            }
            Command::EndCall(id) => {
                if id >= self.call.id {
                    self.call = CallState::new(id);
                }
            }
        }
    }

    fn apply_call(&mut self, command: CallCommand) {
        let call = &mut self.call;
        match command {
            CallCommand::SendOn(mic) => {
                let track = |mic: &Option<(Arc<MediaEngine>, MediaId)>| mic.as_ref().map(|m| m.1);
                if track(&mic) != track(&call.mic) {
                    self.chain.reset_clock();
                }
                call.mic = mic;
            }
            CallCommand::Muted(muted) => call.muted = muted,
            CallCommand::Deafened(deafened) => call.deafened = deafened,
            CallCommand::Volume(user, volume) => call.mixer.set_volume(user, volume),
            CallCommand::ShareVolume(user, volume) => call.share_volumes.set(user, volume),
            CallCommand::Listen { user, units } => {
                call.remotes.insert(user, units);
            }
            CallCommand::Forget(user) => {
                call.remotes.remove(&user);
                call.mixer.remove(user);
            }
            CallCommand::ListenShare {
                user,
                units,
                playout,
            } => match JitterBuffer::new(ropus::Channels::Stereo) {
                Ok(jitter) => {
                    call.shares.insert(
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
            CallCommand::ForgetShare(user) => {
                call.shares.remove(&user);
            }
            CallCommand::ForgetAll => {
                call.remotes.clear();
                call.mixer.clear();
                call.shares.clear();
            }
        }
    }

    fn receive(&mut self) {
        let call = &mut self.call;
        for (user, units) in &mut call.remotes {
            while let Ok(unit) = units.try_recv() {
                if let Err(e) = call
                    .mixer
                    .insert(*user, unit.timestamp, unit.data, self.played)
                {
                    warn!("dropping audio from user {}: {e}", user.0);
                }
            }
        }
        for share in call.shares.values_mut() {
            while let Ok(unit) = share.units.try_recv() {
                share.jitter.insert(unit.timestamp, unit.data, self.played);
            }
        }
    }

    /// Plays and captures 10 ms on whichever of `io`'s devices is ready. True if one was.
    fn play_and_capture(&mut self, io: &mut DeviceIo) -> bool {
        let playing = io.wants_playback();
        if playing {
            self.play(io);
        }
        let capturing = io.has_capture();
        if capturing && self.processes_mic() {
            let input = io.capture();
            self.last_capture = self.capture(&input);
        } else if capturing {
            io.discard_capture();
            self.chain.reset_levels();
        }
        playing || capturing
    }

    /// Mixes 10 ms of voices, shares, the mic test and cues, feeds it to the echo canceller
    /// and queues it on `io`.
    fn play(&mut self, io: &mut DeviceIo) {
        let mut voices = [0.0f32; PULL];
        let mut stereo = [0.0f32; PULL * 2];
        self.mix_shares(io.queued_playback(), &mut stereo);
        self.call.mixer.mix(&mut voices);
        if self.call.deafened {
            stereo.fill(0.0);
            voices.fill(0.0);
        }
        if self.mic_test {
            for (m, l) in voices.iter_mut().zip(&self.last_capture) {
                *m = (*m + l).clamp(-1.0, 1.0);
            }
        }
        self.cues.mix(&mut voices);
        let output_volume = self.settings.output_volume;
        for (pair, voice) in stereo.as_chunks_mut::<2>().0.iter_mut().zip(voices) {
            for sample in pair {
                *sample = ((*sample + voice) * output_volume).clamp(-1.0, 1.0);
            }
        }
        let mut reference = [0.0f32; PULL];
        for (r, [left, right]) in reference.iter_mut().zip(stereo.as_chunks::<2>().0) {
            *r = (left + right) * 0.5;
        }
        if self.processes_mic()
            && let Err(e) = self.chain.render(&reference)
        {
            warn!("echo canceller reference: {e}");
        }
        self.played += PULL as u64;
        io.play(&stereo);
    }

    /// Adds every share's next 10 ms into `out`, and publishes the audio each share has
    /// reaching the speakers once the `queued` pulls ahead of it have played.
    fn mix_shares(&mut self, queued: usize, out: &mut [f32; PULL * 2]) {
        let queued = (queued * PULL) as u32;
        let mut pull = [0.0f32; PULL * 2];
        for (user, share) in &mut self.call.shares {
            share.jitter.pull(&mut pull);
            let volume = self.call.share_volumes.get(*user);
            for (o, s) in out.iter_mut().zip(&pull) {
                *o += s * volume;
            }
            let heard = share
                .jitter
                .playout_timestamp()
                .map(|ts| ts.wrapping_sub(queued));
            share.playout.set(heard);
        }
    }

    /// Processes 10 ms of mic and sends it on the call. Returns the processed audio.
    fn capture(&mut self, input: &[f32; PULL]) -> [f32; PULL] {
        let transmit = !self.call.muted && self.call.mic.is_some();
        self.chain.set_muted(!transmit);
        match self.chain.capture(input, self.push_to_talk) {
            Ok(Some(packet)) => {
                if let Some((engine, media_id)) = &self.call.mic {
                    let data = Bytes::copy_from_slice(packet.data);
                    let unit = Outgoing::new(*media_id, data, packet.timestamp, false);
                    engine.send(Kind::Audio, unit);
                }
            }
            Ok(None) => {}
            Err(e) => warn!("mic processing: {e}"),
        }
        *self.chain.processed()
    }

    /// Whether anything uses the processed mic: a call, the mic test or the level meter.
    fn processes_mic(&self) -> bool {
        self.call.mic.is_some() || self.mic_test || self.meter_shown
    }

    fn publish_levels(&mut self) {
        let mut levels = lock(&self.levels);
        self.meter_shown = levels
            .mic_read_at
            .is_some_and(|at| at.elapsed() < METER_LINGER);
        levels.mic = MicLevel {
            dbfs: self.chain.level_dbfs(),
            gate_open: self.chain.gate_open(),
        };
        levels.call.transmitting = self.chain.transmitting();
        levels.call.speaking.clear();
        levels.call.speaking.extend(self.call.mixer.speaking());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> Worker {
        let settings = VoiceSettings::default();
        Worker::new(
            CaptureChain::new(&settings).unwrap(),
            settings,
            Arc::default(),
        )
    }

    #[test]
    fn a_newer_call_takes_over_and_an_older_one_can_neither_change_nor_end_it() {
        let mut worker = worker();
        worker.apply(Command::Call(1, CallCommand::Deafened(true)));
        assert!(worker.call.deafened);
        worker.apply(Command::Call(2, CallCommand::Muted(false)));
        assert!(
            !worker.call.deafened,
            "the newer call kept the older one's state"
        );
        worker.apply(Command::Call(1, CallCommand::Muted(true)));
        worker.apply(Command::EndCall(1));
        assert_eq!(worker.call.id, 2);
        assert!(!worker.call.muted, "the older call changed the newer one");
        worker.apply(Command::EndCall(2));
        assert!(worker.call.muted, "ending the call left it unmuted");
    }

    #[test]
    fn settings_mic_test_and_cues_need_no_call_or_devices() {
        let mut worker = worker();
        let quiet = VoiceSettings {
            cue_volume: 0.25,
            output_volume: 0.5,
            ..VoiceSettings::default()
        };
        worker.apply(Command::Settings(quiet));
        worker.apply(Command::MicTest(true));
        worker.apply(Command::Cue(Cue::Join));
        assert_eq!(worker.settings, quiet);
        assert!(worker.mic_test);
        let mut out = [0.0f32; PULL];
        worker.cues.mix(&mut out);
        assert!(out.iter().any(|s| *s != 0.0), "the cue didn't play");
    }
}
