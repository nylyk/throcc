use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ropus::{Application, Bitrate, Channels, Encoder, FrameDuration, InbandFec};
use throcc_proto::frame::{VIDEO_CLOCK_HZ, timestamp_before};

use super::audio::SAMPLE_RATE;
use super::jitter::FRAME;

const BITRATE: u32 = 128_000;
const EXPECTED_LOSS_PERCENT: u8 = 5;
/// Bytes one encoded 20 ms stereo frame can take at most.
const MAX_PACKET: usize = 4000;

/// Samples in one 20 ms stereo frame, left and right interleaved.
pub const STEREO_FRAME: usize = FRAME * 2;

/// Encodes share audio: Opus music mode, 48 kHz stereo, 128 kbps, 20 ms, in-band FEC.
pub struct ShareAudioEncoder {
    encoder: Encoder,
    packet: Vec<u8>,
}

impl ShareAudioEncoder {
    pub fn new() -> Result<Self, ropus::EncoderBuildError> {
        let encoder = Encoder::builder(SAMPLE_RATE, Channels::Stereo, Application::Audio)
            .frame_duration(FrameDuration::Ms20)
            .bitrate(Bitrate::Bits(BITRATE))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(EXPECTED_LOSS_PERCENT)
            .build()?;
        Ok(Self {
            encoder,
            packet: vec![0; MAX_PACKET],
        })
    }

    /// Encodes one 20 ms frame of interleaved stereo samples.
    pub fn encode(&mut self, frame: &[f32; STEREO_FRAME]) -> Option<&[u8]> {
        let len = self.encoder.encode_float(frame, &mut self.packet).ok()?;
        self.packet.get(..len)
    }
}

/// Where a share's audio is being heard, as its media timestamp, shared by the audio
/// worker that plays it and the decoder that holds video back to it.
pub struct Playout(AtomicU64);

impl Playout {
    const SILENT: u64 = u64::MAX;

    pub fn new() -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(Self::SILENT)))
    }

    pub fn set(&self, heard: Option<u32>) {
        let value = heard.map_or(Self::SILENT, u64::from);
        self.0.store(value, Ordering::Relaxed);
    }

    /// The share audio's timestamp now reaching the speakers, while it plays.
    pub fn heard(&self) -> Option<u32> {
        u32::try_from(self.0.load(Ordering::Relaxed)).ok()
    }
}

/// Video further ahead of the share audio than this is shown anyway: the audio has
/// stalled or the clocks disagree, and a frozen picture is worse than unsynced sound.
const MAX_HOLD: u32 = VIDEO_CLOCK_HZ / 2;

/// How many of the held frames, oldest first, are due: those at or behind the audio being
/// heard. Without share audio playing, every frame is due.
pub fn due_frames(held: impl IntoIterator<Item = u32>, heard: Option<u32>) -> usize {
    let Some(heard) = heard.map(audio_to_video) else {
        return held.into_iter().count();
    };
    held.into_iter()
        .take_while(|&frame| {
            let ahead = frame.wrapping_sub(heard);
            !timestamp_before(heard, frame) || ahead > MAX_HOLD
        })
        .count()
}

/// An audio timestamp (48 kHz) on the 90 kHz video clock with the same zero.
/// Both wrap together for the first 24 hours of a share.
pub fn audio_to_video(timestamp: u32) -> u32 {
    (u64::from(timestamp) * u64::from(VIDEO_CLOCK_HZ) / u64::from(SAMPLE_RATE)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playout_reads_back_what_was_set() {
        let playout = Playout::new();
        assert_eq!(playout.heard(), None);
        playout.set(Some(u32::MAX));
        assert_eq!(playout.heard(), Some(u32::MAX));
        playout.set(None);
        assert_eq!(playout.heard(), None);
    }

    #[test]
    fn frames_wait_for_the_audio_they_belong_to() {
        let held = [90_000, 91_500, 93_000];
        assert_eq!(due_frames(held, None), 3);
        assert_eq!(due_frames(held, Some(47_000)), 0);
        assert_eq!(due_frames(held, Some(48_000)), 1);
        assert_eq!(due_frames(held, Some(48_800)), 2);
        assert_eq!(due_frames(held, Some(60_000)), 3);
    }

    #[test]
    fn frames_far_ahead_of_the_audio_are_not_held() {
        assert_eq!(due_frames([200_000], Some(0)), 1);
        assert_eq!(due_frames([40_000], Some(0)), 0);
    }

    #[test]
    fn one_second_of_audio_is_one_second_of_video() {
        assert_eq!(audio_to_video(48_000), 90_000);
        assert_eq!(audio_to_video(0), 0);
    }

    #[test]
    fn a_frame_encodes_to_a_music_packet() {
        let mut encoder = ShareAudioEncoder::new().unwrap();
        let frame: [f32; STEREO_FRAME] =
            std::array::from_fn(|i| ((i / 2) as f32 * 0.05).sin() * 0.5);
        let packet = encoder.encode(&frame).unwrap();
        assert!(packet.len() > 100, "{} bytes", packet.len());
    }
}
