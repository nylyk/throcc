# third_party

Crates we carry patches on, wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.

## gpui-pre, gpui-pre-apple, gpui-pre-wgpu, gpui-pre-windows 0.3.10

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

The BGRA image path these replace cost 37 ms of UI-thread CPU per 2560×1440 frame, more than
a 60 fps frame lasts; the NV12 surface costs 2 ms. macOS keeps the BGRA path, because its Metal
renderer lives in `gpui-pre-apple`, which is not vendored.

## Refreshing on a gpui update

1. Bump the pins, `cargo fetch`, and copy the unmodified `gpui-pre*-<version>` crates from
   `~/.cargo/registry/src/*/` over the four directories. Commit that alone.
2. Re-apply our commits on top (`git log -p -- third_party/` lists them; `git cherry-pick`
   usually applies) and update this file.
3. If upstream has gained an NV12 or YUV surface on Linux and Windows, drop the vendored copies
   and the patch section instead.
4. Check that `Cargo.lock` lists the four without a `source`, and that
   `crates/throcc-client/tests/surface.rs` and `tests/surface/run.sh` still pass.
