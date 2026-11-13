# Phase 5 handoff

For the agent that finishes phase 5 of docs/mobile.md ("Watching shares, one at a time") on a Mac. Branch `feature/mobile-5`, cut from `develop`. Delete this file before the branch merges.

Read CLAUDE.md, docs/mobile.md (Core changes 4 to 6, Codecs, macOS without ffmpeg, Phases) and the READMEs of `throcc-video`, `throcc-client-core` and `throcc-mobile` first.

## What was done

Everything below was written on a Linux machine with no Mac, no device and no emulator. What CI or that machine verified is marked.

1. **`throcc-video`.** The codecs left `throcc-client-core` for their own crate: `Encoder`, `Decoder`, `Frame`, the NV12 frames, `MaxSize`, the errors. The ffmpeg backend moved in, then lost its macOS paths. The 22 `ffmpeg` gates in `session.rs` are gone: a build without a backend has an `Encoder` and `Decoder` that never open (`none.rs`), and `ENCODES` says whether this build can share. `check-server-deps.sh` bans the crate from the server. Verified: Linux and Windows CI, all tests.
2. **dav1d and libvpx** (`software/`) through `dav1d-sys` and `libvpx-native-sys`, linked statically on Apple and Android and built from source by `packaging/codecs/build.sh <root> <rust-target>...` (dav1d 1.5.4 with meson, libvpx 1.17.0, both pinned by commit). `packaging/codecs/env.sh` prints the env cargo needs: the `-sys` crates link by name, and each target gets `CARGO_TARGET_<TRIPLE>_RUSTFLAGS=-Lnative=<root>/<triple>/lib`, so one cargo run builds several targets. The `software` feature builds them anywhere from the system's libraries for their tests. rav1d was tried first and dropped: its crates.io package leaves out a header its arm64 assembly needs. Verified: Linux tests (AV1 from `testdata/pattern.ivf`, a VP9 round trip), Android builds both ABIs here, macOS CI.
3. **VideoToolbox** (`apple/`), macOS and iOS, through objc2. H.264 and HEVC decode synchronously into full-range NV12 IOSurface pixel buffers; the session is made from the first parameter sets and remade when ones it can't take arrive, or when iOS invalidates it. Encoding: hardware HEVC, then H.264, then on macOS VP9 in libvpx; each frame is flushed with `complete_frames` before the next; output goes back to Annex B with the parameter sets before each keyframe (`nal.rs`). Pictures of another size are letterboxed on the GPU with `VTPixelTransferSession` (`transfer.rs`), so a captured pixel buffer of any size goes to the encoder without a CPU copy. dav1d and libvpx pictures are uploaded into pooled pixel buffers. Verified: macOS CI runs `apple/tests.rs` against Apple's software encoders (the runners have no hardware one) and the tests fail rather than skip.
4. **macOS without ffmpeg.** `bundle.sh` builds the codecs and no longer bundles dylibs; CI's macOS job and `packaging.yml` dropped ffmpeg; the macOS notices are their own file (`notices.sh client-macos`) without FFmpeg's LGPL; `native.txt` split into `packaging/licenses/native/`. Verified: macOS CI builds and tests the whole workspace. Not verified: `packaging.yml` (it runs on pushes to `develop` touching packaging paths, or by hand) and a signed, notarized dmg.
5. **Android decode** (`android/`): NDK MediaCodec decodes straight into the app's `Surface`, configured at the size the share announced. Each decode waits up to 30 ms for its picture, since a sender sends nothing while its screen stands still. A picture stays in the codec as `Frame::Codec` until the core's decode thread shows it when it is due by the share audio (`Decoder::present`), or drops it; past three out at once the oldest is shown early. A new surface moves the codec with `setOutputSurface`; with no surface there is no codec, and the next one starts from a keyframe. AV1 or VP9 that MediaCodec refuses at configure, or that keeps failing for 3 s, go to dav1d or libvpx, copied into the surface as YV12; H.264 and HEVC get a new codec instead, tried at most every 3 s. Verified: compiles for both ABIs. Not run anywhere.
6. **iOS presenting** (`apple/view.rs`): each picture due is enqueued into the app's `AVSampleBufferDisplayLayer` through its `sampleBufferRenderer`, marked to display at once. Verified: type-checks for iOS. Not run anywhere.
7. **Views** (`output.rs`): the app hands a share's view to `Core::set_share_view(user, pointer)` at any time, and the core passes it to that share's decoder through an `Output`. The core's decode loop looks at the view every 10 ms while no units come, and shows the newest picture again in a new view (iOS, and Android's software path; a new Android codec needs a keyframe, which a sender only makes once its screen changes). Android: `ShareView.nativeWindow(surface)` (a JNI function in `throcc-mobile`) gives the window with a reference; iOS passes the layer with `Unmanaged.passRetained`. The core takes over the reference. `MockClient` shows its still test picture in the view, for the samples.
8. **Apps.** The room shows a tile per share above the people: Watch, or the share with Fullscreen and Stop Watching. Watching one share stops the one before. Fullscreen covers the window in the share's shape, turns with the phone (Android `configChanges`, so rotation keeps the surface), hides the system bars, and keeps everyone in the room along the bottom. Each app keeps a stack of views per share, so closing fullscreen gives the share back to the tile's view. Samples: `share` and `sharefull` on both. Verified: Android compiles; iOS compiles in CI's mobile job (`xcodebuild` for the simulator). Neither has run.

## CI

On `3a631d6` every job passed: lint, netem, surface, linux-video, mobile (android, through `compileDebugKotlin`), mobile (ios, through `build-core.sh simulator` and the `xcodebuild` of the app, the Swift's first compile). The test jobs on Linux, macOS and Windows passed on `4e38140`, and `3a631d6` only changed the Mac codec scripts.

## Review

Two reviewers (Fable 5.2) went over the branch: one the Rust, one the apps, CI and packaging. Every finding with a concrete failure was fixed in `eff4e4b` and `4e38140`: pictures that waited in MediaCodec for the next unit, held pictures dropped instead of shown, codecs that never reopened after a failure (Android H.264/HEVC, VideoToolbox after a failed session create or a malfunction), VP9 keyframes larger than announced decoded before refusal (libvpx is also built with `--size-limit=4096x4096` now), views that stayed black until the screen changed, the MediaCodec configure size, cut-short units, stale encoder outputs and lost IDRs on errors, a timestamp mix-up, the iOS build missing the Mac's codecs for `uniffi-bindgen`, shares announced with a degenerate shape crashing or stretching the tile, the fullscreen room strip overflowing past 8 people, fullscreen outliving the room on iOS, the README and scripts still asking macOS for ffmpeg, the codec cache shared across NDK versions, and VideoToolbox not linked into the iOS app.

Left as is, and worth a look:
- A brand-new Android codec on a still screen stays black until the sender's screen changes (see 7).
- `Shared.outputs` keeps a view for a share only until it is unwatched; a view set for a share never watched stays until the session ends.

## Shared client logic

After phase 5, logic the desktop, Android and iOS each had separately moved into `throcc-client-core` (new `install.rs`, `picture.rs`, `safety.rs`; additions in `state.rs`, `profile.rs`, `account.rs`, `theme.rs`) and `throcc-mobile`, with the desktop's behaviour where they differed:
- name, password and picture checks and their wording
- sign-in errors: phones now get a changed server key and a missing identity as dialogs
- the connection notice
- trust per person, "Verified as"
- the safety-number comparison (`SafetyCheck`): Android used to send whatever key the person presented
- profile picture preparation, and only PNG/JPEG of at most 512 px from others
- theme colours: Oklab mixes and one 4.5:1 readability rule, which now applies on the desktop too
- loading the stores without failing on a damaged file
- the verified, blocked and known-server lists
- the volume curve, the desktop's dB curve everywhere
- joining a locked room, the room we are live in
- the phones' one-share-at-a-time watching and their stack of share views (`Core::show_share`/`hide_share`)

The phones' Lobby lists are gone: the server never shows anyone outside a room.

A security review covered the safety-number move, and a Fable 5.2 review covered the whole move; their findings are fixed. CI compiles both apps (iOS green on `d3aab1d`).

Not run anywhere: the apps' new pin-mismatch and identity-missing dialogs (samples `pinmismatch`, `identitymissing` on both), the rebuilt safety-number sheets, the volume sliders, the slow-connection pill and the picture preparation. Check them with item 1 below.

## First runs on a Mac (2026-11-13)

On an M4 Mac with a Pixel 8 (GrapheneOS) and the iOS simulator; no iPhone, as the iPhone 12 needs a cable for its first pairing with Xcode.

- The codec builds need `meson`, `ninja` and `nasm` from Homebrew. libvpx for x86_64 Android takes the NDK's `llvm-readelf` (`5f4047b`).
- The `share` and `sharefull` samples render right on both. The black bar with a light block across the top of the test picture is its frame number.
- On the Pixel against a real server, watching a macOS desktop's HEVC share and another's: watch, stop, watch again, fullscreen in and out, turning, the lock screen with voice running. All work after these changes (`cc566f9`, `55cba56`):
  - Fullscreen kept no picture on Android: the tile's surface went before the fullscreen one came, so the codec closed and waited for a keyframe. The tile's surface stays under fullscreen now, and a codec moved to a new surface asks for a keyframe.
  - The core reports `CurrentRoom.showing`, the watched shares with a picture; the apps show a spinner until then.
  - Share tiles name the sharer; person cards are one height; phones stay upright outside fullscreen, which turns with the phone; the fullscreen overlay carries the call controls and the room, both hidden with it; on iOS a tap on the picture reaches the overlay; Android's room title is small and without a count.
- A debug APK went to a second tester and worked.

## What is left

In order:

1. **The rest of the app runs.** Switching between two live shares works on Android and in the iOS simulator, and the simulator ran against a real server. Not run yet: backgrounding the app and coming back, and the README screenshots. Also not run anywhere: the pin-mismatch and identity-missing dialogs (samples `pinmismatch`, `identitymissing` on both), the rebuilt safety-number sheets, the volume sliders, the slow-connection pill and the picture preparation.
2. **AV1 in VideoToolbox**: done in `apple/av1.rs`. On the M4 the test stream and a live 1440p60 share from a Linux desktop decode in VideoToolbox. Not yet run on an A17 Pro or newer iPhone.
3. **The desktop on macOS by hand.** Sharing in HEVC to a phone works. Left: watch a Linux or Windows share in AV1 and HEVC, pop out, fullscreen. `sending_1440p60_takes_under_one_core` (ignored test in `throcc-client-core`) with moving content. Check the HEVC low-latency mode on a height that isn't a multiple of 16.
4. **A signed build**: `gh workflow run packaging.yml --ref feature/mobile-5`, then open the dmg on a clean Mac (no Homebrew ffmpeg) and install the signed APK.
5. **Android software fallback**: done on the Galaxy Tab S6 Lite (SM-P610, LineageOS 13, Exynos 9611, no AV1 hardware). Its MediaCodec only had Android's software `c2.android.av1.decoder`, which fell behind and lost references; AV1 and VP9 now skip Android's own software decoders for dav1d and libvpx (`aa6cdc6`), which play 1080p60 AV1 smoothly there and 1440p60 AV1 with some lag and the Can’t keep up pill. Its hardware HEVC decoder refused the low-latency key; MediaCodec now configures again without it (`8279c5b`), and 1440p60 HEVC plays. VP9 on its `OMX.Exynos.vp9.dec` is still untested.
6. **Before merging**: `ci.yml` also runs on pushes to `feature/**` (added for this work); keep it or drop it. Delete this file.

Also open, from the tablet: senders could encode AV1 in several tiles (NVENC and VA-API take a tile layout), so dav1d spreads over more cores on devices without AV1 hardware. And a sender could pick a codec every viewer decodes in hardware, which needs a protocol change and is undecided.

Also open: a receiver that lost its references (a decoder replaced, a backlog dropped) skips units until a keyframe and asks for one, but the sender only encodes it with its next captured frame, and it sends nothing while its screen stands still. Until that screen changes the viewer keeps an old picture, or none in a new decoder; this is the black or wrongly sized tile seen after leaving fullscreen on a still share. Fix on the sender: when an IDR is asked for, encode the last frame again; the pixel-buffer and dmabuf capture paths would have to keep it.

Also seen: `crates/throcc-cli/tests/share.rs` calls `share::receive` with too few arguments since before these changes. It builds only with `--features ffmpeg`, so CI misses it.

## Where things are

- `crates/throcc-video/src/apple/`: `decode.rs`, `encode.rs`, `transfer.rs` (pools and letterboxing), `pixel_buffer.rs`, `view.rs` (iOS), `tests.rs` (runs on macOS CI).
- `crates/throcc-video/src/android/`: `codec.rs` (MediaCodec and the output buffer bookkeeping), `decode.rs`, `window.rs` (the view, YV12 copies).
- `crates/throcc-client-core/src/media/share.rs`: `receive` presents each picture due before it publishes it.
- `mobile/android/.../Share.kt`, `mobile/ios/throcc/ShareView.swift`: the tiles, fullscreen and the view stacks.
- `packaging/codecs/`: the codec builds. `deps/codecs/<triple>` is where they land; `mobile/ios/build-core.sh`, the Gradle `BuildCore` task, `just desktop` and `bundle.sh` build what's missing.
