use std::collections::BTreeMap;

use ropus::{Channels, DecodeMode, Decoder};

/// Samples per channel in one Opus frame: 20 ms at 48 kHz.
pub const FRAME: usize = 960;
/// Samples per channel in one pull: 10 ms at 48 kHz.
pub const PULL: usize = 480;
const MIN_TARGET: u32 = FRAME as u32;
const MAX_TARGET: u32 = 10 * FRAME as u32;
/// Concealed frames played before an empty buffer goes quiet and rebuffers.
const MAX_CONCEALED: u32 = 3;
const MAX_PACKETS: usize = 64;
/// Pulls between two accelerations, so playout catches up without audible jumps.
const ACCELERATE_EVERY: u32 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub normal: u64,
    pub fec: u64,
    pub concealed: u64,
    pub late: u64,
    pub overflow: u64,
    pub accelerated: u64,
    pub rebuffered: u64,
}

/// What filled the last pull.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Played {
    Silence,
    Normal,
    Fec,
    Concealed,
}

/// A jitter buffer for one remote Opus track. It decodes 20 ms packets in timestamp
/// order, recovers a single lost packet from the next one's in-band FEC, conceals longer
/// gaps, and adapts its delay to the measured jitter (RFC 3550 estimator).
/// Arrival times are passed in, so tests run on a simulated clock.
pub struct JitterBuffer {
    decoder: Decoder,
    packets: BTreeMap<u32, Vec<u8>>,
    /// Media timestamp of the next frame to decode.
    next_ts: Option<u32>,
    playing: bool,
    channels: usize,
    /// One decoded frame, channels interleaved.
    frame: Vec<f32>,
    frame_ts: u32,
    frame_pos: usize,
    concealed_in_a_row: u32,
    jitter: f64,
    last_transit: Option<i64>,
    pulls_since_accelerate: u32,
    played: Played,
    pub stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(channels: Channels) -> Result<Self, ropus::DecoderInitError> {
        let count = match channels {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        };
        Ok(Self {
            decoder: Decoder::new(48_000, channels)?,
            packets: BTreeMap::new(),
            next_ts: None,
            playing: false,
            channels: count,
            frame: vec![0.0; FRAME * count],
            frame_ts: 0,
            frame_pos: FRAME,
            concealed_in_a_row: 0,
            jitter: 0.0,
            last_transit: None,
            pulls_since_accelerate: 0,
            played: Played::Silence,
            stats: JitterStats::default(),
        })
    }

    /// Adds a packet with its media timestamp, arriving at `arrival` on the local 48 kHz clock.
    pub fn insert(&mut self, timestamp: u32, payload: &[u8], arrival: u64) {
        let transit = arrival as i64 - i64::from(timestamp);
        if let Some(last) = self.last_transit {
            let d = (transit - last).unsigned_abs() as f64;
            if d < f64::from(MAX_TARGET) * 4.0 {
                self.jitter += (d - self.jitter) / 16.0;
            }
        }
        self.last_transit = Some(transit);

        if self.playing && self.next_ts.is_some_and(|next| before(timestamp, next)) {
            self.stats.late += 1;
            return;
        }
        if !self.playing
            && self
                .next_ts
                .is_none_or(|next| self.packets.is_empty() || before(timestamp, next))
        {
            self.next_ts = Some(timestamp);
        }
        if self.packets.len() >= MAX_PACKETS {
            self.packets.pop_first();
            self.stats.overflow += 1;
        }
        self.packets.insert(timestamp, payload.to_vec());
    }

    /// The delay the buffer aims for, in samples.
    pub fn target(&self) -> u32 {
        let target = MIN_TARGET as f64 + 3.0 * self.jitter;
        (target as u32).clamp(MIN_TARGET, MAX_TARGET)
    }

    /// Buffered audio ahead of playout, in milliseconds.
    pub fn buffer_ms(&self) -> f32 {
        self.buffered() as f32 / 48.0
    }

    fn buffered(&self) -> u32 {
        let undecoded = match (self.next_ts, self.packets.last_key_value()) {
            (Some(next), Some((last, _))) if !before(*last, next) => {
                last.wrapping_sub(next) + FRAME as u32
            }
            _ => 0,
        };
        let in_frame = if self.playing {
            (FRAME - self.frame_pos) as u32
        } else {
            0
        };
        undecoded + in_frame
    }

    /// The media timestamp of the next sample this buffer plays, while it plays.
    pub fn playout_timestamp(&self) -> Option<u32> {
        (self.playing && self.frame_pos < FRAME)
            .then(|| self.frame_ts.wrapping_add(self.frame_pos as u32))
    }

    pub fn last_played(&self) -> Played {
        self.played
    }

    /// Fills 10 ms of output, channels interleaved: `out` holds `PULL` samples per channel.
    pub fn pull(&mut self, out: &mut [f32]) -> Played {
        let channels = self.channels;
        if !self.playing {
            if self.buffered() >= self.target() && self.next_ts.is_some() {
                self.playing = true;
                self.frame_pos = FRAME;
            } else {
                out.fill(0.0);
                self.played = Played::Silence;
                return Played::Silence;
            }
        }
        let mut filled = 0;
        let mut played = Played::Normal;
        while filled < PULL {
            if self.frame_pos == FRAME {
                match self.decode_next() {
                    Some(kind) => {
                        if kind != Played::Normal {
                            played = kind;
                        }
                    }
                    None => {
                        out[filled * channels..].fill(0.0);
                        self.played = Played::Silence;
                        return Played::Silence;
                    }
                }
            }
            let take = (PULL - filled).min(FRAME - self.frame_pos);
            out[filled * channels..(filled + take) * channels].copy_from_slice(
                &self.frame[self.frame_pos * channels..(self.frame_pos + take) * channels],
            );
            self.frame_pos += take;
            filled += take;
        }
        self.accelerate();
        self.played = played;
        played
    }

    fn decode_next(&mut self) -> Option<Played> {
        let next = self.next_ts?;
        let following = next.wrapping_add(FRAME as u32);
        let kind = if let Some(packet) = self.packets.remove(&next) {
            self.decode(&packet, DecodeMode::Normal);
            self.concealed_in_a_row = 0;
            self.stats.normal += 1;
            Played::Normal
        } else if let Some(packet) = self.packets.get(&following).cloned() {
            self.decode(&packet, DecodeMode::Fec);
            self.concealed_in_a_row = 0;
            self.stats.fec += 1;
            Played::Fec
        } else if self.concealed_in_a_row < MAX_CONCEALED || !self.packets.is_empty() {
            self.decode(&[], DecodeMode::Normal);
            self.concealed_in_a_row += 1;
            self.stats.concealed += 1;
            Played::Concealed
        } else {
            self.playing = false;
            self.next_ts = None;
            self.stats.rebuffered += 1;
            return None;
        };
        self.frame_ts = next;
        self.frame_pos = 0;
        self.next_ts = Some(following);
        self.packets.retain(|ts, _| !before(*ts, following));
        Some(kind)
    }

    fn decode(&mut self, packet: &[u8], mode: DecodeMode) {
        if self
            .decoder
            .decode_float(packet, &mut self.frame, mode)
            .is_err()
        {
            self.frame.fill(0.0);
        }
    }

    /// Skips one frame when the buffer runs well over its target, at most once per 200 ms.
    fn accelerate(&mut self) {
        self.pulls_since_accelerate += 1;
        if self.pulls_since_accelerate < ACCELERATE_EVERY
            || self.buffered() <= self.target() + 2 * FRAME as u32
        {
            return;
        }
        if let Some(next) = self.next_ts
            && self.packets.remove(&next).is_some()
        {
            self.next_ts = Some(next.wrapping_add(FRAME as u32));
            self.stats.accelerated += 1;
            self.pulls_since_accelerate = 0;
        }
    }
}

/// Serial-number order for 32-bit media timestamps.
fn before(a: u32, b: u32) -> bool {
    throcc_proto::frame::timestamp_before(a, b)
}

#[cfg(test)]
mod tests {
    use ropus::{Application, Bitrate, Encoder, FrameDuration, InbandFec};

    use super::*;

    fn encoder() -> Encoder {
        Encoder::builder(48_000, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(32_000))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(20)
            .build()
            .unwrap()
    }

    fn tone(frame: usize) -> Vec<f32> {
        (0..FRAME)
            .map(|i| {
                let t = (frame * FRAME + i) as f32 / 48_000.0;
                0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
            })
            .collect()
    }

    fn packets(count: usize) -> Vec<Vec<u8>> {
        let mut enc = encoder();
        (0..count)
            .map(|f| {
                let mut out = vec![0u8; 1500];
                let n = enc.encode_float(&tone(f), &mut out).unwrap();
                out.truncate(n);
                out
            })
            .collect()
    }

    fn drain(jb: &mut JitterBuffer, pulls: usize) -> Vec<Played> {
        let mut out = [0.0; PULL];
        (0..pulls).map(|_| jb.pull(&mut out)).collect()
    }

    #[test]
    fn steady_packets_play_in_order_after_the_target_delay() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(10).iter().enumerate() {
            jb.insert(1000 + (i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        let played = drain(&mut jb, 20);
        assert!(played.iter().all(|p| *p == Played::Normal), "{played:?}");
        assert_eq!(jb.stats.normal, 10);
    }

    #[test]
    fn a_single_lost_packet_is_recovered_from_fec() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(10).iter().enumerate().filter(|(i, _)| *i != 4) {
            jb.insert((i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 18);
        assert_eq!((jb.stats.fec, jb.stats.concealed), (1, 0));
    }

    #[test]
    fn two_lost_packets_are_concealed_then_recovered() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(10)
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4 && *i != 5)
        {
            jb.insert((i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 18);
        assert_eq!((jb.stats.concealed, jb.stats.fec), (1, 1));
    }

    #[test]
    fn playout_timestamp_advances_with_playback() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(6).iter().enumerate() {
            jb.insert(5000 + (i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        let mut out = [0.0; PULL];
        jb.pull(&mut out);
        assert_eq!(jb.playout_timestamp(), Some(5000 + PULL as u32));
        jb.pull(&mut out);
        jb.pull(&mut out);
        assert_eq!(jb.playout_timestamp(), Some(5000 + 3 * PULL as u32));
    }

    #[test]
    fn stereo_keeps_its_channels_apart() {
        let mut enc = Encoder::builder(48_000, Channels::Stereo, Application::Audio)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(128_000))
            .build()
            .unwrap();
        let mut jb = JitterBuffer::new(Channels::Stereo).unwrap();
        for f in 0..6 {
            let left_only: Vec<f32> = tone(f).iter().flat_map(|s| [*s, 0.0]).collect();
            let mut packet = vec![0u8; 4000];
            let n = enc.encode_float(&left_only, &mut packet).unwrap();
            jb.insert((f * FRAME) as u32, &packet[..n], (f * FRAME) as u64);
        }
        let mut out = [0.0f32; PULL * 2];
        let mut energy = [0.0f32; 2];
        for _ in 0..7 {
            jb.pull(&mut out);
            for [left, right] in out.as_chunks::<2>().0 {
                energy[0] += left * left;
                energy[1] += right * right;
            }
        }
        assert!(energy[0] > 100.0 * energy[1].max(1e-6), "{energy:?}");
        assert_eq!(jb.playout_timestamp(), Some(7 * PULL as u32));
    }

    #[test]
    fn late_packets_are_dropped() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let all = packets(8);
        for (i, p) in all.iter().enumerate().take(6).filter(|(i, _)| *i != 1) {
            jb.insert((i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 6);
        jb.insert(FRAME as u32, &all[1], 7 * FRAME as u64);
        assert_eq!(jb.stats.late, 1);
    }

    /// Simulates 20 ms packets with jitter, then `gap` of silence from the sender, then
    /// resumes. Returns the buffer level 1 s after resuming.
    fn buffer_after_gap(gap_frames: usize) -> f32 {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let mut enc = encoder();
        let mut out = [0.0; PULL];
        let mut clock: u64 = 0;
        let jitter = |f: usize| ((f * 7919) % 5) as u64 * 480;
        let mut send = |jb: &mut JitterBuffer, frame: usize, clock: u64| {
            let mut packet = vec![0u8; 1500];
            let n = enc.encode_float(&tone(frame), &mut packet).unwrap();
            jb.insert((frame * FRAME) as u32, &packet[..n], clock + jitter(frame));
        };
        for frame in 0..500 {
            send(&mut jb, frame, clock);
            jb.pull(&mut out);
            jb.pull(&mut out);
            clock += FRAME as u64;
        }
        for _ in 0..gap_frames * 2 {
            jb.pull(&mut out);
            clock += PULL as u64;
        }
        let resume = 500 + gap_frames;
        for frame in resume..resume + 50 {
            send(&mut jb, frame, clock);
            jb.pull(&mut out);
            jb.pull(&mut out);
            clock += FRAME as u64;
        }
        jb.buffer_ms()
    }

    #[test]
    fn buffer_recovers_within_a_second_after_a_five_minute_mute() {
        let five_minutes = 5 * 60 * 50;
        let level = buffer_after_gap(five_minutes);
        println!("buffer one second after a five-minute mute: {level} ms");
        assert!(
            level < 100.0,
            "buffer at {level} ms one second after resuming"
        );
    }
}
