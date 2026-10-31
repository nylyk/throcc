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

See `docs/DECISIONS.md` for why they are vendored and how to refresh them.
