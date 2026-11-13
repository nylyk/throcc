use std::sync::Arc;
use std::sync::atomic::Ordering;

use ndk::audio::{
    AudioCallbackResult, AudioContentType, AudioDirection, AudioError, AudioFormat,
    AudioInputPreset, AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder,
    AudioStreamDataCallback, AudioStreamState, AudioUsage,
};

use super::{DeviceCounters, DeviceError, Devices, PLAYBACK_RING_MS, RING_SECONDS};
use crate::media::audio::SAMPLE_RATE;

const STOP_WAIT_NANOS: i64 = 100_000_000;

/// The running speaker stream, and the mic's where it is open.
pub struct Streams {
    mic: Option<AudioStream>,
    speaker: AudioStream,
}

// SAFETY: AAudio lets a stream be stopped and closed on any thread.
unsafe impl Send for Streams {}

impl Drop for Streams {
    fn drop(&mut self) {
        // A callback must not run once the streams close and free what it uses.
        for stream in self.mic.iter().chain([&self.speaker]) {
            let _ = stream.request_stop();
            let _ = stream.wait_for_state_change(AudioStreamState::Stopping, STOP_WAIT_NANOS);
        }
    }
}

/// Opens the phone's speaker, and its mic `with_mic`, as voice-communication streams, which
/// the platform cancels echo between. The app sets the audio mode and device first.
pub fn open(with_mic: bool) -> Result<Devices, DeviceError> {
    let counters = Arc::new(DeviceCounters::default());

    let (playback, ring) =
        rtrb::RingBuffer::new((SAMPLE_RATE * PLAYBACK_RING_MS / 1000) as usize * 2);
    let speaker = builder(&counters)?
        .direction(AudioDirection::Output)
        .usage(AudioUsage::VoiceCommunication)
        .content_type(AudioContentType::Speech)
        .channel_count(2)
        .data_callback(plays(ring, counters.clone()));
    let (speaker, output_rate) = start(speaker)?;

    let (ring, capture) = rtrb::RingBuffer::new((SAMPLE_RATE * RING_SECONDS) as usize);
    let (mic, input_rate) = if with_mic {
        let mic = builder(&counters)?
            .direction(AudioDirection::Input)
            .input_preset(AudioInputPreset::VoiceCommunication)
            .channel_count(1)
            .data_callback(hears(ring, counters.clone()));
        let (mic, rate) = start(mic)?;
        (Some(mic), rate)
    } else {
        (None, SAMPLE_RATE)
    };

    Ok(Devices {
        _streams: Streams { mic, speaker },
        input_rate,
        output_rate,
        capture,
        playback,
        counters,
    })
}

/// A builder for a shared float stream, at [`SAMPLE_RATE`] where the device gives it, that
/// counts its errors.
fn builder(counters: &Arc<DeviceCounters>) -> Result<AudioStreamBuilder, DeviceError> {
    let errors = counters.clone();
    Ok(AudioStreamBuilder::new()
        .map_err(failed)?
        .sharing_mode(AudioSharingMode::Shared)
        .performance_mode(AudioPerformanceMode::LowLatency)
        .format(AudioFormat::PCM_Float)
        .sample_rate(SAMPLE_RATE as i32)
        .error_callback(Box::new(move |_, _| {
            errors.stream_errors.fetch_add(1, Ordering::Relaxed);
        })))
}

/// Opens and starts the stream `builder` describes, returning it with the rate it runs at.
fn start(builder: AudioStreamBuilder) -> Result<(AudioStream, u32), DeviceError> {
    let stream = builder.open_stream().map_err(failed)?;
    if stream.format() != AudioFormat::PCM_Float {
        return Err(DeviceError::Backend(
            "the device gave a stream that isn't float".into(),
        ));
    }
    let rate = u32::try_from(stream.sample_rate()).unwrap_or(0);
    if rate == 0 || !rate.is_multiple_of(100) {
        return Err(DeviceError::Backend(format!(
            "the device runs at {rate} Hz, which the 10 ms pulls don't divide into whole frames"
        )));
    }
    stream.request_start().map_err(failed)?;
    Ok((stream, rate))
}

fn failed(error: AudioError) -> DeviceError {
    DeviceError::Backend(error.to_string())
}

/// The frames `stream` hands its callback as samples: how many, and how many a frame.
fn layout(stream: &AudioStream, frames: i32) -> (usize, usize) {
    let channels = usize::try_from(stream.channel_count()).unwrap_or(1).max(1);
    (usize::try_from(frames).unwrap_or(0) * channels, channels)
}

/// The callback plays interleaved stereo from the ring: left and right on the first two
/// channels, their mean on any others. It never blocks, allocates or panics; an empty ring
/// plays silence.
fn plays(mut ring: rtrb::Consumer<f32>, counters: Arc<DeviceCounters>) -> AudioStreamDataCallback {
    Box::new(move |stream, data, frames| {
        let (samples, channels) = layout(stream, frames);
        // SAFETY: AAudio hands a buffer of `frames` frames of the stream's float format.
        let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<f32>(), samples) };
        for frame in out.chunks_mut(channels) {
            let (left, right) = if ring.slots() >= 2 {
                (ring.pop().unwrap_or(0.0), ring.pop().unwrap_or(0.0))
            } else {
                counters.underruns.fetch_add(1, Ordering::Relaxed);
                (0.0, 0.0)
            };
            frame.fill((left + right) * 0.5);
            if let [first, second, ..] = frame {
                *first = left;
                *second = right;
            }
        }
        AudioCallbackResult::Continue
    })
}

/// The callback downmixes to mono into the ring. It never blocks, allocates or panics.
fn hears(mut ring: rtrb::Producer<f32>, counters: Arc<DeviceCounters>) -> AudioStreamDataCallback {
    Box::new(move |stream, data, frames| {
        let (samples, channels) = layout(stream, frames);
        // SAFETY: AAudio hands a buffer of `frames` frames of the stream's float format.
        let heard = unsafe { std::slice::from_raw_parts(data.cast::<f32>(), samples) };
        for frame in heard.chunks(channels) {
            let mean = frame.iter().sum::<f32>() / channels as f32;
            if ring.push(mean).is_err() {
                counters.overruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        AudioCallbackResult::Continue
    })
}
