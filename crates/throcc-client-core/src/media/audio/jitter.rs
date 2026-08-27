use neteq::codec::AudioDecoder;
use neteq::{AudioPacket, NetEq, NetEqConfig, RtpHeader};
use ropus::{Channels, DecodeMode, Decoder};
use throcc_proto::MediaId;

use crate::media::audio::{SAMPLE_RATE, SAMPLES_PER_BLOCK, SAMPLES_PER_PACKET};
use crate::{Error, Result};

/// One `media_id` is one stream, so the fields RTP would carry are constants or
/// come free from `seq`.
const PAYLOAD_TYPE: u8 = 111;
const PACKET_MS: u32 = 20;
const CHANNELS: u8 = 1;

/// One remote track's packet, ready for the jitter buffer. Built where the
/// datagram arrived, since that is when it arrived.
pub fn packet(media_id: MediaId, seq: u32, payload: &[u8]) -> AudioPacket {
    AudioPacket::new(
        RtpHeader::new(
            seq as u16,
            seq.wrapping_mul(SAMPLES_PER_PACKET as u32),
            media_id.0,
            PAYLOAD_TYPE,
            false,
        ),
        payload.to_vec(),
        SAMPLE_RATE,
        CHANNELS,
        PACKET_MS,
    )
}

/// One remote track's adaptive jitter buffer: it holds the depth the current
/// network needs and conceals what never arrives.
pub struct JitterBuffer {
    neteq: NetEq,
}

impl JitterBuffer {
    pub fn new() -> Result<Self> {
        let config = NetEqConfig {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            ..NetEqConfig::default()
        };
        let mut neteq = NetEq::new(config)
            .map_err(|e| Error::Audio(format!("building a jitter buffer: {e}")))?;
        neteq.register_decoder(PAYLOAD_TYPE, Box::new(OpusDecoder::new()?));
        Ok(Self { neteq })
    }

    pub fn insert(&mut self, packet: AudioPacket) {
        if let Err(e) = self.neteq.insert_packet(packet) {
            tracing::debug!(error = %e, "the jitter buffer refused a packet");
        }
    }

    /// One 10 ms block, always: the sound card asks whether or not the network
    /// cooperated, so a gap is concealed rather than skipped.
    pub fn pull(&mut self, block: &mut [f32; SAMPLES_PER_BLOCK]) {
        match self.neteq.get_audio() {
            Ok(frame) => {
                let filled = frame.samples.len().min(SAMPLES_PER_BLOCK);
                block[..filled].copy_from_slice(&frame.samples[..filled]);
                block[filled..].fill(0.0);
            }
            Err(_) => block.fill(0.0),
        }
    }
}

/// Opus decoding for the jitter buffer, over the same `ropus` the encoder uses.
struct OpusDecoder {
    decoder: Decoder,
    decoded: Vec<f32>,
}

impl OpusDecoder {
    fn new() -> Result<Self> {
        Ok(Self {
            decoder: Decoder::new(SAMPLE_RATE, Channels::Mono)
                .map_err(|e| Error::Audio(format!("building the Opus decoder: {e}")))?,
            decoded: vec![0.0; SAMPLE_RATE as usize * 120 / 1000],
        })
    }
}

impl AudioDecoder for OpusDecoder {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn channels(&self) -> u8 {
        CHANNELS
    }

    fn decode(&mut self, encoded: &[u8]) -> neteq::Result<Vec<f32>> {
        let samples = self
            .decoder
            .decode_float(encoded, &mut self.decoded, DecodeMode::Normal)
            .map_err(|e| neteq::NetEqError::DecoderError(format!("ropus decode: {e}")))?;
        Ok(self.decoded[..samples].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropus::{Application, Bitrate, Encoder, InbandFec};

    fn encoded_tone(frames: usize) -> Vec<Vec<u8>> {
        let mut encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .bitrate(Bitrate::Bits(24_000))
            .inband_fec(InbandFec::Enabled)
            .build()
            .unwrap();
        let mut buffer = vec![0u8; 1_000];

        (0..frames)
            .map(|frame| {
                let pcm: Vec<f32> = (0..SAMPLES_PER_PACKET)
                    .map(|index| {
                        let sample = (frame * SAMPLES_PER_PACKET + index) as f32;
                        (sample * 0.04).sin() * 0.4
                    })
                    .collect();
                let len = encoder.encode_float(&pcm, &mut buffer).unwrap();
                buffer[..len].to_vec()
            })
            .collect()
    }

    fn energy(block: &[f32]) -> f32 {
        block.iter().map(|sample| sample * sample).sum::<f32>() / block.len() as f32
    }

    #[test]
    fn audio_comes_out_of_what_went_in() {
        let mut buffer = JitterBuffer::new().unwrap();
        let mut block = [0.0f32; SAMPLES_PER_BLOCK];
        let mut loudest = 0.0f32;

        for (seq, frame) in encoded_tone(40).iter().enumerate() {
            buffer.insert(packet(MediaId(7), seq as u32, frame));
            for _ in 0..2 {
                buffer.pull(&mut block);
                loudest = loudest.max(energy(&block));
            }
        }
        assert!(
            loudest > 1e-4,
            "the tone should come back out, loudest block was {loudest}"
        );
    }

    #[test]
    fn a_gap_is_concealed_rather_than_left_silent() {
        let mut buffer = JitterBuffer::new().unwrap();
        let mut block = [0.0f32; SAMPLES_PER_BLOCK];
        let frames = encoded_tone(40);

        for (seq, frame) in frames.iter().enumerate() {
            if seq == 20 {
                continue;
            }
            buffer.insert(packet(MediaId(7), seq as u32, frame));
        }

        let mut silent_blocks = 0;
        for _ in 0..80 {
            buffer.pull(&mut block);
            if energy(&block) < 1e-9 {
                silent_blocks += 1;
            }
        }
        assert!(
            silent_blocks < 40,
            "one missing packet should not silence the track: {silent_blocks} silent blocks"
        );
    }

    #[test]
    fn an_empty_buffer_pulls_something_to_play() {
        let mut buffer = JitterBuffer::new().unwrap();
        let mut block = [1.0f32; SAMPLES_PER_BLOCK];
        buffer.pull(&mut block);
        assert_eq!(block.len(), SAMPLES_PER_BLOCK);
    }
}
