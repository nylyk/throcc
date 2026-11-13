# throcc-video

The video codecs and the pictures between them: one `Encoder` and one `Decoder` with a backend per platform, and the NV12 frames they take and give. It knows codecs and frames, nothing of sessions, keys or the wire beyond `Codec`. `throcc-client-core` always depends on it; the server never does, which `check-server-deps.sh` enforces.

A build with no backend still has both types. They never open: `Encoder::open` fails with `NoEncoder`, `Decoder::new` with `NoDecoder`, and `HAS_BACKEND` is false, so the client does voice only without a `cfg` of its own.

| Module | Holds |
| --- | --- |
| `lib.rs` | `Frame` as the renderer gets it, the encoder's config and its access units, `MaxSize`, the errors, and which frames in GPU memory an encoder refuses after one fails |
| `output.rs` | `Output`: where a decoder shows its pictures itself on the phones. The app hands over a `View` (an Android `Surface`'s window, an iOS `AVSampleBufferDisplayLayer`) at any time, before or while the share is watched, and takes it away again; the decoder follows. Desktops draw from `Picture` and leave it empty |
| `frame.rs` | NV12 pictures in memory: test patterns, shrinking, conversion from 4-byte RGB and to BGRA, copying planes with strides, and the sizes a share is fitted to |
| `dmabuf.rs` | Linux: a captured picture still in GPU memory, as DRM PRIME planes |
| `nal.rs` | H.264 and HEVC access units between Annex B, as senders send them with the parameter sets before each keyframe, and VideoToolbox's 4-byte length-prefixed NAL units with the parameter sets in a format description |
| `apple/` | macOS and iOS, whatever the features: VideoToolbox decodes H.264 and HEVC into full-range NV12 pixel buffers renderers draw uncopied (`pixel_buffer.rs`), falling back to Apple's own software decoders where the hardware lacks them, and AV1 too where the hardware decodes it, with a format description built from the sequence header (`av1.rs`), and encodes hardware HEVC, then H.264, from captured pixel buffers, flushing each frame before the next. Pictures of another size are scaled into the encoder's on the GPU with black bars (`transfer.rs`). Other AV1, and VP9, decode in dav1d and libvpx, into pixel buffers too, and macOS encodes VP9 in libvpx where it has no hardware encoder. On iOS each picture due goes into the app's display layer (`view.rs`), shown at once |
| `android/` | MediaCodec decodes straight into the app's surface, hardware first, from the next keyframe whenever the surface changes and it can't move to the new one. A decoded picture stays in the codec as a `Frame::Codec` until shown or dropped, and at most three are out at once, so pictures held back for the share audio can't starve it. AV1 and VP9 that MediaCodec can't take, or that keep failing in it, decode in dav1d or libvpx and are copied into the surface as YV12. Nothing encodes on Android yet |
| `software/` | dav1d for AV1, with frame threads whose pictures `drain` brings out once units stop, and libvpx for VP9, linked statically on Apple and Android (built by `packaging/codecs/build.sh`), and anywhere with the `software` feature for their tests |
| `ffmpeg/` | Linux and Windows with the `ffmpeg` feature: hardware AV1, HEVC and H.264 through NVENC, Media Foundation and VA-API, then VP9 in libvpx; decoding on the hardware device where there is one, else in software with dav1d for AV1. A VA-API encoder takes DMA-BUFs and scales them on the GPU (`vaapi.rs`) |
| `none.rs` | The backend of a build without one |

Encoders hold to low latency everywhere: no B-frames, no lookahead, a VBV of about two frames, and recovery by NVENC's intra refresh where it runs, else by an IDR on request. Decoders refuse pictures larger than the share announced, and a hardware decoder that keeps failing for three seconds is swapped for software.

```sh
cargo test -p throcc-video --features ffmpeg     # Linux, Windows
cargo test -p throcc-video --features software   # dav1d and libvpx, from the system's
cargo test -p throcc-video                       # macOS, after packaging/codecs/build.sh and env.sh
```

Tests of an ffmpeg hardware encoder print `skipped:` where the hardware is missing. The VideoToolbox tests open Apple's software encoders, which CI's virtual Macs have, and fail rather than skip.
