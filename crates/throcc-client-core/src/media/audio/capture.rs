use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bytes::Bytes;
use cpal::traits::{DeviceTrait as _, StreamTrait as _};
use ropus::{Application, Bitrate, Channels, Encoder, InbandFec};
use sonora_common_audio::push_resampler::PushResampler;
use tokio::sync::mpsc;

use crate::media::audio::{SAMPLE_RATE, SAMPLES_PER_BLOCK, SAMPLES_PER_PACKET, devices};
use crate::{Error, Result};

const BITRATE: u32 = 24_000;
/// Opus adds redundancy sized against the loss it is told to expect.
const EXPECTED_LOSS_PERCENT: u8 = 10;
/// A 20 ms frame at 24 kbps is around 60 bytes, so this is room to spare.
const MAX_PACKET_BYTES: usize = 1_000;
pub const QUEUE_DEPTH: usize = 64;

/// Dropping this stops the device.
pub struct Capture {
    _stream: cpal::Stream,
    muted: Arc<AtomicBool>,
    dropped_frames: Arc<AtomicU64>,
}

impl Capture {
    /// Mute stops sending rather than sending silence, and takes effect here
    /// rather than after a round trip.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Frames the encoder produced that the send queue had no room for.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }
}

/// Opens the microphone and encodes 20 ms Opus frames into `frames`. The work
/// runs on the device's realtime thread, so it must never block or await.
pub fn start(device_id: Option<&str>, frames: mpsc::Sender<Bytes>) -> Result<Capture> {
    let device = devices::input(device_id)?;
    let config = negotiate(&device)?;
    let mut pipeline = Pipeline::new(config.sample_rate, config.channels)?;

    let muted = Arc::new(AtomicBool::new(false));
    let dropped_frames = Arc::new(AtomicU64::new(0));
    let sending = frames;
    let is_muted = muted.clone();
    let drops = dropped_frames.clone();

    let stream = device
        .build_input_stream(
            config,
            move |samples: &[f32], _: &cpal::InputCallbackInfo| {
                if is_muted.load(Ordering::Relaxed) {
                    pipeline.discard();
                    return;
                }
                pipeline.encode(samples, |frame| {
                    if sending.try_send(frame).is_err() {
                        drops.fetch_add(1, Ordering::Relaxed);
                    }
                });
            },
            |e| tracing::warn!(error = %e, "the microphone stream reported an error"),
            None,
        )
        .map_err(|e| Error::Audio(format!("opening the microphone: {e}")))?;
    stream
        .play()
        .map_err(|e| Error::Audio(format!("starting the microphone: {e}")))?;

    tracing::info!(
        sample_rate = config.sample_rate,
        channels = config.channels,
        buffer_size = ?config.buffer_size,
        "capturing"
    );

    Ok(Capture {
        _stream: stream,
        muted,
        dropped_frames,
    })
}

/// 48 kHz mono where the device allows it, and the device's own default rate
/// otherwise, which the pipeline then resamples.
fn negotiate(device: &cpal::Device) -> Result<cpal::StreamConfig> {
    let default = device
        .default_input_config()
        .map_err(|e| Error::Audio(format!("reading the microphone's default config: {e}")))?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        return Err(Error::Audio(format!(
            "this microphone offers {} samples, and only f32 is supported",
            default.sample_format()
        )));
    }

    let mut supported = device
        .supported_input_configs()
        .map_err(|e| Error::Audio(format!("reading the microphone's configs: {e}")))?;
    let preferred = supported
        .find(|range| {
            range.channels() == 1
                && range.sample_format() == cpal::SampleFormat::F32
                && range.contains_rate(SAMPLE_RATE)
        })
        .map(|range| range.with_sample_rate(SAMPLE_RATE));

    Ok(preferred.unwrap_or(default).into())
}

/// Device samples in, Opus frames out: downmix to mono, resample to 48 kHz in
/// 10 ms blocks, and encode every two blocks.
struct Pipeline {
    channels: usize,
    resampler: Option<Resampler>,
    mono: Vec<f32>,
    packet: Vec<f32>,
    encoder: Encoder,
    encoded: Vec<u8>,
}

struct Resampler {
    resampler: PushResampler<f32>,
    samples_per_block: usize,
    block: Vec<f32>,
}

impl Pipeline {
    fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        let resampler = (sample_rate != SAMPLE_RATE).then(|| {
            let samples_per_block = (sample_rate / 100) as usize;
            tracing::info!(
                sample_rate,
                "resampling the microphone to {SAMPLE_RATE} for the codec"
            );
            Resampler {
                resampler: PushResampler::new(samples_per_block, SAMPLES_PER_BLOCK, 1),
                samples_per_block,
                block: vec![0.0; SAMPLES_PER_BLOCK],
            }
        });

        let encoder = Encoder::builder(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .bitrate(Bitrate::Bits(BITRATE))
            .inband_fec(InbandFec::Enabled)
            .packet_loss_perc(EXPECTED_LOSS_PERCENT)
            .build()
            .map_err(|e| Error::Audio(format!("building the Opus encoder: {e}")))?;

        Ok(Self {
            channels: channels.max(1) as usize,
            resampler,
            mono: Vec::with_capacity(SAMPLES_PER_PACKET * 2),
            packet: Vec::with_capacity(SAMPLES_PER_PACKET),
            encoder,
            encoded: vec![0; MAX_PACKET_BYTES],
        })
    }

    /// Muting throws away what is buffered, so unmuting does not send audio from
    /// before it.
    fn discard(&mut self) {
        self.mono.clear();
        self.packet.clear();
    }

    fn encode(&mut self, samples: &[f32], mut send: impl FnMut(Bytes)) {
        for frame in samples.chunks(self.channels) {
            let mixed = frame.iter().sum::<f32>() / self.channels as f32;
            self.mono.push(mixed);
        }

        let block = self
            .resampler
            .as_ref()
            .map_or(SAMPLES_PER_BLOCK, |resampler| resampler.samples_per_block);
        while self.mono.len() >= block {
            match self.resampler.as_mut() {
                None => self.packet.extend_from_slice(&self.mono[..block]),
                Some(resampler) => {
                    resampler
                        .resampler
                        .resample_mono(&self.mono[..block], &mut resampler.block);
                    self.packet.extend_from_slice(&resampler.block);
                }
            }
            self.mono.drain(..block);

            if self.packet.len() >= SAMPLES_PER_PACKET {
                if let Some(frame) = self.encode_packet() {
                    send(frame);
                }
                self.packet.clear();
            }
        }
    }

    fn encode_packet(&mut self) -> Option<Bytes> {
        match self
            .encoder
            .encode_float(&self.packet[..SAMPLES_PER_PACKET], &mut self.encoded)
        {
            Ok(len) => Some(Bytes::copy_from_slice(&self.encoded[..len])),
            Err(e) => {
                tracing::debug!(error = %e, "the encoder refused a frame");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, channels: usize) -> Vec<f32> {
        (0..samples * channels)
            .map(|index| ((index / channels) as f32 * 0.05).sin() * 0.3)
            .collect()
    }

    #[test]
    fn one_opus_frame_comes_out_per_twenty_milliseconds() {
        let mut pipeline = Pipeline::new(SAMPLE_RATE, 1).unwrap();
        let mut frames = Vec::new();
        pipeline.encode(&tone(SAMPLES_PER_PACKET * 5, 1), |frame| frames.push(frame));

        assert_eq!(frames.len(), 5, "five packets of 20 ms");
        assert!(
            frames.iter().all(|frame| !frame.is_empty()),
            "an empty Opus frame is not a frame"
        );
        assert!(
            frames.iter().all(|frame| frame.len() < 300),
            "24 kbps of 20 ms audio does not reach 300 bytes"
        );
    }

    #[test]
    fn a_stereo_device_at_another_rate_still_yields_whole_frames() {
        let mut pipeline = Pipeline::new(44_100, 2).unwrap();
        let mut frames = Vec::new();
        pipeline.encode(&tone(44_100 / 10, 2), |frame| frames.push(frame));

        assert_eq!(
            frames.len(),
            5,
            "100 ms at 44.1 kHz stereo resamples to five 20 ms frames"
        );
    }

    #[test]
    fn a_partial_block_is_kept_until_it_is_whole() {
        let mut pipeline = Pipeline::new(SAMPLE_RATE, 1).unwrap();
        let mut frames = Vec::new();
        for _ in 0..4 {
            pipeline.encode(&tone(SAMPLES_PER_BLOCK / 2, 1), |frame| frames.push(frame));
        }
        assert_eq!(frames.len(), 1, "four half blocks make one 20 ms packet");
    }

    #[test]
    fn muting_drops_what_was_buffered() {
        let mut pipeline = Pipeline::new(SAMPLE_RATE, 1).unwrap();
        let mut frames = Vec::new();
        pipeline.encode(&tone(SAMPLES_PER_BLOCK, 1), |frame| frames.push(frame));
        assert!(frames.is_empty(), "half a packet is not sent");

        pipeline.discard();
        pipeline.encode(&tone(SAMPLES_PER_BLOCK, 1), |frame| frames.push(frame));
        assert!(frames.is_empty(), "the discarded block is not completed");
    }
}
