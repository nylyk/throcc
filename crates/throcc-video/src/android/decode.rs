use std::collections::VecDeque;

use throcc_proto::messages::Codec;
use tracing::{info, warn};

use super::codec::{self, MediaCodec};
use crate::frame::VideoFrame;
use crate::software::{dav1d, vpx};
use crate::{DecodeError, Frame, MaxSize, Output};

/// How long, in 90 kHz media time, MediaCodec may keep failing on AV1 or VP9 before dav1d
/// or libvpx take over.
const HARDWARE_GRACE: u32 = 3 * 90_000;
/// Units whose timestamps are remembered until their picture comes out.
const IN_FLIGHT: usize = 64;

/// Decodes one remote share into the view the app hands over through its [`Output`]:
/// MediaCodec renders straight into it, hardware first, and AV1 or VP9 that MediaCodec
/// can't take decode in dav1d or libvpx and are copied into it. Without a view there is no
/// codec; one starts with the next view, from the next keyframe.
pub struct Decoder {
    codec: Codec,
    max_size: MaxSize,
    output: Output,
    /// The output's change count when last looked at.
    seen: u64,
    engine: Engine,
    /// Whether reference pictures were dropped since [`Decoder::take_flushed`] last ran.
    flushed: bool,
    /// Each unit's sequence number and timestamp, until its picture comes out.
    in_flight: VecDeque<(u64, u32)>,
    sequence: u64,
    /// The timestamp of the first unit in the current run of MediaCodec failures.
    failing_since: Option<u32>,
}

enum Engine {
    /// MediaCodec, while there is a view to render into.
    MediaCodec(Option<MediaCodec>),
    Software {
        decoder: Software,
        spare: Option<VideoFrame>,
    },
}

enum Software {
    Dav1d(dav1d::Decoder),
    Vpx(vpx::Decoder),
}

impl Decoder {
    pub fn new(codec: Codec, max_size: MaxSize, output: Output) -> Result<Self, DecodeError> {
        let decoder = Self {
            codec,
            max_size,
            seen: output.changes().wrapping_sub(1),
            output,
            engine: Engine::MediaCodec(None),
            flushed: false,
            in_flight: VecDeque::new(),
            sequence: 0,
            failing_since: None,
        };
        info!("decoding {codec:?} in MediaCodec");
        Ok(decoder)
    }

    /// Software pictures hand their planes back for the next one.
    pub fn recycle(&mut self, frame: VideoFrame) {
        if let Engine::Software { spare, .. } = &mut self.engine {
            *spare = Some(frame);
        }
    }

    pub fn take_flushed(&mut self) -> bool {
        std::mem::take(&mut self.flushed)
    }

    pub fn engine(&self) -> &'static str {
        match &self.engine {
            Engine::MediaCodec(_) => "MediaCodec",
            Engine::Software {
                decoder: Software::Dav1d(_),
                ..
            } => "dav1d (software)",
            Engine::Software {
                decoder: Software::Vpx(_),
                ..
            } => "libvpx (software)",
        }
    }

    /// Shows `frame`, the newest picture due, in the view.
    pub fn present(&mut self, frame: &Frame) {
        match frame {
            Frame::Codec(frame) => frame.show(),
            Frame::Planes(frame) => {
                if let Some(view) = self.output.view()
                    && !view.show(frame)
                {
                    warn!(
                        "can't show a {}×{} picture in this view",
                        frame.width, frame.height
                    );
                }
            }
        }
    }

    pub fn decode(&mut self, unit: &[u8], timestamp: u32) -> Result<Option<Frame>, DecodeError> {
        self.follow_view();
        let in_codec = match &self.engine {
            Engine::MediaCodec(None) => return Ok(None),
            Engine::MediaCodec(Some(_)) => true,
            Engine::Software { .. } => false,
        };
        if in_codec {
            let decoded = self.decode_in_codec(unit, timestamp);
            if decoded.is_err() && self.hardware_failed_for_long(timestamp) {
                self.switch_to_software()?;
                return self.decode(unit, timestamp);
            }
            if let Ok(Some(_)) = decoded {
                self.failing_since = None;
            }
            return decoded;
        }
        match &mut self.engine {
            Engine::MediaCodec(_) => Ok(None),
            Engine::Software { decoder, spare } => {
                let spare = spare.take();
                let decoded = match decoder {
                    Software::Dav1d(d) => d.decode(unit, timestamp, spare)?,
                    Software::Vpx(d) => d.decode(unit, timestamp, spare)?,
                };
                Ok(decoded.map(Frame::Planes))
            }
        }
    }

    /// Starts, moves or stops the codec when the app changed the view.
    fn follow_view(&mut self) {
        let changes = self.output.changes();
        if changes == self.seen {
            return;
        }
        self.seen = changes;
        let Engine::MediaCodec(codec) = &mut self.engine else {
            return;
        };
        let view = self.output.view();
        match (codec.as_mut(), view) {
            (Some(running), Some(view)) => {
                if !running.retarget(view.clone()) {
                    *codec = None;
                    *codec = MediaCodec::open(self.codec, self.max_size, view);
                    self.flushed = true;
                }
            }
            (_, None) => {
                *codec = None;
                self.in_flight.clear();
            }
            (None, Some(view)) => {
                *codec = MediaCodec::open(self.codec, self.max_size, view);
                // A new codec decodes from the next keyframe on.
                self.flushed = true;
                if codec.is_none() {
                    warn!("MediaCodec can't decode this {:?} share", self.codec);
                    if self.switch_to_software().is_err() {
                        self.flushed = false;
                    }
                }
            }
        }
    }

    fn decode_in_codec(
        &mut self,
        unit: &[u8],
        timestamp: u32,
    ) -> Result<Option<Frame>, DecodeError> {
        let Engine::MediaCodec(Some(codec)) = &mut self.engine else {
            return Ok(None);
        };
        self.sequence += 1;
        let sequence = self.sequence;
        let (queued, outputs) = codec
            .decode(unit, sequence, self.max_size)
            .map_err(DecodeError::MediaCodec)?;
        if queued {
            if self.in_flight.len() == IN_FLIGHT {
                self.in_flight.pop_front();
            }
            self.in_flight.push_back((sequence, timestamp));
        } else {
            warn!("MediaCodec had no free input buffer; a unit was dropped");
            self.flushed = true;
        }
        let mut newest = None;
        for (sequence, output) in outputs {
            match output {
                codec::Output::Frame(mut frame) => {
                    frame.timestamp = self.timestamp_of(sequence).unwrap_or(timestamp);
                    newest = Some(Frame::Codec(frame));
                }
                codec::Output::TooLarge(width, height) => {
                    return Err(DecodeError::TooLarge(width, height));
                }
            }
        }
        Ok(newest)
    }

    /// The timestamp of the unit queued as `sequence`, forgetting it and every older one.
    fn timestamp_of(&mut self, sequence: u64) -> Option<u32> {
        while let Some(&(queued, timestamp)) = self.in_flight.front() {
            self.in_flight.pop_front();
            if queued == sequence {
                return Some(timestamp);
            }
            if queued > sequence {
                break;
            }
        }
        None
    }

    fn hardware_failed_for_long(&mut self, timestamp: u32) -> bool {
        if !matches!(self.codec, Codec::Av1 | Codec::Vp9) {
            return false;
        }
        let since = *self.failing_since.get_or_insert(timestamp);
        timestamp.wrapping_sub(since) >= HARDWARE_GRACE
    }

    /// Hands AV1 or VP9 to dav1d or libvpx for good.
    fn switch_to_software(&mut self) -> Result<(), DecodeError> {
        let decoder = match self.codec {
            Codec::Av1 => Software::Dav1d(dav1d::Decoder::new(self.max_size)?),
            Codec::Vp9 => Software::Vpx(vpx::Decoder::new(self.max_size)?),
            codec => return Err(DecodeError::NoDecoder(codec)),
        };
        warn!("decoding {:?} in software", self.codec);
        self.engine = Engine::Software {
            decoder,
            spare: None,
        };
        self.in_flight.clear();
        self.flushed = true;
        Ok(())
    }
}
