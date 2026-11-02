#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig,
    SupportedStreamConfigRange,
};

use super::audio::SAMPLE_RATE;

/// Ring capacity in seconds of audio, at the device rate.
const RING_SECONDS: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("no {0} device")]
    Missing(&'static str),
    #[error("no device called {0}")]
    NotFound(String),
    #[error("the {0} device uses {1:?} samples, which throcc doesn't handle")]
    Format(&'static str, SampleFormat),
    #[error("audio device: {0}")]
    Cpal(String),
}

fn cpal_error(e: impl std::fmt::Display) -> DeviceError {
    DeviceError::Cpal(e.to_string())
}

/// Counts of samples the callbacks couldn't move, read by the audio worker.
#[derive(Debug, Default)]
pub struct DeviceCounters {
    /// Mic samples dropped because the capture ring was full.
    pub overruns: AtomicU64,
    /// Speaker samples played as silence because the playback ring was empty.
    pub underruns: AtomicU64,
}

/// An open mic and speaker pair. Dropping it stops both streams.
pub struct Devices {
    _input: Stream,
    _output: Stream,
    pub input_rate: u32,
    pub output_rate: u32,
    pub capture: rtrb::Consumer<f32>,
    /// Left and right samples, interleaved.
    pub playback: rtrb::Producer<f32>,
    pub counters: Arc<DeviceCounters>,
}

/// Names of the available input and output devices.
pub fn list() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let names = |devices: Result<Vec<Device>, _>| -> Vec<String> {
        devices
            .unwrap_or_default()
            .iter()
            .filter_map(|d| d.description().ok().map(|desc| desc.name().to_owned()))
            .collect()
    };
    let inputs = names(host.input_devices().map(|d| d.collect()));
    let outputs = names(host.output_devices().map(|d| d.collect()));
    (inputs, outputs)
}

/// The device called `name` among `all`, or with no name the `default` one.
fn pick<D: Iterator<Item = Device>, E: std::fmt::Display>(
    name: Option<&str>,
    all: impl FnOnce() -> Result<D, E>,
    default: impl FnOnce() -> Option<Device>,
    which: &'static str,
) -> Result<Device, DeviceError> {
    let Some(name) = name else {
        return default().ok_or(DeviceError::Missing(which));
    };
    all()
        .map_err(cpal_error)?
        .find(|d| d.description().is_ok_and(|desc| desc.name() == name))
        .ok_or_else(|| DeviceError::NotFound(name.to_owned()))
}

/// `default` at [`SAMPLE_RATE`] if the device supports it, so no resampling is needed.
fn stream_config<E>(
    default: &SupportedStreamConfig,
    supported: Result<impl Iterator<Item = SupportedStreamConfigRange>, E>,
) -> StreamConfig {
    let at_sample_rate = supported.is_ok_and(|mut ranges| {
        ranges.any(|r| (r.min_sample_rate()..=r.max_sample_rate()).contains(&SAMPLE_RATE))
    });
    StreamConfig {
        channels: default.channels(),
        sample_rate: if at_sample_rate {
            SAMPLE_RATE
        } else {
            default.sample_rate()
        },
        buffer_size: cpal::BufferSize::Default,
    }
}

/// Opens the named devices, or the system defaults.
pub fn open(input: Option<&str>, output: Option<&str>) -> Result<Devices, DeviceError> {
    let host = cpal::default_host();
    let input_device = pick(
        input,
        || host.input_devices(),
        || host.default_input_device(),
        "input",
    )?;
    let output_device = pick(
        output,
        || host.output_devices(),
        || host.default_output_device(),
        "output",
    )?;

    let input_config = input_device.default_input_config().map_err(cpal_error)?;
    let output_config = output_device.default_output_config().map_err(cpal_error)?;
    let in_config = stream_config(&input_config, input_device.supported_input_configs());
    let out_config = stream_config(&output_config, output_device.supported_output_configs());
    let (input_rate, output_rate) = (in_config.sample_rate, out_config.sample_rate);

    let counters = Arc::new(DeviceCounters::default());
    let (capture_tx, capture) = rtrb::RingBuffer::new((input_rate * RING_SECONDS) as usize);
    let (playback, playback_rx) = rtrb::RingBuffer::new((output_rate * RING_SECONDS) as usize * 2);
    let input_stream = match input_config.sample_format() {
        SampleFormat::F32 => {
            build_input::<f32>(&input_device, &in_config, capture_tx, counters.clone())
        }
        SampleFormat::I16 => {
            build_input::<i16>(&input_device, &in_config, capture_tx, counters.clone())
        }
        other => Err(DeviceError::Format("input", other)),
    }?;
    let output_stream = match output_config.sample_format() {
        SampleFormat::F32 => {
            build_output::<f32>(&output_device, &out_config, playback_rx, counters.clone())
        }
        SampleFormat::I16 => {
            build_output::<i16>(&output_device, &out_config, playback_rx, counters.clone())
        }
        other => Err(DeviceError::Format("output", other)),
    }?;
    input_stream.play().map_err(cpal_error)?;
    output_stream.play().map_err(cpal_error)?;
    Ok(Devices {
        _input: input_stream,
        _output: output_stream,
        input_rate,
        output_rate,
        capture,
        playback,
        counters,
    })
}

/// The callback downmixes to mono into the ring. It never blocks, allocates or panics.
fn build_input<T>(
    device: &Device,
    config: &StreamConfig,
    mut ring: rtrb::Producer<f32>,
    counters: Arc<DeviceCounters>,
) -> Result<Stream, DeviceError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| {
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
                    if ring.push(sum / channels as f32).is_err() {
                        counters.overruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            |e| tracing::warn!("input stream: {e}"),
            None,
        )
        .map_err(cpal_error)
}

/// The callback plays interleaved stereo from the ring: left and right on the first two
/// channels, their mean on any others. It never blocks, allocates or panics; an empty ring
/// plays silence.
fn build_output<T>(
    device: &Device,
    config: &StreamConfig,
    mut ring: rtrb::Consumer<f32>,
    counters: Arc<DeviceCounters>,
) -> Result<Stream, DeviceError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels.max(1));
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _| {
                for frame in data.chunks_mut(channels) {
                    let (left, right) = if ring.slots() >= 2 {
                        (ring.pop().unwrap_or(0.0), ring.pop().unwrap_or(0.0))
                    } else {
                        counters.underruns.fetch_add(1, Ordering::Relaxed);
                        (0.0, 0.0)
                    };
                    frame.fill(T::from_sample((left + right) * 0.5));
                    if let [first, second, ..] = frame {
                        *first = T::from_sample(left);
                        *second = T::from_sample(right);
                    }
                }
            },
            |e| tracing::warn!("output stream: {e}"),
            None,
        )
        .map_err(cpal_error)
}
