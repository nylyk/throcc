use std::collections::BTreeMap;

use ropus::{Channels, DecodeMode, Decoder};
use throcc_proto::frame::timestamp_before;

use super::audio::{FRAME, PULL, SAMPLE_RATE};
const MIN_TARGET: u32 = FRAME as u32;
const MAX_TARGET: u32 = 10 * FRAME as u32;
/// Concealed frames played before a buffer with no packet within its target rebuffers.
const MAX_CONCEALED: u32 = 3;
const MAX_PACKETS: usize = 64;
/// Pulls between two accelerations, so playout catches up without audible jumps. Also the
/// window whose lowest buffer level decides one, which spans a whole burst of PipeWire's
/// largest common quantum (8192 samples).
const ACCELERATE_EVERY: u32 = 20;
/// Buffered audio that stays above this through a whole window is excess delay.
const SPARE: u32 = 2 * FRAME as u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub normal: u64,
    pub fec: u64,
    pub concealed: u64,
    pub late: u64,
    pub overflow: u64,
    pub accelerated: u64,
    pub rebuffered: u64,
    /// Concealed frames played while the next packet was late, which add to the delay.
    pub stretched: u64,
    /// Times playout jumped to a packet off its 20 ms grid.
    pub realigned: u64,
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
    /// The least audio buffered after a pull since the last acceleration window began.
    lowest: u32,
    pub stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(channels: Channels) -> Result<Self, ropus::DecoderInitError> {
        let count = channels.count();
        Ok(Self {
            decoder: Decoder::new(SAMPLE_RATE, channels)?,
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
            lowest: u32::MAX,
            stats: JitterStats::default(),
        })
    }

    /// Adds a packet with its media timestamp, arriving at `arrival` on the local clock
    /// counting samples played.
    pub fn insert(&mut self, timestamp: u32, payload: &[u8], arrival: u64) {
        let transit = arrival as i64 - i64::from(timestamp);
        if let Some(last) = self.last_transit {
            let d = (transit - last).unsigned_abs() as f64;
            if d < f64::from(MAX_TARGET) * 4.0 {
                self.jitter += (d - self.jitter) / 16.0;
            }
        }
        self.last_transit = Some(transit);

        if self.playing
            && self
                .next_ts
                .is_some_and(|next| timestamp_before(timestamp, next))
        {
            self.stats.late += 1;
            return;
        }
        if !self.playing
            && self
                .next_ts
                .is_none_or(|next| self.packets.is_empty() || timestamp_before(timestamp, next))
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
    #[cfg(test)]
    fn buffer_ms(&self) -> f32 {
        self.buffered() as f32 * 1000.0 / SAMPLE_RATE as f32
    }

    fn buffered(&self) -> u32 {
        let undecoded = match (self.next_ts, self.packets.last_key_value()) {
            (Some(next), Some((last, _))) if !timestamp_before(*last, next) => {
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
        self.playing
            .then(|| self.frame_ts.wrapping_add(self.frame_pos as u32))
    }

    /// Fills 10 ms of output, channels interleaved: `out` holds `PULL` samples per channel.
    pub fn pull(&mut self, out: &mut [f32]) {
        let channels = self.channels;
        if !self.playing {
            if self.buffered() >= self.target() && self.next_ts.is_some() {
                self.playing = true;
                self.frame_pos = FRAME;
            } else {
                out.fill(0.0);
                return;
            }
        }
        let mut filled = 0;
        while filled < PULL {
            if self.frame_pos == FRAME && !self.decode_next() {
                out[filled * channels..].fill(0.0);
                return;
            }
            let take = (PULL - filled).min(FRAME - self.frame_pos);
            out[filled * channels..(filled + take) * channels].copy_from_slice(
                &self.frame[self.frame_pos * channels..(self.frame_pos + take) * channels],
            );
            self.frame_pos += take;
            filled += take;
        }
        self.accelerate();
    }

    /// Decodes, recovers or conceals the next frame. False when playout stopped to rebuffer.
    fn decode_next(&mut self) -> bool {
        let Some(mut next) = self.next_ts else {
            return false;
        };
        if !self.packets.contains_key(&next)
            && let Some(&first) = self.packets.keys().next()
            && first.wrapping_sub(next) % FRAME as u32 != 0
        {
            next = first;
            self.stats.realigned += 1;
        }
        let following = next.wrapping_add(FRAME as u32);
        if let Some(packet) = self.packets.remove(&next) {
            decode(
                &mut self.decoder,
                &mut self.frame,
                &packet,
                DecodeMode::Normal,
            );
            self.concealed_in_a_row = 0;
            self.stats.normal += 1;
        } else if let Some(packet) = self.packets.get(&following) {
            decode(&mut self.decoder, &mut self.frame, packet, DecodeMode::Fec);
            self.concealed_in_a_row = 0;
            self.stats.fec += 1;
        } else if self.packets.is_empty() && self.concealed_in_a_row < MAX_CONCEALED {
            decode(&mut self.decoder, &mut self.frame, &[], DecodeMode::Normal);
            self.concealed_in_a_row += 1;
            self.stats.stretched += 1;
            self.frame_ts = next.wrapping_sub(FRAME as u32);
            self.frame_pos = 0;
            return true;
        } else if self.concealed_in_a_row < MAX_CONCEALED || self.first_packet_within_target(next) {
            decode(&mut self.decoder, &mut self.frame, &[], DecodeMode::Normal);
            self.concealed_in_a_row += 1;
            self.stats.concealed += 1;
        } else {
            self.playing = false;
            self.next_ts = self.packets.keys().next().copied();
            self.stats.rebuffered += 1;
            return false;
        }
        self.frame_ts = next;
        self.frame_pos = 0;
        self.next_ts = Some(following);
        self.packets
            .retain(|ts, _| !timestamp_before(*ts, following));
        true
    }

    fn first_packet_within_target(&self, next: u32) -> bool {
        self.packets
            .keys()
            .next()
            .is_some_and(|first| first.wrapping_sub(next) <= self.target())
    }

    /// Skips one frame when the buffer stayed over [`SPARE`] for a whole 200 ms window.
    fn accelerate(&mut self) {
        self.pulls_since_accelerate += 1;
        self.lowest = self.lowest.min(self.buffered());
        if self.pulls_since_accelerate < ACCELERATE_EVERY {
            return;
        }
        let lowest = std::mem::replace(&mut self.lowest, u32::MAX);
        self.pulls_since_accelerate = 0;
        if lowest <= SPARE {
            return;
        }
        if let Some(next) = self.next_ts
            && self.packets.remove(&next).is_some()
        {
            self.next_ts = Some(next.wrapping_add(FRAME as u32));
            self.stats.accelerated += 1;
        }
    }
}

/// Decodes one packet into `frame`, or silence if it fails.
fn decode(decoder: &mut Decoder, frame: &mut [f32], packet: &[u8], mode: DecodeMode) {
    if decoder.decode_float(packet, frame, mode).is_err() {
        frame.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use ropus::{Application, Bitrate, Encoder, FrameDuration, InbandFec};

    use super::*;

    fn encoder() -> Encoder {
        Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(32_000))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(20)
            .build()
            .unwrap()
    }

    fn ms(samples: u64) -> f64 {
        samples as f64 * 1000.0 / f64::from(SAMPLE_RATE)
    }

    fn tone(frame: usize) -> Vec<f32> {
        (0..FRAME)
            .map(|i| {
                let t = (frame * FRAME + i) as f32 / SAMPLE_RATE as f32;
                0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
            })
            .collect()
    }

    /// 30 s of the tone, encoded once: Opus encoding dominates these tests' time, and
    /// only the timestamps the packets are inserted at differ between them.
    static TONE: LazyLock<Vec<Vec<u8>>> = LazyLock::new(|| {
        let mut enc = encoder();
        (0..30 * 50)
            .map(|f| {
                let mut out = vec![0u8; 1500];
                let n = enc.encode_float(&tone(f), &mut out).unwrap();
                out.truncate(n);
                out
            })
            .collect()
    });

    fn packets(count: usize) -> &'static [Vec<u8>] {
        &TONE[..count]
    }

    /// The tone's packet for `frame`, repeating after 30 s.
    fn packet(frame: usize) -> &'static [u8] {
        &TONE[frame % TONE.len()]
    }

    fn drain(jb: &mut JitterBuffer, pulls: usize) {
        let mut out = [0.0; PULL];
        for _ in 0..pulls {
            jb.pull(&mut out);
        }
    }

    #[test]
    fn steady_packets_play_in_order_after_the_target_delay() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(10).iter().enumerate() {
            jb.insert(1000 + (i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 20);
        let JitterStats {
            normal,
            fec,
            concealed,
            stretched,
            rebuffered,
            ..
        } = jb.stats;
        assert_eq!(
            (normal, fec, concealed, stretched, rebuffered),
            (10, 0, 0, 0, 0)
        );
    }

    #[test]
    fn lost_packets_are_recovered_from_fec_or_concealed() {
        for (lost, fec_and_concealed) in [(&[4][..], (1, 0)), (&[4, 5][..], (1, 1))] {
            let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
            for (i, p) in packets(10).iter().enumerate() {
                if !lost.contains(&i) {
                    jb.insert((i * FRAME) as u32, p, (i * FRAME) as u64);
                }
            }
            drain(&mut jb, 18);
            assert_eq!(
                (jb.stats.fec, jb.stats.concealed),
                fec_and_concealed,
                "lost {lost:?}"
            );
        }
    }

    #[test]
    fn playout_timestamp_is_known_after_every_pull() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        for (i, p) in packets(6).iter().enumerate() {
            jb.insert(5000 + (i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        let mut out = [0.0; PULL];
        for pulls in 1..=8u32 {
            jb.pull(&mut out);
            assert_eq!(
                jb.playout_timestamp(),
                Some(5000 + pulls * PULL as u32),
                "after {pulls} pulls"
            );
        }
    }

    /// Talks for 2 s, leaves the buffer unpulled for 3 s as a deafened listener does,
    /// then talks for 10 s, which must play.
    #[test]
    fn talk_resumes_after_a_frozen_gap() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let mut out = [0.0; PULL];
        let mut talk = |jb: &mut JitterBuffer, frames: std::ops::Range<usize>| {
            for frame in frames {
                jb.insert(
                    (frame * FRAME) as u32,
                    packet(frame),
                    (frame * FRAME) as u64,
                );
                jb.pull(&mut out);
                jb.pull(&mut out);
            }
        };
        talk(&mut jb, 0..100);
        let before = jb.stats.normal;
        talk(&mut jb, 250..750);
        let resumed = jb.stats.normal - before;
        assert!(
            resumed >= 495,
            "{resumed} of 500 frames played: {:?}",
            jb.stats
        );
    }

    #[test]
    fn stereo_keeps_its_channels_apart() {
        let mut enc = Encoder::builder(SAMPLE_RATE, Channels::Stereo, Application::Audio)
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
    fn packets_off_the_grid_play_after_one_realignment() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let all = packets(12);
        for (i, p) in all.iter().enumerate() {
            let shift = if i < 4 { 0 } else { 333 };
            jb.insert((i * FRAME + shift) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 24);
        assert_eq!(jb.stats.realigned, 1);
        assert_eq!(jb.stats.normal, 12, "{:?}", jb.stats);
        assert_eq!(jb.stats.concealed, 0);
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
        let mut out = [0.0; PULL];
        let mut clock: u64 = 0;
        let jitter = |f: usize| ((f * 7919) % 5) as u64 * 480;
        let send = |jb: &mut JitterBuffer, frame: usize, clock: u64| {
            jb.insert((frame * FRAME) as u32, packet(frame), clock + jitter(frame));
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

    /// Plays 30 s of audio captured in bursts of `quantum` samples, as PipeWire delivers
    /// desktop audio, from a watcher joining at `phase`. Returns the stats after the first
    /// 2 s and the most delay in the last 10 s, in ms.
    fn bursts(quantum: u64, phase: u64) -> (JitterStats, f64) {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let mut out = [0.0; PULL];
        let frames = TONE.len();
        let sent = |frame: usize| ((frame as u64 + 1) * FRAME as u64).div_ceil(quantum) * quantum;
        let (mut next, mut clock, mut delay) = (0, 0u64, 0.0f64);
        let mut warm = JitterStats::default();
        while next < frames {
            while next < frames && sent(next) + phase <= clock {
                jb.insert((next * FRAME) as u32, &TONE[next], clock);
                next += 1;
            }
            jb.pull(&mut out);
            clock += PULL as u64;
            if clock == 2 * u64::from(SAMPLE_RATE) {
                warm = jb.stats;
            }
            if let Some(timestamp) = jb
                .playout_timestamp()
                .filter(|_| clock > 20 * u64::from(SAMPLE_RATE))
            {
                let behind = clock - phase - u64::from(timestamp);
                delay = delay.max(ms(behind));
            }
        }
        let stats = jb.stats;
        let after_warm = JitterStats {
            concealed: stats.concealed - warm.concealed,
            stretched: stats.stretched - warm.stretched,
            late: stats.late - warm.late,
            accelerated: stats.accelerated - warm.accelerated,
            ..stats
        };
        (after_warm, delay)
    }

    #[test]
    fn audio_in_pipewire_bursts_plays_without_gaps_at_the_least_delay() {
        for quantum in [1024, 2048, 4096, 8192] {
            for phase in [0, 300, 1500] {
                let (stats, delay) = bursts(quantum, phase);
                let gaps = stats.concealed + stats.stretched + stats.late + stats.accelerated;
                assert_eq!(gaps, 0, "quantum {quantum}, phase {phase}: {stats:?}");
                let burst_ms = ms(quantum);
                assert!(
                    delay < burst_ms + 40.0,
                    "quantum {quantum}, phase {phase}: {delay:.0} ms behind"
                );
            }
        }
    }

    #[test]
    fn a_late_packet_plays_after_a_stretch_instead_of_being_dropped() {
        let mut jb = JitterBuffer::new(Channels::Mono).unwrap();
        let all = packets(8);
        let mut out = [0.0; PULL];
        for (i, p) in all.iter().enumerate().take(2) {
            jb.insert((i * FRAME) as u32, p, (i * FRAME) as u64);
        }
        drain(&mut jb, 5);
        for (i, p) in all.iter().enumerate().skip(2) {
            jb.insert((i * FRAME) as u32, p, 5 * PULL as u64);
        }
        for _ in 0..12 {
            jb.pull(&mut out);
        }
        assert_eq!(jb.stats.late, 0, "{:?}", jb.stats);
        assert_eq!(jb.stats.normal, 8, "{:?}", jb.stats);
        assert_eq!(jb.stats.stretched, 1, "{:?}", jb.stats);
    }
}
