use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitInitialize, AudioUnitPropertyID, AudioUnitRender,
    AudioUnitRenderActionFlags, AudioUnitScope, AudioUnitSetProperty, AudioUnitUninitialize,
    kAudioOutputUnitProperty_EnableIO, kAudioOutputUnitProperty_SetInputCallback,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output, kAudioUnitSubType_VoiceProcessingIO, kAudioUnitType_Output,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
    kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM,
};

use super::{DeviceCounters, DeviceError, Devices, PLAYBACK_RING_MS, RING_SECONDS};
use crate::media::audio::SAMPLE_RATE;

/// The unit's bus to the speaker.
const SPEAKER_BUS: u32 = 0;
/// The unit's bus from the mic.
const MIC_BUS: u32 = 1;
/// The most frames one callback moves. The system asks for at most 4096, with the screen
/// locked.
const MAX_FRAMES: usize = 4096;
const SAMPLE_BYTES: u32 = size_of::<f32>() as u32;
/// The value of `kAudioUnitSubType_RemoteIO`.
const REMOTE_IO: u32 = u32::from_be_bytes(*b"rioc");

/// The running audio unit, with what its callbacks use.
pub struct Streams {
    unit: AudioUnit,
    /// Null without a mic.
    mic: *mut Mic,
    speaker: *mut Speaker,
}

// SAFETY: an audio unit may be stopped and disposed of on any thread, and only the unit's
// callbacks reach `mic` and `speaker` until then.
unsafe impl Send for Streams {}

impl Drop for Streams {
    fn drop(&mut self) {
        // SAFETY: `unit` is the live unit `open` made. Once it is disposed of, no callback
        // runs, so the boxes `open` leaked for them are ours again.
        unsafe {
            AudioOutputUnitStop(self.unit);
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
            if !self.mic.is_null() {
                drop(Box::from_raw(self.mic));
            }
            drop(Box::from_raw(self.speaker));
        }
    }
}

struct Mic {
    unit: AudioUnit,
    ring: rtrb::Producer<f32>,
    counters: Arc<DeviceCounters>,
    heard: [f32; MAX_FRAMES],
}

struct Speaker {
    ring: rtrb::Consumer<f32>,
    counters: Arc<DeviceCounters>,
}

/// Opens the phone's speaker, and its mic `with_mic`, through one audio unit at
/// [`SAMPLE_RATE`]. With the mic it is the voice-processing unit, which cancels echo. The
/// app sets up and activates its audio session first.
pub fn open(with_mic: bool) -> Result<Devices, DeviceError> {
    let counters = Arc::new(DeviceCounters::default());
    let (capture_ring, capture) = rtrb::RingBuffer::new((SAMPLE_RATE * RING_SECONDS) as usize);
    let (playback, playback_ring) =
        rtrb::RingBuffer::new((SAMPLE_RATE * PLAYBACK_RING_MS / 1000) as usize * 2);

    let unit = new_unit(with_mic)?;
    let speaker = Box::into_raw(Box::new(Speaker {
        ring: playback_ring,
        counters: counters.clone(),
    }));
    let mic = if with_mic {
        Box::into_raw(Box::new(Mic {
            unit,
            ring: capture_ring,
            counters: counters.clone(),
            heard: [0.0; MAX_FRAMES],
        }))
    } else {
        ptr::null_mut()
    };
    let streams = Streams { unit, mic, speaker };

    let format = mono_at_sample_rate();
    set(
        unit,
        kAudioUnitProperty_StreamFormat,
        kAudioUnitScope_Input,
        SPEAKER_BUS,
        &format,
    )?;
    let plays = AURenderCallbackStruct {
        inputProc: Some(play),
        inputProcRefCon: speaker.cast(),
    };
    set(
        unit,
        kAudioUnitProperty_SetRenderCallback,
        kAudioUnitScope_Input,
        SPEAKER_BUS,
        &plays,
    )?;
    if with_mic {
        let on: u32 = 1;
        set(
            unit,
            kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Input,
            MIC_BUS,
            &on,
        )?;
        set(
            unit,
            kAudioUnitProperty_StreamFormat,
            kAudioUnitScope_Output,
            MIC_BUS,
            &format,
        )?;
        let hears = AURenderCallbackStruct {
            inputProc: Some(hear),
            inputProcRefCon: mic.cast(),
        };
        set(
            unit,
            kAudioOutputUnitProperty_SetInputCallback,
            kAudioUnitScope_Global,
            MIC_BUS,
            &hears,
        )?;
    }
    // SAFETY: `unit` is live, and what its callbacks point at lives until `streams` drops.
    check("starting the audio unit", unsafe {
        AudioUnitInitialize(unit)
    })?;
    // SAFETY: as above.
    check("starting the audio unit", unsafe {
        AudioOutputUnitStart(unit)
    })?;

    Ok(Devices {
        _streams: streams,
        input_rate: SAMPLE_RATE,
        output_rate: SAMPLE_RATE,
        capture,
        playback,
        counters,
    })
}

fn new_unit(with_mic: bool) -> Result<AudioUnit, DeviceError> {
    let subtype = if with_mic {
        kAudioUnitSubType_VoiceProcessingIO
    } else {
        REMOTE_IO
    };
    let mut wanted = AudioComponentDescription {
        componentType: kAudioUnitType_Output,
        componentSubType: subtype,
        componentManufacturer: kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    // SAFETY: a null component starts the search, and `wanted` outlives the call.
    let component = unsafe { AudioComponentFindNext(ptr::null_mut(), NonNull::from(&mut wanted)) };
    if component.is_null() {
        return Err(DeviceError::Missing("voice"));
    }
    let mut unit: AudioUnit = ptr::null_mut();
    // SAFETY: `component` was just found, and `unit` is a valid place for the instance.
    let status = unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut unit)) };
    check("making the audio unit", status)?;
    Ok(unit)
}

/// One channel of packed floats at [`SAMPLE_RATE`]. The unit converts to what the hardware
/// runs at.
fn mono_at_sample_rate() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: SAMPLE_BYTES,
        mFramesPerPacket: 1,
        mBytesPerFrame: SAMPLE_BYTES,
        mChannelsPerFrame: 1,
        mBitsPerChannel: SAMPLE_BYTES * 8,
        mReserved: 0,
    }
}

fn set<T>(
    unit: AudioUnit,
    property: AudioUnitPropertyID,
    scope: AudioUnitScope,
    bus: u32,
    value: &T,
) -> Result<(), DeviceError> {
    // SAFETY: `unit` is live, and `value` is a `T` of the size given, which the unit copies.
    let status = unsafe {
        AudioUnitSetProperty(
            unit,
            property,
            scope,
            bus,
            ptr::from_ref(value).cast(),
            size_of::<T>() as u32,
        )
    };
    check("setting up the audio unit", status)
}

fn check(what: &str, status: i32) -> Result<(), DeviceError> {
    if status == 0 {
        Ok(())
    } else {
        Err(DeviceError::Backend(format!("{what} failed ({status})")))
    }
}

/// Takes what the mic heard into the ring. It never blocks, allocates or panics.
unsafe extern "C-unwind" fn hear(
    mic: NonNull<c_void>,
    flags: NonNull<AudioUnitRenderActionFlags>,
    time: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    _data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: the unit hands back the `Mic` given at setup, which lives until the unit is
    // disposed of, and only this callback touches it.
    let mic = unsafe { mic.cast::<Mic>().as_mut() };
    let Some(heard) = mic.heard.get_mut(..frames as usize) else {
        mic.counters.stream_errors.fetch_add(1, Ordering::Relaxed);
        return 0;
    };
    let mut buffers = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: frames * SAMPLE_BYTES,
            mData: heard.as_mut_ptr().cast(),
        }],
    };
    // SAFETY: `unit` is the live unit calling us, and `buffers` has room for `frames`
    // samples of the mono float format set on the mic bus.
    let status = unsafe {
        AudioUnitRender(
            mic.unit,
            flags.as_ptr(),
            time,
            MIC_BUS,
            frames,
            NonNull::from(&mut buffers),
        )
    };
    if status != 0 {
        mic.counters.stream_errors.fetch_add(1, Ordering::Relaxed);
        return status;
    }
    for sample in heard {
        if mic.ring.push(*sample).is_err() {
            mic.counters.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }
    0
}

/// Plays the mean of left and right from the ring. It never blocks, allocates or panics;
/// an empty ring plays silence.
unsafe extern "C-unwind" fn play(
    speaker: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _time: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: the unit hands back the `Speaker` given at setup, which lives until the unit
    // is disposed of, and only this callback touches it.
    let speaker = unsafe { speaker.cast::<Speaker>().as_mut() };
    // SAFETY: the unit passes one buffer of the mono float format set on the speaker bus.
    let Some([buffer]) = (unsafe { data.as_mut() }).map(|list| &mut list.mBuffers) else {
        return 0;
    };
    if buffer.mData.is_null() {
        return 0;
    }
    let samples = (buffer.mDataByteSize / SAMPLE_BYTES).min(frames) as usize;
    // SAFETY: the buffer holds `mDataByteSize` bytes of floats for this call.
    let out = unsafe { std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), samples) };
    for sample in out {
        *sample = if speaker.ring.slots() >= 2 {
            (speaker.ring.pop().unwrap_or(0.0) + speaker.ring.pop().unwrap_or(0.0)) * 0.5
        } else {
            speaker.counters.underruns.fetch_add(1, Ordering::Relaxed);
            0.0
        };
    }
    0
}
