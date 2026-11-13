//! The backend of a build without one: an encoder and a decoder that never open.

use std::convert::Infallible;

use throcc_proto::messages::{Codec, Recovery};

use crate::frame::VideoFrame;
#[cfg(not(target_os = "android"))]
use crate::{DecodeError, Frame, MaxSize, Output};
#[cfg(target_os = "linux")]
use crate::{DmaBuf, GpuEncoded};
use crate::{EncodeError, EncodedUnit, EncoderConfig};

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

#[cfg(not(target_os = "android"))]
pub struct Decoder(Infallible);

#[cfg(not(target_os = "android"))]
impl Decoder {
    pub fn new(codec: Codec, _max_size: MaxSize, _output: Output) -> Result<Self, DecodeError> {
        Err(DecodeError::NoDecoder(codec))
    }

    pub fn present(&mut self, _frame: &Frame) {
        match self.0 {}
    }

    pub fn follow_view(&mut self) -> bool {
        match self.0 {}
    }

    pub fn recycle(&mut self, _frame: VideoFrame) {
        match self.0 {}
    }

    pub fn drain(&mut self) -> Result<Option<Frame>, DecodeError> {
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
