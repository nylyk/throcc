use std::time::Duration;

use bytes::Bytes;
use ndk_sys as ndk;
use throcc_proto::messages::{Codec, Recovery};
use tracing::{debug, info};

use super::View;
use super::codec::mime;
use crate::frame::VideoFrame;
use crate::{EncodeError, EncodedUnit, EncoderConfig, switches};

/// The encoders tried in order: HEVC, then H.264.
const LADDER: &[Codec] = &[Codec::Hevc, Codec::H264];
/// A GOP cap of this many seconds bounds how long a picture stays broken without a request.
const GOP_SECONDS: i32 = 4;
/// How long a still screen goes without a picture before the encoder sends the last again.
const REPEAT_AFTER_US: i64 = 500_000;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface`.
const COLOR_FORMAT_SURFACE: i32 = 0x7F00_0789;
/// `MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR`.
const BITRATE_MODE_CBR: i32 = 2;
/// `MediaFormat.COLOR_RANGE_FULL`, as every share is encoded.
const COLOR_RANGE_FULL: i32 = 1;
/// `MediaFormat.COLOR_STANDARD_BT709`.
const COLOR_STANDARD_BT709: i32 = 1;
/// `MediaFormat.COLOR_TRANSFER_SDR_VIDEO`.
const COLOR_TRANSFER_SDR_VIDEO: i32 = 3;

/// A low-latency MediaCodec encoder fed through its input surface, which the screen is drawn
/// into: no B-frames, a constant bitrate where the codec offers one, and an IDR on request.
pub struct Encoder {
    codec: *mut ndk::AMediaCodec,
    kind: Codec,
    input: View,
    /// The parameter sets the codec put out first, sent again before every keyframe.
    parameter_sets: Vec<u8>,
}

// SAFETY: the codec is used through `&mut self` only, one thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Opens the first encoder of the ladder that the device configures.
    pub fn open(config: EncoderConfig) -> Result<Self, EncodeError> {
        LADDER
            .iter()
            .filter(|codec| switches::encodes(**codec))
            .find_map(|codec| {
                let encoder = Self::open_with(*codec, config);
                if encoder.is_none() {
                    debug!(?codec, "no usable MediaCodec encoder");
                }
                encoder
            })
            .ok_or(EncodeError::NoEncoder)
    }

    fn open_with(kind: Codec, config: EncoderConfig) -> Option<Self> {
        // SAFETY: the MIME type is a C string literal.
        let codec = unsafe { ndk::AMediaCodec_createEncoderByType(mime(kind).as_ptr()) };
        if codec.is_null() {
            return None;
        }
        // Some codecs refuse the optional keys outright rather than ignore them.
        let configured =
            configure(codec, kind, config, true) || configure(codec, kind, config, false);
        let mut window = std::ptr::null_mut();
        // SAFETY: the codec is live and configured if `configured` holds, and the window it
        // creates comes with a reference of its own.
        let started = configured
            && unsafe {
                ndk::AMediaCodec_createInputSurface(codec, &mut window)
                    == ndk::media_status_t::AMEDIA_OK
                    && ndk::AMediaCodec_start(codec) == ndk::media_status_t::AMEDIA_OK
            };
        // SAFETY: the window came from the codec with a reference we hold, or is null.
        let input = unsafe { View::from_raw(window as usize as u64) };
        let Some(input) = input.filter(|_| started) else {
            // SAFETY: the codec was created above and is used no further.
            unsafe { ndk::AMediaCodec_delete(codec) };
            return None;
        };
        let encoder = Self {
            codec,
            kind,
            input,
            parameter_sets: Vec::new(),
        };
        info!(encoder = encoder.name(), "video encoder ready");
        Some(encoder)
    }

    pub fn codec(&self) -> Codec {
        self.kind
    }

    pub fn name(&self) -> &'static str {
        match self.kind {
            Codec::Hevc => "hevc_mediacodec",
            _ => "h264_mediacodec",
        }
    }

    /// The hardware API doing the work, as people see it named.
    pub fn engine(&self) -> &'static str {
        "MediaCodec"
    }

    /// The encoder heals a broken picture with an IDR on request.
    pub fn recovery(&self) -> Recovery {
        Recovery::Idr
    }

    /// Asks the codec for a keyframe next.
    pub fn recover(&mut self, _kind: Recovery) {
        // SAFETY: the codec is live, and the format is deleted after the call copies it.
        unsafe {
            let format = ndk::AMediaFormat_new();
            ndk::AMediaFormat_setInt32(format, ndk::AMEDIACODEC_KEY_REQUEST_SYNC_FRAME, 0);
            ndk::AMediaCodec_setParameters(self.codec, format);
            ndk::AMediaFormat_delete(format);
        }
    }

    /// Pictures come in through the input surface only.
    pub fn encode(&mut self, _picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        Err(EncodeError::SurfaceOnly)
    }

    /// The surface the encoder takes its pictures from.
    pub fn input(&self) -> &View {
        &self.input
    }

    /// The units the codec has put out, waiting up to `wait` for the first. `stamp` turns
    /// the time a picture was drawn, in nanoseconds on `CLOCK_MONOTONIC`, into the unit's
    /// timestamp; a unit it gives none is dropped.
    pub fn drain(
        &mut self,
        wait: Duration,
        mut stamp: impl FnMut(i64) -> Option<u32>,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        let mut units = Vec::new();
        let mut wait = wait.as_micros() as i64;
        loop {
            // SAFETY: zeroed is how the call expects the struct, which it fills in.
            let mut info: ndk::AMediaCodecBufferInfo = unsafe { std::mem::zeroed() };
            // SAFETY: the codec is live.
            let index =
                unsafe { ndk::AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, wait) };
            match index {
                index if index >= 0 => {
                    wait = 0;
                    let data = self.take_output(index as usize, &info);
                    let flags = info.flags as u32;
                    if flags & ndk::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0 {
                        self.parameter_sets = data;
                        continue;
                    }
                    let Some(timestamp) = stamp(info.presentationTimeUs * 1000) else {
                        continue;
                    };
                    if data.is_empty() {
                        continue;
                    }
                    let keyframe = flags & ndk::AMEDIACODEC_BUFFER_FLAG_KEY_FRAME != 0;
                    let data = if keyframe {
                        [self.parameter_sets.as_slice(), &data].concat()
                    } else {
                        data
                    };
                    units.push(EncodedUnit {
                        data: Bytes::from(data),
                        keyframe,
                        timestamp,
                    });
                }
                index
                    if index == ndk::AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED as isize
                        || index == ndk::AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED as isize => {}
                index if index == ndk::AMEDIACODEC_INFO_TRY_AGAIN_LATER as isize => break,
                error => return Err(EncodeError::MediaCodec(error as i32)),
            }
        }
        Ok(units)
    }

    /// Copies output buffer `index` out and hands it back to the codec.
    fn take_output(&mut self, index: usize, info: &ndk::AMediaCodecBufferInfo) -> Vec<u8> {
        let mut size = 0usize;
        // SAFETY: the codec is live and `index` came from it; the buffer holds `size`
        // bytes, of which the info's range is the unit, until it is released below.
        unsafe {
            let buffer = ndk::AMediaCodec_getOutputBuffer(self.codec, index, &mut size);
            let (offset, len) = (info.offset.max(0) as usize, info.size.max(0) as usize);
            let data = if buffer.is_null() || offset + len > size {
                Vec::new()
            } else {
                std::slice::from_raw_parts(buffer.add(offset), len).to_vec()
            };
            ndk::AMediaCodec_releaseOutputBuffer(self.codec, index, false);
            data
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the codec is live until here.
        unsafe {
            ndk::AMediaCodec_stop(self.codec);
            ndk::AMediaCodec_delete(self.codec);
        }
    }
}

/// Configures `codec` to encode `kind` from a surface, with the keys some codecs refuse
/// when `optional` holds.
fn configure(
    codec: *mut ndk::AMediaCodec,
    kind: Codec,
    config: EncoderConfig,
    optional: bool,
) -> bool {
    let fps = config.fps.max(1);
    // SAFETY: the format is live until deleted below, and the keys are the NDK's.
    unsafe {
        let format = ndk::AMediaFormat_new();
        let int = |key, value: i32| ndk::AMediaFormat_setInt32(format, key, value);
        ndk::AMediaFormat_setString(format, ndk::AMEDIAFORMAT_KEY_MIME, mime(kind).as_ptr());
        int(ndk::AMEDIAFORMAT_KEY_WIDTH, config.width as i32);
        int(ndk::AMEDIAFORMAT_KEY_HEIGHT, config.height as i32);
        int(ndk::AMEDIAFORMAT_KEY_COLOR_FORMAT, COLOR_FORMAT_SURFACE);
        int(ndk::AMEDIAFORMAT_KEY_BIT_RATE, (config.kbps * 1000) as i32);
        int(ndk::AMEDIAFORMAT_KEY_FRAME_RATE, fps as i32);
        int(ndk::AMEDIAFORMAT_KEY_I_FRAME_INTERVAL, GOP_SECONDS);
        int(ndk::AMEDIAFORMAT_KEY_COLOR_RANGE, COLOR_RANGE_FULL);
        int(ndk::AMEDIAFORMAT_KEY_COLOR_STANDARD, COLOR_STANDARD_BT709);
        int(
            ndk::AMEDIAFORMAT_KEY_COLOR_TRANSFER,
            COLOR_TRANSFER_SDR_VIDEO,
        );
        ndk::AMediaFormat_setInt64(
            format,
            ndk::AMEDIAFORMAT_KEY_REPEAT_PREVIOUS_FRAME_AFTER,
            REPEAT_AFTER_US,
        );
        if optional {
            int(ndk::AMEDIAFORMAT_KEY_BITRATE_MODE, BITRATE_MODE_CBR);
            // Realtime priority, and a unit out per picture in: no reordering, no batching.
            int(ndk::AMEDIAFORMAT_KEY_PRIORITY, 0);
            int(ndk::AMEDIAFORMAT_KEY_LATENCY, 1);
            ndk::AMediaFormat_setFloat(
                format,
                ndk::AMEDIAFORMAT_KEY_MAX_FPS_TO_ENCODER,
                fps as f32,
            );
        }
        let status = ndk::AMediaCodec_configure(
            codec,
            format,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            ndk::AMEDIACODEC_CONFIGURE_FLAG_ENCODE as u32,
        );
        ndk::AMediaFormat_delete(format);
        status == ndk::media_status_t::AMEDIA_OK
    }
}
