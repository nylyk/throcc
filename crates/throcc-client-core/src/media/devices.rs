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

/// Capture ring capacity in seconds of audio, at the device rate.
const RING_SECONDS: u32 = 1;
/// Playback ring capacity in milliseconds of audio, room for the few pulls kept queued.
const PLAYBACK_RING_MS: u32 = 200;

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
    /// Errors either stream reported, such as an xrun or the device going away. Counted
    /// rather than logged, since the callback runs on the device's real-time thread.
    pub stream_errors: AtomicU64,
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
    // On Windows, cpal puts the calling thread in a single-threaded COM apartment until the
    // thread exits. On a thread-pool thread that breaks process loopback activation later
    // run there (RPC_E_CHANGED_MODE), so the listing gets a thread of its own.
    std::thread::spawn(list_here).join().unwrap_or_default()
}

fn list_here() -> (Vec<String>, Vec<String>) {
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

/// The sample formats the callbacks take, best first.
const HANDLED: [SampleFormat; 3] = [SampleFormat::F32, SampleFormat::I16, SampleFormat::I32];

/// The rate to open a device whose default config is `default` at. CoreAudio sets a
/// device's rate for every app to the one a stream opens at, so on macOS devices open at the
/// rate they run at and the audio thread resamples. Elsewhere, and at a rate the audio
/// thread's 10 ms pulls don't divide into whole frames, they open at [`SAMPLE_RATE`] where
/// they can.
fn open_rate(default: &SupportedStreamConfig) -> u32 {
    let rate = default.sample_rate();
    if cfg!(target_os = "macos") && rate.is_multiple_of(100) {
        rate
    } else {
        SAMPLE_RATE
    }
}

/// The configs to open a device with, best first: one at `rate` in a sample format the
/// callbacks take, preferring the default's channel count and then F32; then the default
/// config as it is, unless the audio thread's 10 ms pulls don't divide its rate into whole
/// frames, which would make the audio drift.
fn configs<E>(
    default: SupportedStreamConfig,
    supported: Result<impl Iterator<Item = SupportedStreamConfigRange>, E>,
    rate: u32,
) -> Vec<SupportedStreamConfig> {
    let rank = |format: SampleFormat| HANDLED.iter().position(|handled| *handled == format);
    let at_rate = supported
        .into_iter()
        .flatten()
        .filter_map(|range| range.try_with_sample_rate(rate))
        .filter_map(|config| {
            Some((
                config.channels() != default.channels(),
                rank(config.sample_format())?,
                config,
            ))
        })
        .min_by_key(|(other_channels, rank, _)| (*other_channels, *rank))
        .map(|(.., config)| config);
    let mut configs = Vec::from_iter(at_rate);
    if default.sample_rate().is_multiple_of(100) && !configs.contains(&default) {
        configs.push(default);
    }
    configs
}

/// Opens a stream with the first of `configs` that works, or returns the last error.
fn open_first<T>(
    configs: Vec<SupportedStreamConfig>,
    mut open: impl FnMut(&SupportedStreamConfig) -> Result<T, DeviceError>,
) -> Result<T, DeviceError> {
    let mut last = DeviceError::Cpal(format!(
        "the device offers no stream config at {SAMPLE_RATE} Hz or another rate divisible by 100"
    ));
    for config in configs {
        match open(&config) {
            Ok(opened) => return Ok(opened),
            Err(e) => {
                tracing::debug!(?config, "could not open the audio device: {e}");
                last = e;
            }
        }
    }
    Err(last)
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
    let counters = Arc::new(DeviceCounters::default());

    let input_default = input_device.default_input_config().map_err(cpal_error)?;
    let input_configs = configs(
        input_default,
        input_device.supported_input_configs(),
        open_rate(&input_default),
    );
    let (input_stream, input_rate, capture) = open_first(input_configs, |config| {
        let rate = config.sample_rate();
        let (ring, capture) = rtrb::RingBuffer::new((rate * RING_SECONDS) as usize);
        let counters = counters.clone();
        let stream_config = config.config();
        let stream = match config.sample_format() {
            SampleFormat::F32 => build_input::<f32>(&input_device, &stream_config, ring, counters),
            SampleFormat::I16 => build_input::<i16>(&input_device, &stream_config, ring, counters),
            SampleFormat::I32 => build_input::<i32>(&input_device, &stream_config, ring, counters),
            other => Err(DeviceError::Format("input", other)),
        }?;
        stream.play().map_err(cpal_error)?;
        Ok((stream, rate, capture))
    })?;

    let output_default = output_device.default_output_config().map_err(cpal_error)?;
    let output_configs = configs(
        output_default,
        output_device.supported_output_configs(),
        open_rate(&output_default),
    );
    let (output_stream, output_rate, playback) = open_first(output_configs, |config| {
        let rate = config.sample_rate();
        let (playback, ring) = rtrb::RingBuffer::new((rate * PLAYBACK_RING_MS / 1000) as usize * 2);
        let counters = counters.clone();
        let stream_config = config.config();
        let stream = match config.sample_format() {
            SampleFormat::F32 => {
                build_output::<f32>(&output_device, &stream_config, ring, counters)
            }
            SampleFormat::I16 => {
                build_output::<i16>(&output_device, &stream_config, ring, counters)
            }
            SampleFormat::I32 => {
                build_output::<i32>(&output_device, &stream_config, ring, counters)
            }
            other => Err(DeviceError::Format("output", other)),
        }?;
        stream.play().map_err(cpal_error)?;
        Ok((stream, rate, playback))
    })?;

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
    let errors = counters.clone();
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
            move |_| {
                errors.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
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
    let errors = counters.clone();
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
            move |_| {
                errors.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(cpal_error)
}

#[cfg(test)]
mod tests {
    use cpal::SupportedBufferSize;

    use super::*;

    fn range(channels: u16, rates: (u32, u32), format: SampleFormat) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            rates.0,
            rates.1,
            SupportedBufferSize::Unknown,
            format,
        )
    }

    fn config(channels: u16, rate: u32, format: SampleFormat) -> SupportedStreamConfig {
        SupportedStreamConfig::new(channels, rate, SupportedBufferSize::Unknown, format)
    }

    fn picked(
        default: SupportedStreamConfig,
        ranges: &[SupportedStreamConfigRange],
    ) -> Vec<SupportedStreamConfig> {
        configs::<()>(default, Ok(ranges.iter().copied()), SAMPLE_RATE)
    }

    #[test]
    fn a_handled_format_at_48k_comes_first_then_the_default() {
        use SampleFormat::{F32, I16, I32, U8};
        let alsa_hw = config(2, 44_100, I32);
        let ranges = [
            range(2, (8_000, 192_000), I32),
            range(2, (8_000, 192_000), I16),
        ];
        assert_eq!(picked(alsa_hw, &ranges), [config(2, 48_000, I16), alsa_hw]);

        let default = config(2, 44_100, F32);
        let ranges = [
            range(2, (44_100, 44_100), F32),
            range(1, (48_000, 48_000), F32),
            range(2, (48_000, 48_000), U8),
        ];
        assert_eq!(picked(default, &ranges), [config(1, 48_000, F32), default]);

        let ranges = [
            range(2, (48_000, 48_000), I16),
            range(2, (48_000, 48_000), F32),
            range(1, (48_000, 48_000), F32),
        ];
        assert_eq!(picked(default, &ranges), [config(2, 48_000, F32), default]);

        let default = config(2, 48_000, F32);
        assert_eq!(
            picked(default, &[range(2, (48_000, 48_000), F32)]),
            [default]
        );
        let unknown: Result<std::iter::Empty<_>, ()> = Err(());
        assert_eq!(configs(default, unknown, SAMPLE_RATE), [default]);
    }

    #[test]
    fn a_device_kept_at_its_rate_opens_at_that_rate_only() {
        use SampleFormat::{F32, I16};
        let default = config(2, 44_100, I16);
        let ranges = [
            range(2, (8_000, 192_000), I16),
            range(2, (8_000, 192_000), F32),
            range(1, (48_000, 48_000), F32),
        ];
        let kept = configs::<()>(default, Ok(ranges.iter().copied()), 44_100);
        assert_eq!(kept, [config(2, 44_100, F32), default]);
        assert!(kept.iter().all(|c| c.sample_rate() == 44_100));

        let only_48k = [range(2, (48_000, 48_000), F32)];
        assert_eq!(
            configs::<()>(default, Ok(only_48k.iter().copied()), 44_100),
            [default]
        );
    }

    #[test]
    fn a_default_at_a_rate_without_whole_10_ms_pulls_is_never_a_fallback() {
        use SampleFormat::F32;
        let default = config(2, 22_050, F32);
        let ranges = [range(2, (22_050, 22_050), F32)];
        assert!(picked(default, &ranges).is_empty());
        let unknown: Result<std::iter::Empty<_>, ()> = Err(());
        assert!(configs(default, unknown, SAMPLE_RATE).is_empty());

        let ranges = [
            range(2, (22_050, 22_050), F32),
            range(1, (8_000, 48_000), F32),
        ];
        assert_eq!(picked(default, &ranges), [config(1, 48_000, F32)]);
    }

    #[test]
    fn only_macos_keeps_the_device_rate_and_only_in_whole_10_ms_pulls() {
        let kept = if cfg!(target_os = "macos") {
            44_100
        } else {
            SAMPLE_RATE
        };
        assert_eq!(open_rate(&config(2, 44_100, SampleFormat::F32)), kept);
        assert_eq!(
            open_rate(&config(2, 22_050, SampleFormat::F32)),
            SAMPLE_RATE
        );
    }

    #[cfg(windows)]
    #[test]
    fn listing_leaves_the_calling_thread_free_to_join_the_multithreaded_apartment() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

        list();
        // SAFETY: called on this test's own thread, paired with `CoUninitialize` on success.
        let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        assert!(joined.is_ok(), "{joined:?}");
        // SAFETY: COM was initialised on this thread just above.
        unsafe { CoUninitialize() };
    }
}
