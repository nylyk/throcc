//! Video encoders and decoders, and the NV12 pictures between them. One [`Encoder`] and one
//! [`Decoder`] with a backend per platform; a build without one has an encoder and a decoder
//! that never open.

#[cfg(target_os = "linux")]
mod dmabuf;
pub mod frame;
#[cfg(target_os = "macos")]
mod pixel_buffer;

#[cfg(feature = "ffmpeg")]
mod ffmpeg;
#[cfg(not(feature = "ffmpeg"))]
mod none;
#[cfg(software)]
pub mod software;

use bytes::Bytes;
use throcc_proto::messages::Codec;

#[cfg(target_os = "linux")]
pub use dmabuf::{DmaBuf, DmaBufPlane, MAX_PLANES};
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::decode::Decoder;
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::encode::Encoder;
#[cfg(all(feature = "ffmpeg", target_os = "linux"))]
pub use ffmpeg::encode::tries_gpu_input_first;
#[cfg(all(feature = "ffmpeg", target_os = "linux"))]
pub use ffmpeg::vaapi::VaError;
#[cfg(all(not(feature = "ffmpeg"), target_os = "linux"))]
pub use none::tries_gpu_input_first;
#[cfg(not(feature = "ffmpeg"))]
pub use none::{Decoder, Encoder};
#[cfg(target_os = "macos")]
pub use pixel_buffer::{FULL_RANGE_NV12, PixelBuffer};

/// Whether this build has a video backend at all. Without one, nothing encodes or decodes.
pub const HAS_BACKEND: bool = cfg!(feature = "ffmpeg");

/// A picture as handed to the renderer: NV12 in memory, or on macOS the pixel buffer in GPU
/// memory it was captured or decoded into.
pub enum Frame {
    Planes(frame::VideoFrame),
    #[cfg(target_os = "macos")]
    PixelBuffer(PixelBuffer),
}

impl Frame {
    pub fn timestamp(&self) -> u32 {
        match self {
            Frame::Planes(frame) => frame.timestamp,
            #[cfg(target_os = "macos")]
            Frame::PixelBuffer(buffer) => buffer.timestamp,
        }
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        match self {
            Frame::Planes(frame) => (frame.width, frame.height),
            #[cfg(target_os = "macos")]
            Frame::PixelBuffer(buffer) => (buffer.width(), buffer.height()),
        }
    }

    /// The planes, for a picture held in memory.
    pub fn planes(&self) -> Option<&frame::VideoFrame> {
        match self {
            Frame::Planes(frame) => Some(frame),
            #[cfg(target_os = "macos")]
            Frame::PixelBuffer(_) => None,
        }
    }

    /// Takes the planes out, for a picture held in memory.
    pub fn into_planes(self) -> Option<frame::VideoFrame> {
        match self {
            Frame::Planes(frame) => Some(frame),
            #[cfg(target_os = "macos")]
            Frame::PixelBuffer(_) => None,
        }
    }

    /// The picture in memory, copied out of GPU memory if it is there.
    pub fn copied_to_memory(&self) -> frame::VideoFrame {
        match self {
            Frame::Planes(frame) => frame.clone(),
            #[cfg(target_os = "macos")]
            Frame::PixelBuffer(buffer) => buffer.copied_to_memory(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub kbps: u32,
}

/// An encoded access unit.
#[derive(Clone, Debug)]
pub struct EncodedUnit {
    pub data: Bytes,
    pub keyframe: bool,
    pub timestamp: u32,
}

/// What encoding a frame in GPU memory gave.
#[cfg(target_os = "linux")]
pub struct GpuEncoded {
    pub units: Vec<EncodedUnit>,
    /// The preview asked for, or why there is none.
    pub preview: Option<Result<frame::VideoFrame, EncodeError>>,
}

/// The widest and tallest any decoded picture may be.
pub const MAX_SIDE: usize = 4096;

/// The largest picture a decoder hands on, each side at most [`MAX_SIDE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxSize {
    width: usize,
    height: usize,
}

impl MaxSize {
    pub const ANY: Self = Self {
        width: MAX_SIDE,
        height: MAX_SIDE,
    };

    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.min(MAX_SIDE),
            height: height.min(MAX_SIDE),
        }
    }

    pub fn pixels(self) -> usize {
        self.width * self.height
    }

    /// Whether a picture of `width`×`height` fits. Negative sides never do.
    pub fn fits(self, width: i64, height: i64) -> bool {
        let side = |v: i64| usize::try_from(v).unwrap_or(usize::MAX);
        side(width) <= self.width && side(height) <= self.height
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("no usable video encoder on this machine")]
    NoEncoder,
    #[cfg(feature = "ffmpeg")]
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] rsmpeg::error::RsmpegError),
    #[error("can't scale the picture to the encoder's size")]
    Scaler,
    #[error("the hardware encoder has no frame pool")]
    NoSurfaces,
    #[error("this encoder takes no frames in GPU memory")]
    NoGpuInput,
    #[error("the picture is {0}×{1}, not the encoder's size")]
    OtherSize(usize, usize),
    #[cfg(software)]
    #[error("libvpx: {0}")]
    Vpx(String),
    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[error("importing a frame in GPU memory: {0}")]
    Import(VaError),
    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[error("GPU video processing: {0}")]
    Gpu(VaError),
    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[error("the preview came back in pixel format {0}, which throcc doesn't present")]
    PreviewFormat(i32),
}

/// Frames in GPU memory a share stops taking after one of them failed.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Those with this modifier, which failed to import.
    Modifier(u64),
    /// All of them, since the encoder or the GPU can't process them.
    All,
}

#[cfg(target_os = "linux")]
impl EncodeError {
    /// What to refuse after a frame in GPU memory with `modifier` failed with this error.
    /// `None` for errors that may pass, which cost only that frame.
    pub fn refusal(&self, modifier: u64) -> Option<Refusal> {
        match self {
            #[cfg(feature = "ffmpeg")]
            EncodeError::Import(_) => Some(Refusal::Modifier(modifier)),
            #[cfg(feature = "ffmpeg")]
            EncodeError::Gpu(_) => Some(Refusal::All),
            EncodeError::NoGpuInput => Some(Refusal::All),
            _ => {
                let _ = modifier;
                None
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("no decoder for {0:?}")]
    NoDecoder(Codec),
    #[error("decoded frames in pixel format {0}, which throcc doesn't present")]
    Format(i32),
    #[error("a {0}×{1} picture is larger than this share may send")]
    TooLarge(i32, i32),
    #[cfg(feature = "ffmpeg")]
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] rsmpeg::error::RsmpegError),
    #[cfg(software)]
    #[error("dav1d failed with {0}")]
    Dav1d(i32),
    #[cfg(software)]
    #[error("libvpx: {0}")]
    Vpx(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_side_is_bounded_not_only_the_area() {
        let size = MaxSize::new(1920 + 64, 1080 + 64);
        assert!(size.fits(1920, 1080));
        assert!(!size.fits(65_536, 16));
        assert!(!size.fits(16, 4096));
        assert!(!size.fits(-2, 16));
        assert!(!MaxSize::ANY.fits(MAX_SIDE as i64 + 2, 2));
        assert_eq!(MaxSize::new(usize::MAX, 8), MaxSize::new(MAX_SIDE, 8));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_encoder_that_cant_take_frames_in_gpu_memory_refuses_all() {
        assert_eq!(EncodeError::NoGpuInput.refusal(7), Some(Refusal::All));
        assert_eq!(EncodeError::NoSurfaces.refusal(7), None);
    }

    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[test]
    fn a_layout_that_fails_to_import_is_refused_alone() {
        let import = EncodeError::Import(VaError::Format(0));
        assert_eq!(import.refusal(7), Some(Refusal::Modifier(7)));
    }

    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[test]
    fn a_gpu_that_cant_process_them_refuses_all() {
        let processing = EncodeError::Gpu(VaError::Call {
            call: "vaEndPicture",
            status: 1,
        });
        assert_eq!(processing.refusal(7), Some(Refusal::All));
    }

    #[cfg(all(feature = "ffmpeg", target_os = "linux"))]
    #[test]
    fn a_passing_error_costs_only_the_frame() {
        let send = EncodeError::Ffmpeg(rsmpeg::error::RsmpegError::SendFrameAgainError);
        assert_eq!(send.refusal(7), None);
    }
}
