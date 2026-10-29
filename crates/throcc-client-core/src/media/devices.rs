#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SizedSample, Stream, StreamConfig};

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

fn find(devices: impl Iterator<Item = Device>, name: &str) -> Option<Device> {
    devices
        .into_iter()
        .find(|d| d.description().is_ok_and(|desc| desc.name() == name))
}

/// Opens the named devices, or the system defaults.
pub fn open(input: Option<&str>, output: Option<&str>) -> Result<Devices, DeviceError> {
    let host = cpal::default_host();
    let input_device = match input {
        Some(name) => find(host.input_devices().map_err(cpal_error)?, name)
            .ok_or_else(|| DeviceError::NotFound(name.to_owned()))?,
        None => host
            .default_input_device()
            .ok_or(DeviceError::Missing("input"))?,
    };
    let output_device = match output {
        Some(name) => find(host.output_devices().map_err(cpal_error)?, name)
            .ok_or_else(|| DeviceError::NotFound(name.to_owned()))?,
        None => host
            .default_output_device()
            .ok_or(DeviceError::Missing("output"))?,
    };

    let input_config = input_device.default_input_config().map_err(cpal_error)?;
    let output_config = output_device.default_output_config().map_err(cpal_error)?;
    let input_rate = prefer_48k(&input_device, true).unwrap_or(input_config.sample_rate());
    let output_rate = prefer_48k(&output_device, false).unwrap_or(output_config.sample_rate());

    let counters = Arc::new(DeviceCounters::default());
    let (capture_tx, capture) = rtrb::RingBuffer::new((input_rate * RING_SECONDS) as usize);
    let (playback, playback_rx) = rtrb::RingBuffer::new((output_rate * RING_SECONDS) as usize);

    let in_config = StreamConfig {
        channels: input_config.channels(),
        sample_rate: input_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let out_config = StreamConfig {
        channels: output_config.channels(),
        sample_rate: output_rate,
        buffer_size: cpal::BufferSize::Default,
    };
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

/// 48 kHz if the device supports it, so no resampling is needed.
fn prefer_48k(device: &Device, input: bool) -> Option<u32> {
    let supports = |min: u32, max: u32| (min..=max).contains(&48_000);
    let found = if input {
        device
            .supported_input_configs()
            .ok()?
            .any(|c| supports(c.min_sample_rate(), c.max_sample_rate()))
    } else {
        device
            .supported_output_configs()
            .ok()?
            .any(|c| supports(c.min_sample_rate(), c.max_sample_rate()))
    };
    found.then_some(48_000)
}

trait ToF32: SizedSample + Send + 'static {
    fn to_f32(self) -> f32;
    fn from_f32(sample: f32) -> Self;
}

impl ToF32 for f32 {
    fn to_f32(self) -> f32 {
        self
    }
    fn from_f32(sample: f32) -> Self {
        sample
    }
}

impl ToF32 for i16 {
    fn to_f32(self) -> f32 {
        f32::from(self) / 32768.0
    }
    fn from_f32(sample: f32) -> Self {
        (sample.clamp(-1.0, 1.0) * 32767.0) as i16
    }
}

/// The callback downmixes to mono into the ring. It never blocks, allocates or panics.
fn build_input<T: ToF32>(
    device: &Device,
    config: &StreamConfig,
    mut ring: rtrb::Producer<f32>,
    counters: Arc<DeviceCounters>,
) -> Result<Stream, DeviceError> {
    let channels = usize::from(config.channels.max(1));
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| {
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|s| s.to_f32()).sum();
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

/// The callback copies mono samples from the ring to every channel. It never blocks,
/// allocates or panics; an empty ring plays silence.
fn build_output<T: ToF32>(
    device: &Device,
    config: &StreamConfig,
    mut ring: rtrb::Consumer<f32>,
    counters: Arc<DeviceCounters>,
) -> Result<Stream, DeviceError> {
    let channels = usize::from(config.channels.max(1));
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _| {
                for frame in data.chunks_mut(channels) {
                    let sample = ring.pop().unwrap_or_else(|_| {
                        counters.underruns.fetch_add(1, Ordering::Relaxed);
                        0.0
                    });
                    frame.fill(T::from_f32(sample));
                }
            },
            |e| tracing::warn!("output stream: {e}"),
            None,
        )
        .map_err(cpal_error)
}
