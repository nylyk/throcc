//! Android: MediaCodec decodes straight into the `Surface` the app hands over, hardware
//! first; AV1 and VP9 fall back to dav1d and libvpx where MediaCodec can't take the share,
//! whose pictures are copied into the surface. Phones send no shares yet, so nothing
//! encodes here.

mod codec;
mod decode;
mod window;

pub use codec::CodecFrame;
pub use decode::Decoder;
pub use window::View;
