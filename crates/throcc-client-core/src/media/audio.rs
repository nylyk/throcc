use std::collections::HashMap;

use ropus::{Application, Bitrate, Channels, Encoder, FrameDuration, InbandFec};
use sonora::config::{
    AdaptiveDigital, EchoCanceller, GainController2, HighPassFilter, NoiseSuppression,
};
use sonora::{AudioProcessing, Config, StreamConfig};
use throcc_proto::ids::UserId;

pub use sonora::config::NoiseSuppressionLevel;

use super::jitter::{FRAME, JitterBuffer, PULL};

pub const SAMPLE_RATE: u32 = 48_000;
/// Voice activation keeps sending this long after speech stops.
const HANGOVER_PULLS: u32 = 30;
const MAX_PACKET: usize = 1500;
/// Processed mic level (dBFS) above which voice activation opens, unless configured otherwise.
pub const DEFAULT_GATE_DBFS: f32 = -50.0;
/// A decoded level above this (dBFS) counts as speaking for the UI's ring.
const SPEAKING_DBFS: f32 = -45.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transmit {
    VoiceActivity { threshold_dbfs: f32 },
    PushToTalk,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceSettings {
    pub echo_cancellation: bool,
    pub noise_suppression: Option<NoiseSuppressionLevel>,
    pub automatic_gain: bool,
    pub transmit: Transmit,
    pub kbps: u32,
    /// Gain on the mic before processing, 0 to 2.
    pub input_volume: f32,
    /// Gain on everything played, 0 to 2.
    pub output_volume: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            echo_cancellation: true,
            noise_suppression: Some(NoiseSuppressionLevel::Moderate),
            automatic_gain: true,
            transmit: Transmit::VoiceActivity {
                threshold_dbfs: DEFAULT_GATE_DBFS,
            },
            kbps: 64,
            input_volume: 1.0,
            output_volume: 1.0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio processing: {0:?}")]
    Processing(sonora::Error),
    #[error("Opus encoder: {0}")]
    Encoder(String),
    #[error("Opus decoder: {0}")]
    Decoder(String),
}

/// One encoded 20 ms mic frame.
pub struct Packet<'a> {
    pub timestamp: u32,
    pub data: &'a [u8],
}

/// Mic input to Opus packets: 10 ms frames through AEC3, noise suppression and AGC2, then
/// a level or push-to-talk gate, then 20 ms Opus frames with FEC and DTX.
/// The media clock runs on while gated, so timestamps stay real time.
pub struct CaptureChain {
    apm: AudioProcessing,
    encoder: Encoder,
    transmit: Transmit,
    input_volume: f32,
    /// Echo cancellation, noise suppression and gain control as last applied.
    processing: (bool, Option<NoiseSuppressionLevel>, bool),
    processed: [f32; PULL],
    frame: [f32; FRAME],
    frame_len: usize,
    frame_open: bool,
    packet: [u8; MAX_PACKET],
    clock: u32,
    hangover: u32,
    muted: bool,
    transmitting: bool,
    /// Level of the last 10 ms after processing, in dBFS.
    pub level_dbfs: f32,
}

impl CaptureChain {
    pub fn new(settings: &VoiceSettings) -> Result<Self, AudioError> {
        let mut apm = AudioProcessing::builder()
            .capture_config(StreamConfig::new(SAMPLE_RATE, 1))
            .render_config(StreamConfig::new(SAMPLE_RATE, 1))
            .build();
        apm.apply_config(processing_config(settings));
        let encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(settings.kbps * 1000))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(5)
            .dtx(true)
            .build()
            .map_err(|e| AudioError::Encoder(e.to_string()))?;
        Ok(Self {
            apm,
            encoder,
            transmit: settings.transmit,
            input_volume: settings.input_volume,
            processing: processing_choice(settings),
            processed: [0.0; PULL],
            frame: [0.0; FRAME],
            frame_len: 0,
            frame_open: false,
            packet: [0; MAX_PACKET],
            clock: 0,
            hangover: 0,
            muted: false,
            transmitting: false,
            level_dbfs: f32::NEG_INFINITY,
        })
    }

    pub fn apply(&mut self, settings: &VoiceSettings) {
        let processing = processing_choice(settings);
        if processing != self.processing {
            self.apm.apply_config(processing_config(settings));
            self.processing = processing;
        }
        self.transmit = settings.transmit;
        self.input_volume = settings.input_volume;
        self.encoder
            .set_bitrate(Bitrate::Bits(settings.kbps * 1000))
            .ok();
    }

    /// Closes the gate while keeping processing and the media clock running.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Restarts the media clock, for a new track.
    pub fn reset_clock(&mut self) {
        self.clock = 0;
        self.frame_len = 0;
        self.frame_open = false;
    }

    /// Whether the last 20 ms went out.
    pub fn transmitting(&self) -> bool {
        self.transmitting
    }

    /// Feeds 10 ms of what the speakers play, the echo canceller's reference.
    pub fn render(&mut self, played: &[f32; PULL]) -> Result<(), AudioError> {
        let mut out = [0.0f32; PULL];
        self.apm
            .process_render_f32(&[played], &mut [&mut out])
            .map_err(AudioError::Processing)
    }

    /// Processes 10 ms of mic input. Every 20 ms that the gate was open, returns a packet.
    pub fn capture(
        &mut self,
        input: &[f32; PULL],
        push_to_talk: bool,
    ) -> Result<Option<Packet<'_>>, AudioError> {
        let input = input.map(|sample| sample * self.input_volume);
        self.apm
            .process_capture_f32(&[&input], &mut [&mut self.processed])
            .map_err(AudioError::Processing)?;
        self.level_dbfs = dbfs(&self.processed);

        let open = match self.transmit {
            Transmit::PushToTalk => push_to_talk,
            Transmit::VoiceActivity { threshold_dbfs } => {
                if self.level_dbfs > threshold_dbfs {
                    self.hangover = HANGOVER_PULLS;
                } else {
                    self.hangover = self.hangover.saturating_sub(1);
                }
                self.hangover > 0
            }
        };
        self.frame_open |= open && !self.muted;
        self.frame[self.frame_len..self.frame_len + PULL].copy_from_slice(&self.processed);
        self.frame_len += PULL;
        if self.frame_len < FRAME {
            return Ok(None);
        }
        self.frame_len = 0;
        let timestamp = self.clock;
        self.clock = self.clock.wrapping_add(FRAME as u32);
        self.transmitting = std::mem::take(&mut self.frame_open);
        if !self.transmitting {
            return Ok(None);
        }
        let len = self
            .encoder
            .encode_float(&self.frame, &mut self.packet)
            .map_err(|e| AudioError::Encoder(e.to_string()))?;
        Ok(Some(Packet {
            timestamp,
            data: &self.packet[..len],
        }))
    }

    /// The last 10 ms after processing, before the gate.
    pub fn processed(&self) -> &[f32; PULL] {
        &self.processed
    }
}

fn processing_choice(settings: &VoiceSettings) -> (bool, Option<NoiseSuppressionLevel>, bool) {
    (
        settings.echo_cancellation,
        settings.noise_suppression,
        settings.automatic_gain,
    )
}

fn processing_config(settings: &VoiceSettings) -> Config {
    Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: settings.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: settings.noise_suppression.map(|level| NoiseSuppression {
            level,
            ..Default::default()
        }),
        gain_controller2: settings.automatic_gain.then(|| GainController2 {
            adaptive_digital: Some(AdaptiveDigital::default()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Stream {
    jitter: JitterBuffer,
    volume: f32,
    level_dbfs: f32,
}

/// Every remote voice stream summed into one output: per-user volume, soft clip.
/// The result is what the speakers play and the echo canceller's reference.
pub struct Mixer {
    streams: HashMap<UserId, Stream>,
    volumes: HashMap<UserId, f32>,
    pull: [f32; PULL],
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            streams: HashMap::new(),
            volumes: HashMap::new(),
            pull: [0.0; PULL],
        }
    }
}

impl Mixer {
    pub fn insert(
        &mut self,
        user: UserId,
        timestamp: u32,
        payload: &[u8],
        arrival: u64,
    ) -> Result<(), AudioError> {
        let stream = match self.streams.entry(user) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(Stream {
                jitter: JitterBuffer::new(Channels::Mono)
                    .map_err(|e| AudioError::Decoder(e.to_string()))?,
                volume: self.volumes.get(&user).copied().unwrap_or(1.0),
                level_dbfs: f32::NEG_INFINITY,
            }),
        };
        stream.jitter.insert(timestamp, payload, arrival);
        Ok(())
    }

    pub fn remove(&mut self, user: UserId) {
        self.streams.remove(&user);
    }

    pub fn clear(&mut self) {
        self.streams.clear();
    }

    /// Sets `user`'s volume, kept for streams they start later.
    pub fn set_volume(&mut self, user: UserId, volume: f32) {
        let volume = volume.clamp(0.0, 2.0);
        self.volumes.insert(user, volume);
        if let Some(stream) = self.streams.get_mut(&user) {
            stream.volume = volume;
        }
    }

    /// Mixes the next 10 ms of every stream into `out`.
    pub fn mix(&mut self, out: &mut [f32; PULL]) {
        out.fill(0.0);
        for stream in self.streams.values_mut() {
            stream.jitter.pull(&mut self.pull);
            stream.level_dbfs = dbfs(&self.pull);
            for (o, s) in out.iter_mut().zip(&self.pull) {
                *o += s * stream.volume;
            }
        }
        for o in out.iter_mut() {
            *o = soft_clip(*o);
        }
    }

    pub fn speaking(&self) -> impl Iterator<Item = (UserId, bool)> + '_ {
        self.streams
            .iter()
            .map(|(user, s)| (*user, s.level_dbfs > SPEAKING_DBFS))
    }
}

/// Linear up to ±0.5, then bends smoothly towards ±1.
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.5;
    if x.abs() <= KNEE {
        x
    } else {
        let over = (x.abs() - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
    }
}

pub fn dbfs(samples: &[f32]) -> f32 {
    let mean = samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32;
    10.0 * mean.max(1e-12).log10()
}

#[cfg(test)]
pub(crate) mod tests {
    use ropus::{DecodeMode, Decoder};

    use super::*;

    /// Speech-like test signal: a gliding voiced harmonic series, shaped into syllables.
    pub fn speech(seed: u32, seconds: f32) -> Vec<f32> {
        let n = (seconds * SAMPLE_RATE as f32) as usize;
        let mut phase = 0.0f32;
        let mut noise = seed.wrapping_mul(2_654_435_761) | 1;
        let offset = seed as f32 * 0.37;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let f0 = 135.0 + 35.0 * (2.0 * std::f32::consts::PI * (0.6 + offset) * t).sin();
                phase += 2.0 * std::f32::consts::PI * f0 / SAMPLE_RATE as f32;
                let voiced: f32 = (1..30).map(|k| (phase * k as f32).sin() / k as f32).sum();
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                let hiss = (noise as f32 / u32::MAX as f32 - 0.5) * 0.3;
                let syllable = (2.0 * std::f32::consts::PI * (3.3 + offset) * t)
                    .sin()
                    .max(0.0)
                    .powf(0.7);
                let phrase = if (t + offset) % 2.5 < 2.1 { 1.0 } else { 0.0 };
                0.12 * syllable * phrase * (voiced + hiss)
            })
            .collect()
    }

    /// A small room heard through a cheap, slightly overdriven loudspeaker.
    fn echo(far: &[f32]) -> Vec<f32> {
        let taps = [(1440, 0.45), (1776, 0.2), (2352, 0.1), (3024, 0.05)];
        (0..far.len())
            .map(|i| {
                taps.iter()
                    .filter(|(delay, _)| i >= *delay)
                    .map(|(delay, gain)| gain * (2.0 * far[i - delay]).tanh() / 2.0)
                    .sum()
            })
            .collect()
    }

    fn energy(x: &[f32]) -> f64 {
        x.iter().map(|s| f64::from(*s).powi(2)).sum()
    }

    fn correlation(a: &[f32], b: &[f32]) -> f64 {
        let dot: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum();
        dot / (energy(a) * energy(b)).sqrt().max(1e-12)
    }

    /// Runs far-end playback and near-end capture through the chain; returns the processed mic.
    fn run_aec(far: &[f32], near: &[f32]) -> Vec<f32> {
        let settings = VoiceSettings {
            noise_suppression: None,
            automatic_gain: false,
            ..Default::default()
        };
        let mut chain = CaptureChain::new(&settings).unwrap();
        let mut out = Vec::with_capacity(near.len());
        for (f, n) in far
            .as_chunks::<PULL>()
            .0
            .iter()
            .zip(near.as_chunks::<PULL>().0)
        {
            chain.render(f).unwrap();
            chain.apm.set_stream_delay_ms(30).ok();
            chain.capture(n, false).unwrap();
            out.extend_from_slice(chain.processed());
        }
        out
    }

    #[test]
    fn echo_cancellation_converges_above_the_erle_floor() {
        let far = speech(1, 12.0);
        let near = echo(&far);
        let out = run_aec(&far, &near);
        let tail = near.len() / 2;
        let erle = 10.0 * (energy(&near[tail..]) / energy(&out[tail..]).max(1e-12)).log10();
        println!("ERLE after convergence: {erle:.1} dB");
        assert!(erle > 20.0, "ERLE {erle:.1} dB");
    }

    /// Compares the near-end talker processed during double talk with the same talker
    /// processed alone, so the chain's own filtering doesn't count as damage.
    #[test]
    fn near_end_speech_survives_double_talk() {
        let far = speech(1, 12.0);
        let talker = speech(7, 12.0);
        let start = 8 * SAMPLE_RATE as usize;
        let late_talker: Vec<f32> = (0..talker.len())
            .map(|i| if i >= start { talker[i] } else { 0.0 })
            .collect();
        let near: Vec<f32> = echo(&far)
            .iter()
            .zip(&late_talker)
            .map(|(e, t)| e + t)
            .collect();
        let alone = run_aec(&vec![0.0; far.len()], &late_talker);
        let both = run_aec(&far, &near);
        let (alone, both) = (&alone[start..], &both[start..]);
        let kept = 10.0 * (energy(both) / energy(alone)).log10();
        let similarity = correlation(both, alone);
        println!(
            "double talk: near-end kept at {kept:.1} dB, correlation {similarity:.2} with the talker alone"
        );
        assert!(kept > -8.0, "near-end speech lost {kept:.1} dB");
        assert!(
            similarity > 0.6,
            "near-end speech correlation {similarity:.2}"
        );
    }

    #[test]
    fn fec_recovers_a_dropped_frame_better_than_concealment() {
        let input = speech(3, 2.0);
        let mut encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(32_000))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(20)
            .build()
            .unwrap();
        let packets: Vec<Vec<u8>> = input
            .as_chunks::<FRAME>()
            .0
            .iter()
            .map(|frame| {
                let mut out = vec![0u8; MAX_PACKET];
                let n = encoder.encode_float(frame, &mut out).unwrap();
                out.truncate(n);
                out
            })
            .collect();
        let lost = packets
            .iter()
            .enumerate()
            .skip(20)
            .find(|(i, _)| dbfs(&input[i * FRAME..(i + 1) * FRAME]) > -30.0)
            .map(|(i, _)| i)
            .unwrap();

        let decode = |recover: Option<DecodeMode>| {
            let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono).unwrap();
            let mut frame = vec![0.0f32; FRAME];
            for (i, packet) in packets.iter().enumerate().take(lost + 1) {
                match (i == lost, recover) {
                    (true, Some(mode)) => {
                        decoder.decode_float(&packets[lost + 1], &mut frame, mode)
                    }
                    (true, None) => decoder.decode_float(&[], &mut frame, DecodeMode::Normal),
                    (false, _) => decoder.decode_float(packet, &mut frame, DecodeMode::Normal),
                }
                .unwrap();
            }
            frame
        };
        let reference = decode(Some(DecodeMode::Normal));
        let reference = {
            let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono).unwrap();
            let mut frame = vec![0.0f32; FRAME];
            for packet in packets.iter().take(lost + 1) {
                decoder
                    .decode_float(packet, &mut frame, DecodeMode::Normal)
                    .unwrap();
            }
            let _ = reference;
            frame
        };
        let snr = |x: &[f32]| {
            let error: Vec<f32> = x.iter().zip(&reference).map(|(a, b)| a - b).collect();
            10.0 * (energy(&reference) / energy(&error).max(1e-12)).log10()
        };
        let (fec, concealed) = (snr(&decode(Some(DecodeMode::Fec))), snr(&decode(None)));
        println!(
            "frame {lost}: FEC {fec:.1} dB, concealment {concealed:.1} dB against the lossless decode"
        );
        assert!(
            fec > concealed + 3.0 && fec > 3.0,
            "FEC {fec:.1} dB, concealment {concealed:.1} dB"
        );
    }

    #[test]
    fn voice_activation_gates_silence_and_keeps_the_clock_running() {
        let mut chain = CaptureChain::new(&VoiceSettings::default()).unwrap();
        let mut timestamps = Vec::new();
        let silence = [0.0f32; PULL];
        for _ in 0..100 {
            if let Some(packet) = chain.capture(&silence, false).unwrap() {
                timestamps.push(packet.timestamp);
            }
        }
        assert!(
            timestamps.is_empty(),
            "sent {} packets of silence",
            timestamps.len()
        );
        let voice = speech(5, 2.0);
        for frame in voice.as_chunks::<PULL>().0 {
            if let Some(packet) = chain.capture(frame, false).unwrap() {
                timestamps.push(packet.timestamp);
            }
        }
        assert!(!timestamps.is_empty(), "voice activation never opened");
        assert!(
            timestamps[0] >= 50 * FRAME as u32,
            "the clock stopped while gated"
        );
    }

    #[test]
    fn a_muted_input_volume_keeps_the_gate_shut() {
        let settings = VoiceSettings {
            input_volume: 0.0,
            ..VoiceSettings::default()
        };
        let mut chain = CaptureChain::new(&settings).unwrap();
        let voice = speech(5, 2.0);
        let sent = voice
            .as_chunks::<PULL>()
            .0
            .iter()
            .filter(|frame| chain.capture(frame, false).unwrap().is_some())
            .count();
        assert_eq!(sent, 0);
    }

    #[test]
    fn mixer_sums_streams_with_volume_and_soft_clips() {
        assert_eq!(soft_clip(0.3), 0.3);
        assert!(soft_clip(3.0) < 1.0 && soft_clip(3.0) > 0.9);
        assert_eq!(soft_clip(-3.0), -soft_clip(3.0));
        let mut mixer = Mixer::default();
        let mut out = [1.0f32; PULL];
        mixer.mix(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_volume_applies_to_streams_started_after_it() {
        let user = UserId(7);
        let mut mixer = Mixer::default();
        mixer.set_volume(user, 0.5);
        mixer.insert(user, 0, &[], 0).unwrap();
        assert_eq!(mixer.streams[&user].volume, 0.5);
        mixer.clear();
        mixer.insert(user, 0, &[], 0).unwrap();
        assert_eq!(mixer.streams[&user].volume, 0.5);
    }
}
