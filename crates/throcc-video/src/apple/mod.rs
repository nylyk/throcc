//! VideoToolbox on macOS and iOS: H.264 and HEVC decoded into pixel buffers renderers draw
//! uncopied, and encoded in hardware from captured pixel buffers. AV1 decodes in dav1d and
//! VP9 in libvpx, into pixel buffers too; macOS encodes VP9 in libvpx where it has no
//! hardware encoder.

mod attributes;
mod decode;
mod encode;
mod pixel_buffer;
mod transfer;
#[cfg(target_os = "ios")]
mod view;

pub use decode::Decoder;
pub use encode::Encoder;
pub use pixel_buffer::{FULL_RANGE_NV12, PixelBuffer};
#[cfg(target_os = "ios")]
pub use view::View;

/// VideoToolbox's and CoreVideo's success.
const NO_ERR: i32 = 0;
/// The 90 kHz media clock, as CoreMedia counts time.
const TIMESCALE: i32 = throcc_proto::frame::VIDEO_CLOCK_HZ as i32;

#[cfg(test)]
mod tests;
