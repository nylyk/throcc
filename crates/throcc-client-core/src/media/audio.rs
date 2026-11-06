use std::collections::HashMap;
use std::collections::hash_map::Entry;

use ropus::{Application, Bitrate, Channels, Encoder, FrameDuration, InbandFec};
use sonora::config::{
    AdaptiveDigital, EchoCanceller, GainController2, HighPassFilter, NoiseSuppressionLevel,
};
use sonora::{AudioProcessing, Config, StreamConfig};
use sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper;
use throcc_proto::frame::AUDIO_CLOCK_HZ;
use throcc_proto::ids::UserId;

use super::jitter::JitterBuffer;
use crate::settings::{NoiseSuppression, Transmit, VoiceSettings};
use crate::state::{MAX_VOLUME, MIC_KBPS};

/// The rate all audio is processed at, which is also its media clock.
pub const SAMPLE_RATE: u32 = AUDIO_CLOCK_HZ;
/// Samples per channel in one Opus frame: 20 ms.
pub const FRAME: usize = SAMPLE_RATE as usize / 50;
/// Samples per channel in one pull: 10 ms.
pub const PULL: usize = SAMPLE_RATE as usize / 100;
/// Voice activation keeps sending, and the speaking ring stays lit, this long after speech stops.
const HANGOVER_PULLS: u32 = 30;
pub(crate) const MAX_PACKET: usize = 1500;
/// Speech probability above which the speech detector counts 10 ms as speech.
const SPEECH_PROBABILITY: f32 = 0.5;
/// A decoded level above this (dBFS) counts as speaking for the UI's ring.
const SPEAKING_DBFS: f32 = -60.0;

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

/// Mic input to Opus packets: processing, the transmit gate, then 20 ms frames with FEC
/// and DTX. The media clock runs on while gated, so timestamps stay real time.
pub struct CaptureChain {
    apm: AudioProcessing,
    encoder: Encoder,
    transmit: Transmit,
    speech_only: bool,
    speech: VoiceActivityDetectorWrapper,
    input_volume: f32,
    /// Echo cancellation, noise suppression and gain control as last applied.
    processing: (bool, Option<NoiseSuppression>, bool),
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
    level_dbfs: f32,
    /// Whether the transmit gate let the last 10 ms through, before muting.
    gate_open: bool,
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
            .bitrate(Bitrate::Bits(MIC_KBPS * 1000))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(5)
            .dtx(true)
            .build()
            .map_err(|e| AudioError::Encoder(e.to_string()))?;
        Ok(Self {
            apm,
            encoder,
            transmit: settings.transmit,
            speech_only: settings.speech_only,
            speech: VoiceActivityDetectorWrapper::new(
                sonora_simd::detect_backend(),
                SAMPLE_RATE as i32,
            ),
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
            gate_open: false,
        })
    }

    pub fn apply(&mut self, settings: &VoiceSettings) {
        let processing = processing_choice(settings);
        if processing != self.processing {
            self.apm.apply_config(processing_config(settings));
            self.processing = processing;
        }
        self.transmit = settings.transmit;
        self.speech_only = settings.speech_only;
        self.input_volume = settings.input_volume;
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

    /// Level of the last 10 ms after processing, in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.level_dbfs
    }

    /// Whether the transmit gate let the last 10 ms through, before muting.
    pub fn gate_open(&self) -> bool {
        self.gate_open
    }

    /// Reports silence until the next capture, for a mic that stopped delivering.
    pub fn reset_levels(&mut self) {
        self.level_dbfs = f32::NEG_INFINITY;
        self.gate_open = false;
        self.transmitting = false;
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
                let speech = !self.speech_only || self.hears_speech();
                hold(
                    &mut self.hangover,
                    speech && self.level_dbfs > threshold_dbfs,
                )
            }
        };
        self.gate_open = open;
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

    fn hears_speech(&mut self) -> bool {
        let in_16_bit_range = self.processed.map(|sample| sample * 32768.0);
        self.speech.analyze(&in_16_bit_range) > SPEECH_PROBABILITY
    }
}

fn processing_choice(settings: &VoiceSettings) -> (bool, Option<NoiseSuppression>, bool) {
    (
        settings.echo_cancellation,
        settings.noise_suppression,
        settings.automatic_gain,
    )
}

fn sonora_level(choice: NoiseSuppression) -> NoiseSuppressionLevel {
    match choice {
        NoiseSuppression::Low => NoiseSuppressionLevel::Low,
        NoiseSuppression::Moderate => NoiseSuppressionLevel::Moderate,
        NoiseSuppression::High => NoiseSuppressionLevel::High,
    }
}

fn processing_config(settings: &VoiceSettings) -> Config {
    Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: settings.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: settings.noise_suppression.map(|choice| {
            sonora::config::NoiseSuppression {
                level: sonora_level(choice),
                ..Default::default()
            }
        }),
        gain_controller2: settings.automatic_gain.then(|| GainController2 {
            adaptive_digital: Some(AdaptiveDigital::default()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Restarts `hangover` while `active`, else counts it down. True while it runs.
fn hold(hangover: &mut u32, active: bool) -> bool {
    *hangover = if active {
        HANGOVER_PULLS
    } else {
        hangover.saturating_sub(1)
    };
    *hangover > 0
}

/// Per-user gains, 1 until set, kept for streams that start later.
#[derive(Default)]
pub struct Gains(HashMap<UserId, f32>);

impl Gains {
    pub fn set(&mut self, user: UserId, gain: f32) {
        self.0.insert(user, gain.clamp(0.0, MAX_VOLUME));
    }

    pub fn get(&self, user: UserId) -> f32 {
        self.0.get(&user).copied().unwrap_or(1.0)
    }
}

struct Stream {
    jitter: JitterBuffer,
    /// Pulls left before the stream stops counting as speaking.
    speaking_pulls: u32,
}

/// Every remote voice stream summed into one output: per-user volume, soft clip.
/// The result is what the speakers play and the echo canceller's reference.
pub struct Mixer {
    streams: HashMap<UserId, Stream>,
    volumes: Gains,
    pull: [f32; PULL],
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            streams: HashMap::new(),
            volumes: Gains::default(),
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
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(Stream {
                jitter: JitterBuffer::new(Channels::Mono)
                    .map_err(|e| AudioError::Decoder(e.to_string()))?,
                speaking_pulls: 0,
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
        self.volumes.set(user, volume);
    }

    /// Mixes the next 10 ms of every stream into `out`.
    pub fn mix(&mut self, out: &mut [f32; PULL]) {
        out.fill(0.0);
        for (user, stream) in &mut self.streams {
            stream.jitter.pull(&mut self.pull);
            hold(&mut stream.speaking_pulls, dbfs(&self.pull) > SPEAKING_DBFS);
            let volume = self.volumes.get(*user);
            for (o, s) in out.iter_mut().zip(&self.pull) {
                *o += s * volume;
            }
        }
        for o in out.iter_mut() {
            *o = soft_clip(*o);
        }
    }

    pub fn speaking(&self) -> impl Iterator<Item = (UserId, bool)> + '_ {
        self.streams
            .iter()
            .map(|(user, s)| (*user, s.speaking_pulls > 0))
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
mod tests {
    use ropus::{DecodeMode, Decoder};

    use super::*;

    /// Speech-like test signal: a gliding voiced harmonic series, shaped into syllables.
    fn speech(seed: u32, seconds: f32) -> Vec<f32> {
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
        let reference = {
            let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono).unwrap();
            let mut frame = vec![0.0f32; FRAME];
            for packet in packets.iter().take(lost + 1) {
                decoder
                    .decode_float(packet, &mut frame, DecodeMode::Normal)
                    .unwrap();
            }
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

    /// Loud broadband clicks, like typing, 20 ms long every 150 ms.
    fn clicks(seconds: f32) -> Vec<f32> {
        let n = (seconds * SAMPLE_RATE as f32) as usize;
        let mut noise = 0x9e37_79b9u32;
        (0..n)
            .map(|i| {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                let in_click = i % (SAMPLE_RATE as usize * 150 / 1000) < FRAME;
                if in_click {
                    (noise as f32 / u32::MAX as f32 - 0.5) * 0.8
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn packets_sent(settings: &VoiceSettings, input: &[f32]) -> usize {
        let mut chain = CaptureChain::new(settings).unwrap();
        input
            .as_chunks::<PULL>()
            .0
            .iter()
            .filter(|frame| chain.capture(frame, false).unwrap().is_some())
            .count()
    }

    #[test]
    fn speech_only_keeps_loud_clicks_out_and_lets_speech_through() {
        let speech_only = VoiceSettings {
            speech_only: true,
            ..VoiceSettings::default()
        };
        let typing = clicks(3.0);
        let by_level = packets_sent(&VoiceSettings::default(), &typing);
        let by_speech = packets_sent(&speech_only, &typing);
        let talking = packets_sent(&speech_only, &speech(5, 3.0));
        println!("clicks: {by_level} packets by level, {by_speech} speech only; speech: {talking}");
        assert!(by_level > 50, "the clicks never crossed the level gate");
        assert!(by_speech < by_level / 2, "{by_speech} packets of clicks");
        assert!(talking > 75, "only {talking} packets of 150 for speech");
    }

    #[test]
    fn a_muted_chain_sends_nothing_but_still_reports_its_gate() {
        let mut chain = CaptureChain::new(&VoiceSettings::default()).unwrap();
        chain.set_muted(true);
        let (mut sent, mut opened) = (0, 0);
        for frame in speech(5, 2.0).as_chunks::<PULL>().0 {
            sent += usize::from(chain.capture(frame, false).unwrap().is_some());
            opened += usize::from(chain.gate_open);
        }
        assert_eq!(sent, 0);
        assert!(opened > 100, "the gate opened for {opened} of 200 pulls");
        for _ in 0..100 {
            chain.capture(&[0.0; PULL], false).unwrap();
        }
        assert!(
            !chain.gate_open,
            "the gate stayed open through a second of silence"
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

        let mut encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .build()
            .unwrap();
        let packets: Vec<Vec<u8>> = speech(0, 0.4)
            .as_chunks::<FRAME>()
            .0
            .iter()
            .map(|frame| {
                let mut packet = vec![0u8; MAX_PACKET];
                let n = encoder.encode_float(frame, &mut packet).unwrap();
                packet.truncate(n);
                packet
            })
            .collect();
        let mixed = |volumes: &[(UserId, f32)]| {
            let mut mixer = Mixer::default();
            for &(user, volume) in volumes {
                mixer.set_volume(user, volume);
                for (i, packet) in packets.iter().enumerate() {
                    let timestamp = (i * FRAME) as u32;
                    mixer
                        .insert(user, timestamp, packet, u64::from(timestamp))
                        .unwrap();
                }
            }
            let (mut out, mut samples) = ([0.0f32; PULL], Vec::new());
            for _ in 0..(packets.len() + 2) * FRAME / PULL {
                mixer.mix(&mut out);
                samples.extend(out);
            }
            samples
        };
        let same = |got: Vec<f32>, want: Vec<f32>| {
            got.iter().zip(&want).all(|(g, w)| (g - w).abs() < 1e-6)
        };
        let (mara, ilse) = (UserId(1), UserId(2));
        let alone = mixed(&[(mara, 1.0)]);
        let peak = alone.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak > 0.1 && peak < 0.5,
            "peak {peak} outside soft clip's linear range"
        );
        assert!(
            same(mixed(&[(mara, 0.5), (ilse, 0.5)]), alone.clone()),
            "two streams at half volume don't sum to one at full"
        );
        assert!(
            same(
                mixed(&[(mara, 0.25)]),
                alone.iter().map(|s| s * 0.25).collect()
            ),
            "volume isn't applied"
        );
        assert!(
            same(
                mixed(&[(mara, 1.0), (ilse, 1.0)]),
                alone.iter().map(|s| soft_clip(2.0 * s)).collect()
            ),
            "the sum isn't soft clipped"
        );
    }

    #[test]
    fn speaking_holds_through_the_gaps_between_syllables_and_ends_after_speech() {
        let user = UserId(7);
        let mut input = speech(0, 2.0);
        input.extend([0.0; SAMPLE_RATE as usize]);
        let mut encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .build()
            .unwrap();
        let mut mixer = Mixer::default();
        let mut out = [0.0f32; PULL];
        let mut samples = Vec::new();
        for (i, frame) in input.as_chunks::<FRAME>().0.iter().enumerate() {
            let mut packet = vec![0u8; MAX_PACKET];
            let n = encoder.encode_float(frame, &mut packet).unwrap();
            let timestamp = (i * FRAME) as u32;
            mixer
                .insert(user, timestamp, &packet[..n], u64::from(timestamp))
                .unwrap();
            for _ in 0..FRAME / PULL {
                mixer.mix(&mut out);
            }
            if i % 5 == 4 {
                samples.push(mixer.speaking().any(|(_, speaking)| speaking));
            }
        }
        let during = &samples[3..18];
        assert!(
            during.iter().all(|s| *s),
            "the ring went dark mid-speech: {during:?}"
        );
        assert!(!samples.last().unwrap(), "the ring stayed lit after speech");
    }

    #[test]
    fn gains_are_one_until_set_and_stay_in_range() {
        let mut gains = Gains::default();
        assert_eq!(gains.get(UserId(7)), 1.0);
        gains.set(UserId(7), 0.5);
        assert_eq!(gains.get(UserId(7)), 0.5);
        gains.set(UserId(7), 100.0);
        assert_eq!(gains.get(UserId(7)), MAX_VOLUME);
    }
}
