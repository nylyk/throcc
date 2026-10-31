# third_party

Crates we carry patches on, wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.

## gpui-pre, gpui-pre-wgpu, gpui-pre-windows 0.3.10

Apache-2.0, a crates.io republish of zed-industries/zed@08cbb88. Each directory started as an
exact copy of the published crate, so `git log -p third_party/` shows only our changes.

Changes, all for drawing received screen shares as NV12 on Linux and Windows instead of
converting every frame to BGRA on the UI thread:

- **gpui-pre:** `surface()` also takes an `Nv12Frame` (a `SurfaceId` per stream, a generation,
  the size and the Y and CbCr planes) off Apple platforms. `PaintSurface` carries it and the
  element's corner radii. The macOS `CVPixelBuffer` path is unchanged apart from the radii,
  which the Metal renderer does not read.
- **gpui-pre-wgpu:** draws the `Surfaces` batch, which it skipped. Each stream keeps an R8 Y
  and an RG8 CbCr texture, recreated only when its size changes and written only when the
  frame's generation changes; streams missing from a scene drop theirs. The surface parameters
  sit in one uniform buffer behind a dynamic offset, so a stream's bind group is built once.
  `fs_surface` uses full-range BT.709, which throcc captures and encodes, instead of BT.601,
  and rounds corners with the quads' `quad_sdf`.
- **gpui-pre-windows:** `draw_surfaces`, an empty stub, draws each surface with a new
  `surface_vertex`/`surface_fragment` pair in `shaders.hlsl` (registered in `build.rs` for the
  release-mode fxc build): an R8 Y and an R8G8 CbCr texture per stream, written with
  `UpdateSubresource` only for a new generation, sampled through a clamping sampler, with the
  same content-mask clipping and `quad_sdf` rounding as polychrome sprites. A lost device drops
  the textures.

See `docs/DECISIONS.md` for why they are vendored and how to refresh them.
