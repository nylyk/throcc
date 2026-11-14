# Phase 5 handoff

For the agent that finishes phase 5 of docs/mobile.md ("Watching shares, one at a time"). Branch `feature/mobile-5`, cut from `develop`. Delete this file before the branch merges.

Read CLAUDE.md, docs/mobile.md (Core changes 4 to 6, Codecs, macOS without ffmpeg, Phases) and the READMEs of `throcc-video`, `throcc-client-core` and `throcc-mobile` first.

## What the branch holds

- **`throcc-video`**, the codecs in their own crate: `Encoder`, `Decoder`, `Frame`, NV12 frames, `MaxSize`. A build without a backend has an encoder and decoder that never open (`none.rs`); `check-server-deps.sh` keeps the crate out of the server.
- **dav1d and libvpx** (`software/`), static on Apple and Android, built by `packaging/codecs/build.sh` (needs `meson`, `ninja`, `nasm`). dav1d decodes with frame threads; `Decoder::drain` brings out what is in flight once units stop.
- **VideoToolbox** (`apple/`): H.264, HEVC and, where the hardware has it, AV1 (`av1.rs`, the `av1C` from the sequence header) decode into IOSurface pixel buffers; dav1d takes AV1 elsewhere, libvpx VP9. Hardware HEVC, then H.264 encode, then VP9 in libvpx on macOS. macOS ships without ffmpeg.
- **Android** (`android/`): MediaCodec renders into the app's surface. AV1 and VP9 skip Android's own software decoders (`c2.android.*`, `OMX.google.*`) for dav1d and libvpx. A codec that worked and then fails is replaced at once; one that refuses the low-latency key is configured without it.
- **The decode loop** (`throcc-client-core/src/media/share.rs`): after lost references it skips to a keyframe and asks for one; a backlog of 30 units is dropped for the newest keyframe. The health refresh marks `lagging_shares` when that happens (the "Can’t keep up" pill on every client), beside `slow_shares` from the server ("Slow connection"). `CurrentRoom.showing` says which watched shares have a picture yet.
- **Still screens**: the sender encodes its last picture again after 500 ms without a new one, stamped by `Capture::repeat_timestamp`, so viewers get at least two pictures a second and an asked-for IDR comes with the next. Linux and macOS repeat; Windows has no `repeat_timestamp` yet.
- **Testing switches** (`switches.rs`): `THROCC_SOFTWARE_DECODE=1` and `THROCC_CODEC=av1|hevc|h264|vp9`; on Android `--ez software_decode true`, on iOS `--software-decode`.
- **The phones**: only the watched share sits above the room, as tall as 60% of the window; a sharer's card has Watch. The overlay is the same in the tile and fullscreen: who shares and what top left with a warning under it, how many watch top right, stop watching, picture in picture and fullscreen centred along the bottom; fullscreen adds the room and the call controls as a pill beside them. It hides after five seconds; a tap toggles it. Phones stay upright outside fullscreen, which turns with the phone and keeps the screen on.

## Verified

On an M4 Mac, a Pixel 8 (GrapheneOS), a Galaxy Tab S6 Lite (LineageOS, no AV1 hardware) and the iPhone 17 simulator, against a real server:
- Watching, switching shares, fullscreen in and out, turning, the lock screen with voice running, picture in picture on Android.
- AV1 in VideoToolbox on the M4: the test stream and a live 1440p60 share.
- On the tablet: 1080p60 AV1 smooth in dav1d, 1440p60 AV1 with some lag and the pill, 1440p60 HEVC in hardware.
- Still screens load at once from macOS and on the desktop.
- On both phones' samples: the pin-mismatch and identity-missing dialogs, the safety-number sheets, Mark as Verified, the volume sliders, the warning pills, a room of 29 in fullscreen, the overlay under the Ink and Paper themes.

## What is left

1. **A real iPhone.** Nothing ran on one yet. TestFlight to an iPhone 15 Pro would cover AV1 in VideoToolbox on an A17 Pro, picture in picture (the simulator has none, so the button hides there), and backgrounding. The team `58ZLB553RJ` is a paid account.
2. **Backgrounding on Android**: home screen mid-share for a while, then back.
3. **Picture preparation**: a large photo as a profile picture, on a phone.
4. **The macOS desktop by hand**: watch a Windows share, pop out, fullscreen; `sending_1440p60_takes_under_one_core` (ignored test in `throcc-client-core`) with moving content; HEVC low latency at a height that isn't a multiple of 16.
5. **A signed build**: `gh workflow run packaging.yml --ref feature/mobile-5`, then the dmg on a clean Mac without Homebrew ffmpeg, and the signed APK.
6. **VP9 on the tablet**, on its `OMX.Exynos.vp9.dec`: share with `THROCC_CODEC=vp9`.
7. **Windows still screens**: `repeat_timestamp` for the Windows capture.
8. **README screenshots**, once the phone UI is final.
9. **Before merging**: keep or drop `ci.yml` on pushes to `feature/**`; delete this file.

Smaller:
- `crates/throcc-cli/tests/share.rs` calls `share::receive` with too few arguments; it builds only with `--features ffmpeg`, so CI misses it.
- On iOS the fullscreen room strip starts a short last row on the left; Android centres it.
- `Shared.outputs` keeps a view set for a share never watched until the session ends.
- iOS lists rebuild on a theme change (`.id(theme.line)` in `ThemedList`/`ThemedForm`) because separators once showed the default colour after switching themes; it was never reproduced, so watch for it.

Set aside on purpose: AV1 in several tiles on the sender, and a sender choosing a codec every viewer decodes in hardware (a protocol change).

## Working here

- Android builds limited to one ABI (`-Pandroid.injected.build.abi=arm64-v8a`) leave `outputs/apk` stale; install `build/intermediates/apk/debug/app-debug.apk` with `adb install -t`.
- The iOS simulator shows a tap's result about a second later, and often drops the first tap after a launch.
- Samples with a light theme: `--theme Paper` on iOS.

## Where things are

- `crates/throcc-video/src/apple/`: `decode.rs`, `av1.rs`, `encode.rs`, `transfer.rs`, `pixel_buffer.rs`, `view.rs` (iOS), `tests.rs` (runs on macOS CI).
- `crates/throcc-video/src/android/`: `codec.rs` (MediaCodec), `decode.rs`, `window.rs` (the view, YV12 copies).
- `crates/throcc-video/src/software/`: `dav1d.rs`, `vpx.rs`; `switches.rs`.
- `crates/throcc-client-core/src/media/share.rs`: the sender loop and `receive`.
- `mobile/android/.../Share.kt`, `mobile/ios/throcc/ShareView.swift`, `PictureInPicture.swift`: the tile, the overlay, fullscreen and picture in picture.
- `packaging/codecs/`: the codec builds; `deps/codecs/<triple>` is where they land.
