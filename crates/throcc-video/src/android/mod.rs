//! Android: MediaCodec decodes straight into the `Surface` the app hands over, hardware
//! first; AV1 and VP9 fall back to dav1d and libvpx where MediaCodec can't take the share,
//! whose pictures are copied into the surface. MediaCodec encodes HEVC, then H.264, from an
//! input surface the screen is drawn into.

mod codec;
mod decode;
mod encode;
mod window;

pub use codec::CodecFrame;
pub use decode::Decoder;
pub use encode::Encoder;
pub use window::View;
