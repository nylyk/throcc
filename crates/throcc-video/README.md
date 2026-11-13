# throcc-video

The video codecs and the pictures between them: one `Encoder` and one `Decoder` with a backend per platform, and the NV12 frames they take and give. It knows codecs and frames, nothing of sessions, keys or the wire beyond `Codec`. `throcc-client-core` always depends on it; the server never does, which `check-server-deps.sh` enforces.

A build with no backend still has both types. They never open: `Encoder::open` fails with `NoEncoder`, `Decoder::new` with `NoDecoder`, and `HAS_BACKEND` is false, so the client does voice only without a `cfg` of its own.

| Module | Holds |
| --- | --- |
| `lib.rs` | `Frame` as the renderer gets it, the encoder's config and its access units, `MaxSize`, the errors, and which frames in GPU memory an encoder refuses after one fails |
| `frame.rs` | NV12 pictures in memory: test patterns, shrinking, conversion from 4-byte RGB and to BGRA, copying planes with strides, and the sizes a share is fitted to |
| `dmabuf.rs` | Linux: a captured picture still in GPU memory, as DRM PRIME planes |
| `pixel_buffer.rs` | macOS: a captured or decoded picture in a full-range NV12 CoreVideo pixel buffer |
| `ffmpeg/` | The `ffmpeg` feature's backend: hardware AV1, HEVC and H.264 through NVENC, Media Foundation, VA-API and VideoToolbox, then VP9 in libvpx; decoding on the hardware device where there is one, else in software with dav1d for AV1. On Linux a VA-API encoder takes DMA-BUFs and scales them on the GPU (`vaapi.rs`); on macOS VideoToolbox takes captured pixel buffers and decodes into them, uncopied |
| `none.rs` | The backend of a build without one |

Encoders hold to low latency everywhere: no B-frames, no lookahead, a VBV of about two frames, and recovery by NVENC's intra refresh where it runs, else by an IDR on request. Decoders refuse pictures larger than the share announced, and a hardware decoder that keeps failing for three seconds is swapped for software.

```sh
cargo test -p throcc-video --features ffmpeg
```

Tests of a hardware encoder print `skipped:` where the hardware is missing.
