//! Video encoders and decoders, and the NV12 pictures between them. One [`Encoder`] and one
//! [`Decoder`] with a backend per platform: VideoToolbox on macOS and iOS, ffmpeg on Linux
//! and Windows with the `ffmpeg` feature. A build without one has an encoder and a decoder
//! that never open.

#[cfg(target_os = "linux")]
mod dmabuf;
pub mod frame;
#[cfg(any(target_vendor = "apple", test))]
mod nal;
#[cfg(software)]
pub mod software;

mod output;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(ffmpeg)]
mod ffmpeg;
#[cfg(not(any(target_vendor = "apple", ffmpeg)))]
mod none;

use bytes::Bytes;
use throcc_proto::messages::Codec;

#[cfg(target_os = "android")]
pub use android::{CodecFrame, Decoder};
#[cfg(target_vendor = "apple")]
pub use apple::{Decoder, Encoder, FULL_RANGE_NV12, PixelBuffer};
#[cfg(target_os = "linux")]
pub use dmabuf::{DmaBuf, DmaBufPlane, MAX_PLANES};
#[cfg(ffmpeg)]
pub use ffmpeg::decode::Decoder;
#[cfg(ffmpeg)]
pub use ffmpeg::encode::Encoder;
#[cfg(all(ffmpeg, target_os = "linux"))]
pub use ffmpeg::encode::tries_gpu_input_first;
#[cfg(all(ffmpeg, target_os = "linux"))]
pub use ffmpeg::vaapi::VaError;
#[cfg(not(any(target_vendor = "apple", target_os = "android", ffmpeg)))]
pub use none::Decoder;
#[cfg(not(any(target_vendor = "apple", ffmpeg)))]
pub use none::Encoder;
#[cfg(all(not(ffmpeg), target_os = "linux"))]
pub use none::tries_gpu_input_first;
pub use output::{Output, View};

/// Whether this build encodes video, so it can share. Phones send no shares yet.
pub const ENCODES: bool = cfg!(any(target_vendor = "apple", ffmpeg)) && !cfg!(target_os = "ios");

/// A picture as handed to the renderer: NV12 in memory, on Apple platforms the pixel buffer
/// in GPU memory it was captured or decoded into, or on Android an output buffer of the codec
/// that shows it by rendering it into the app's view.
pub enum Frame {
    Planes(frame::VideoFrame),
    #[cfg(target_vendor = "apple")]
    PixelBuffer(PixelBuffer),
    #[cfg(target_os = "android")]
    Codec(CodecFrame),
}

impl Frame {
    pub fn timestamp(&self) -> u32 {
        match self {
            Frame::Planes(frame) => frame.timestamp,
            #[cfg(target_vendor = "apple")]
            Frame::PixelBuffer(buffer) => buffer.timestamp,
            #[cfg(target_os = "android")]
            Frame::Codec(frame) => frame.timestamp,
        }
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        match self {
            Frame::Planes(frame) => (frame.width, frame.height),
            #[cfg(target_vendor = "apple")]
            Frame::PixelBuffer(buffer) => (buffer.width(), buffer.height()),
            #[cfg(target_os = "android")]
            Frame::Codec(frame) => (frame.width, frame.height),
        }
    }

    /// Whether the picture can be shown again, in another view.
    pub fn shows_again(&self) -> bool {
        #[cfg(target_os = "android")]
        if let Frame::Codec(_) = self {
            return false;
        }
        true
    }

    /// The planes, for a picture held in memory.
    pub fn planes(&self) -> Option<&frame::VideoFrame> {
        match self {
            Frame::Planes(frame) => Some(frame),
            #[cfg(target_vendor = "apple")]
            Frame::PixelBuffer(_) => None,
            #[cfg(target_os = "android")]
            Frame::Codec(_) => None,
        }
    }

    /// Takes the planes out, for a picture held in memory.
    pub fn into_planes(self) -> Option<frame::VideoFrame> {
        match self {
            Frame::Planes(frame) => Some(frame),
            #[cfg(target_vendor = "apple")]
            Frame::PixelBuffer(_) => None,
            #[cfg(target_os = "android")]
            Frame::Codec(_) => None,
        }
    }

    /// The picture in memory, copied out of GPU memory if it is there. A picture still in
    /// Android's codec can't be read back, and comes out black.
    pub fn copied_to_memory(&self) -> frame::VideoFrame {
        match self {
            Frame::Planes(frame) => frame.clone(),
            #[cfg(target_vendor = "apple")]
            Frame::PixelBuffer(buffer) => buffer.copied_to_memory(),
            #[cfg(target_os = "android")]
            Frame::Codec(frame) => frame::VideoFrame::blank(frame.width, frame.height),
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

/// Room above each side a share announces for decoders that pad pictures to their alignment.
pub const DECODER_PADDING: usize = 64;

/// The largest picture a decoder hands on, each side at most [`MAX_SIDE`], and the size the
/// share announced, which a codec is set up for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxSize {
    width: usize,
    height: usize,
    announced: (usize, usize),
}

impl MaxSize {
    pub const ANY: Self = Self {
        width: MAX_SIDE,
        height: MAX_SIDE,
        announced: (MAX_SIDE, MAX_SIDE),
    };

    pub fn new(width: usize, height: usize) -> Self {
        let (width, height) = (width.min(MAX_SIDE), height.min(MAX_SIDE));
        Self {
            width,
            height,
            announced: (width, height),
        }
    }

    /// The bound for a share announced at `width`×`height`: that, plus
    /// [`DECODER_PADDING`] for decoders that pad pictures.
    pub fn for_share(width: usize, height: usize) -> Self {
        let padded = Self::new(
            width.saturating_add(DECODER_PADDING),
            height.saturating_add(DECODER_PADDING),
        );
        Self {
            announced: (width.clamp(16, MAX_SIDE), height.clamp(16, MAX_SIDE)),
            ..padded
        }
    }

    pub fn pixels(self) -> usize {
        self.width * self.height
    }

    /// The widest and the tallest a picture may be.
    pub fn sides(self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The size the share announced.
    pub fn announced(self) -> (usize, usize) {
        self.announced
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
    #[cfg(ffmpeg)]
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
    #[cfg(target_vendor = "apple")]
    #[error("VideoToolbox failed with {0}")]
    VideoToolbox(i32),
    #[cfg(all(ffmpeg, target_os = "linux"))]
    #[error("importing a frame in GPU memory: {0}")]
    Import(VaError),
    #[cfg(all(ffmpeg, target_os = "linux"))]
    #[error("GPU video processing: {0}")]
    Gpu(VaError),
    #[cfg(all(ffmpeg, target_os = "linux"))]
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
            #[cfg(ffmpeg)]
            EncodeError::Import(_) => Some(Refusal::Modifier(modifier)),
            #[cfg(ffmpeg)]
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
    #[cfg(ffmpeg)]
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] rsmpeg::error::RsmpegError),
    #[cfg(software)]
    #[error("dav1d failed with {0}")]
    Dav1d(i32),
    #[cfg(target_vendor = "apple")]
    #[error("VideoToolbox failed with {0}")]
    VideoToolbox(i32),
    #[error("waiting for a keyframe")]
    NoKeyframe,
    #[cfg(target_os = "android")]
    #[error("MediaCodec failed with {0}")]
    MediaCodec(i32),
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

    #[cfg(all(ffmpeg, target_os = "linux"))]
    #[test]
    fn a_layout_that_fails_to_import_is_refused_alone() {
        let import = EncodeError::Import(VaError::Format(0));
        assert_eq!(import.refusal(7), Some(Refusal::Modifier(7)));
    }

    #[cfg(all(ffmpeg, target_os = "linux"))]
    #[test]
    fn a_gpu_that_cant_process_them_refuses_all() {
        let processing = EncodeError::Gpu(VaError::Call {
            call: "vaEndPicture",
            status: 1,
        });
        assert_eq!(processing.refusal(7), Some(Refusal::All));
    }

    #[cfg(all(ffmpeg, target_os = "linux"))]
    #[test]
    fn a_passing_error_costs_only_the_frame() {
        let send = EncodeError::Ffmpeg(rsmpeg::error::RsmpegError::SendFrameAgainError);
        assert_eq!(send.refusal(7), None);
    }
}
