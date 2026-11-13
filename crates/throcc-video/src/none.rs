//! The backend of a build without one: an encoder and a decoder that never open.

use std::convert::Infallible;

use throcc_proto::messages::{Codec, Recovery};

use crate::frame::VideoFrame;
use crate::{DecodeError, EncodeError, EncodedUnit, EncoderConfig, Frame, MaxSize};
#[cfg(target_os = "linux")]
use crate::{DmaBuf, GpuEncoded};

pub struct Encoder(Infallible);

impl Encoder {
    pub fn open(_config: EncoderConfig) -> Result<Self, EncodeError> {
        Err(EncodeError::NoEncoder)
    }

    pub fn codec(&self) -> Codec {
        match self.0 {}
    }

    pub fn name(&self) -> &'static str {
        match self.0 {}
    }

    pub fn engine(&self) -> &'static str {
        match self.0 {}
    }

    pub fn recovery(&self) -> Recovery {
        match self.0 {}
    }

    pub fn recover(&mut self, _kind: Recovery) {
        match self.0 {}
    }

    pub fn encode(&mut self, _picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        match self.0 {}
    }

    #[cfg(target_os = "linux")]
    pub fn encode_dmabuf(
        &mut self,
        _frame: &DmaBuf,
        _preview_width: Option<usize>,
    ) -> Result<GpuEncoded, EncodeError> {
        match self.0 {}
    }
}

/// No encoder here takes frames in GPU memory.
#[cfg(target_os = "linux")]
pub fn tries_gpu_input_first() -> bool {
    false
}

pub struct Decoder(Infallible);

impl Decoder {
    pub fn new(codec: Codec, _max_size: MaxSize) -> Result<Self, DecodeError> {
        Err(DecodeError::NoDecoder(codec))
    }

    pub fn recycle(&mut self, _frame: VideoFrame) {
        match self.0 {}
    }

    pub fn take_flushed(&mut self) -> bool {
        match self.0 {}
    }

    pub fn engine(&self) -> &'static str {
        match self.0 {}
    }

    pub fn decode(&mut self, _unit: &[u8], _timestamp: u32) -> Result<Option<Frame>, DecodeError> {
        match self.0 {}
    }
}
