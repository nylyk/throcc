use std::sync::Arc;

use cpal::traits::{DeviceTrait as _, StreamTrait as _};
use neteq::AudioPacket;
use sonora_common_audio::push_resampler::PushResampler;
use throcc_proto::MediaId;
use tokio::sync::mpsc;

use crate::media::audio::cleanup::RenderReference;
use crate::media::audio::jitter::JitterBuffer;
use crate::media::audio::{SAMPLE_RATE, SAMPLES_PER_BLOCK, devices};
use crate::{Error, Result};

pub const PACKET_QUEUE_DEPTH: usize = 32;
pub const TRACK_QUEUE_DEPTH: usize = 16;
const EXPECTED_TRACKS: usize = 16;

/// A remote track and the jitter buffer that will play it out, built off the
/// realtime thread and handed to the output callback.
pub struct TrackFeed {
    pub media_id: MediaId,
    pub packets: mpsc::Receiver<AudioPacket>,
    pub jitter: JitterBuffer,
}

/// Dropping this stops playback.
pub struct Playout {
    _stream: cpal::Stream,
}

pub fn start(
    device_id: Option<&str>,
    feeds: mpsc::Receiver<TrackFeed>,
    reference: Arc<RenderReference>,
) -> Result<Playout> {
    let device = devices::output(device_id)?;
    let config = negotiate(&device)?;
    let mut mixer = Mixer::new(feeds, config.sample_rate, config.channels, reference);

    let stream = device
        .build_output_stream(
            config,
            move |output: &mut [f32], _: &cpal::OutputCallbackInfo| mixer.fill(output),
            |e| tracing::warn!(error = %e, "the playback stream reported an error"),
            None,
        )
        .map_err(|e| Error::Audio(format!("opening playback: {e}")))?;
    stream
        .play()
        .map_err(|e| Error::Audio(format!("starting playback: {e}")))?;

    tracing::info!(
        sample_rate = config.sample_rate,
        channels = config.channels,
        buffer_size = ?config.buffer_size,
        "playing out"
    );

    Ok(Playout { _stream: stream })
}

fn negotiate(device: &cpal::Device) -> Result<cpal::StreamConfig> {
    let default = device
        .default_output_config()
        .map_err(|e| Error::Audio(format!("reading the speaker's default config: {e}")))?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        return Err(Error::Audio(format!(
            "this output offers {} samples, and only f32 is supported",
            default.sample_format()
        )));
    }

    let mut supported = device
        .supported_output_configs()
        .map_err(|e| Error::Audio(format!("reading the speaker's configs: {e}")))?;
    let preferred = supported
        .find(|range| {
            range.sample_format() == cpal::SampleFormat::F32 && range.contains_rate(SAMPLE_RATE)
        })
        .map(|range| range.with_sample_rate(SAMPLE_RATE));

    Ok(preferred.unwrap_or(default).into())
}

struct Track {
    media_id: MediaId,
    packets: mpsc::Receiver<AudioPacket>,
    jitter: JitterBuffer,
}

/// Every remote track, summed into whatever the device asks for. This runs on
/// the realtime thread, so it must never block or await.
struct Mixer {
    feeds: mpsc::Receiver<TrackFeed>,
    reference: Arc<RenderReference>,
    tracks: Vec<Track>,
    block: [f32; SAMPLES_PER_BLOCK],
    mixed: Vec<f32>,
    ready: Vec<f32>,
    resampler: Option<Resampler>,
    channels: usize,
}

struct Resampler {
    resampler: PushResampler<f32>,
    samples_per_block: usize,
}

impl Mixer {
    fn new(
        feeds: mpsc::Receiver<TrackFeed>,
        sample_rate: u32,
        channels: u16,
        reference: Arc<RenderReference>,
    ) -> Self {
        let resampler = (sample_rate != SAMPLE_RATE).then(|| {
            let samples_per_block = (sample_rate / 100) as usize;
            tracing::info!(sample_rate, "resampling playback from {SAMPLE_RATE}");
            Resampler {
                resampler: PushResampler::new(SAMPLES_PER_BLOCK, samples_per_block, 1),
                samples_per_block,
            }
        });

        Self {
            feeds,
            reference,
            tracks: Vec::with_capacity(EXPECTED_TRACKS),
            block: [0.0; SAMPLES_PER_BLOCK],
            mixed: vec![0.0; SAMPLES_PER_BLOCK],
            ready: Vec::with_capacity(SAMPLES_PER_BLOCK * 2),
            resampler,
            channels: channels.max(1) as usize,
        }
    }

    fn fill(&mut self, output: &mut [f32]) {
        self.take_new_tracks();
        self.take_arrived_packets();

        let frames = output.len() / self.channels;
        while self.ready.len() < frames {
            self.mix_one_block();
        }

        for (frame, sample) in output
            .chunks_mut(self.channels)
            .zip(self.ready.drain(..frames))
        {
            frame.fill(sample);
        }
    }

    fn take_new_tracks(&mut self) {
        while let Ok(feed) = self.feeds.try_recv() {
            tracing::debug!(media_id = %feed.media_id, "playing out a new track");
            self.tracks.push(Track {
                media_id: feed.media_id,
                packets: feed.packets,
                jitter: feed.jitter,
            });
        }
    }

    /// A track whose sender has gone is torn down here, so a departed peer leaves
    /// no jitter buffer behind.
    fn take_arrived_packets(&mut self) {
        self.tracks.retain_mut(|track| {
            loop {
                match track.packets.try_recv() {
                    Ok(packet) => track.jitter.insert(packet),
                    Err(mpsc::error::TryRecvError::Empty) => return true,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        tracing::debug!(media_id = %track.media_id, "tearing down a track");
                        return false;
                    }
                }
            }
        });
    }

    fn mix_one_block(&mut self) {
        self.mixed.fill(0.0);
        for track in &mut self.tracks {
            track.jitter.pull(&mut self.block);
            for (sum, sample) in self.mixed.iter_mut().zip(&self.block) {
                *sum += sample;
            }
        }
        for sample in &mut self.mixed {
            *sample = sample.clamp(-1.0, 1.0);
        }
        self.reference.played(&self.mixed);

        match self.resampler.as_mut() {
            None => self.ready.extend_from_slice(&self.mixed),
            Some(resampler) => {
                let start = self.ready.len();
                self.ready.resize(start + resampler.samples_per_block, 0.0);
                resampler
                    .resampler
                    .resample_mono(&self.mixed, &mut self.ready[start..]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::audio::jitter;

    fn feed(media_id: MediaId) -> (mpsc::Sender<AudioPacket>, TrackFeed) {
        let (sender, packets) = mpsc::channel(PACKET_QUEUE_DEPTH);
        (
            sender,
            TrackFeed {
                media_id,
                packets,
                jitter: JitterBuffer::new().unwrap(),
            },
        )
    }

    #[test]
    fn an_empty_room_plays_silence_rather_than_noise() {
        let (_registration, feeds) = mpsc::channel(TRACK_QUEUE_DEPTH);
        let mut mixer = Mixer::new(feeds, SAMPLE_RATE, 2, Arc::new(RenderReference::default()));

        let mut output = vec![7.0f32; SAMPLES_PER_BLOCK * 2];
        mixer.fill(&mut output);
        assert!(
            output.iter().all(|sample| *sample == 0.0),
            "nothing playing must leave silence"
        );
    }

    #[test]
    fn a_track_is_torn_down_when_its_peer_goes() {
        let (registration, feeds) = mpsc::channel(TRACK_QUEUE_DEPTH);
        let mut mixer = Mixer::new(feeds, SAMPLE_RATE, 1, Arc::new(RenderReference::default()));
        let (packets, track) = feed(MediaId(7));
        registration.blocking_send(track).unwrap();

        let mut output = vec![0.0f32; SAMPLES_PER_BLOCK];
        mixer.fill(&mut output);
        assert_eq!(mixer.tracks.len(), 1, "the track was registered");

        drop(packets);
        mixer.fill(&mut output);
        assert!(
            mixer.tracks.is_empty(),
            "a track with no sender left must not keep its jitter buffer"
        );
    }

    #[test]
    fn what_a_peer_sent_reaches_the_device_buffer() {
        let (registration, feeds) = mpsc::channel(TRACK_QUEUE_DEPTH);
        let mut mixer = Mixer::new(feeds, SAMPLE_RATE, 2, Arc::new(RenderReference::default()));
        let (packets, track) = feed(MediaId(7));
        registration.blocking_send(track).unwrap();

        let mut encoder =
            ropus::Encoder::builder(SAMPLE_RATE, ropus::Channels::Mono, ropus::Application::Voip)
                .bitrate(ropus::Bitrate::Bits(24_000))
                .build()
                .unwrap();
        let mut encoded = vec![0u8; 1_000];

        let mut output = vec![0.0f32; SAMPLES_PER_BLOCK * 2];
        let mut loudest = 0.0f32;
        for seq in 0..40u32 {
            let pcm: Vec<f32> = (0..SAMPLES_PER_BLOCK * 2)
                .map(|index| ((seq as usize * 960 + index) as f32 * 0.04).sin() * 0.4)
                .collect();
            let len = encoder.encode_float(&pcm, &mut encoded).unwrap();
            packets
                .blocking_send(jitter::packet(MediaId(7), seq, &encoded[..len]))
                .unwrap();

            mixer.fill(&mut output);
            loudest = loudest.max(
                output
                    .iter()
                    .map(|sample| sample.abs())
                    .fold(0.0f32, f32::max),
            );
        }

        assert!(
            loudest > 0.01,
            "the peer's audio should reach the device, loudest sample was {loudest}"
        );
    }
}
