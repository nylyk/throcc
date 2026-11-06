# third_party

Crates we carry patches on, wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.

## gpui-pre, gpui-pre-apple, gpui-pre-wgpu, gpui-pre-windows 0.3.10

Apache-2.0, a crates.io republish of zed-industries/zed@08cbb88. Each directory started as an
exact copy of the published crate, so `git log -p third_party/` shows only our changes.

Changes, all for drawing received screen shares as NV12 instead of converting every frame to
BGRA on the UI thread:

- **gpui-pre:** `surface()` also takes an `Nv12Frame` (a `SurfaceId` per stream, a generation,
  the size and the Y and CbCr planes). `PaintSurface` carries it and the element's corner
  radii; on Apple platforms it carries a `SurfaceSource`, either a `CVPixelBuffer` or an
  `Nv12Frame`.
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
- **gpui-pre-apple:** `draw_surfaces` also draws `Nv12Frame`s: each stream keeps up to four
  slots of an R8 Y and an RG8 CbCr texture, and a new generation is written with
  `replace_region` into a slot no frame still on the GPU reads, so frames in flight never
  tear. Streams missing from a scene drop theirs. `SurfaceBounds` carries the corner radii,
  and `surface_fragment`, which `CVPixelBuffer`s share, uses full-range BT.709 and rounds
  corners with `quad_sdf`. The CoreVideo textures a frame reads from `CVPixelBuffer`s are
  held until the GPU finishes that frame, so a capture or decoder pool can't reuse a buffer
  still being drawn.

The BGRA image path these replace cost 37 ms of UI-thread CPU per 2560×1440 frame, more than
a 60 fps frame lasts; the NV12 surface costs 2 ms. On an Apple GPU, a release build took 5 ms
for the conversion alone, and takes 0.55 ms to upload and draw the NV12 surface.

## Refreshing on a gpui update

1. Bump the pins, `cargo fetch`, and copy the unmodified `gpui-pre*-<version>` crates from
   `~/.cargo/registry/src/*/` over the four directories. Commit that alone.
2. Re-apply our commits on top (`git log -p -- third_party/` lists them; `git cherry-pick`
   usually applies) and update this file.
3. If upstream has gained an NV12 or YUV surface on Linux and Windows, drop the vendored copies
   and the patch section instead.
4. Check that `Cargo.lock` lists the four without a `source`, and that
   `crates/throcc-client/tests/surface.rs` and `tests/surface/run.sh` still pass.
