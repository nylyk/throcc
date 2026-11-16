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
- **The phones**: the room's cards sit centred both ways, a short last row too; only the watched share sits above them, as tall as 60% of the window; a sharer's card has Watch. The overlay is the same in the tile and fullscreen: who shares and what top left with a warning under it, how many watch top right, stop watching, picture in picture and fullscreen centred along the bottom; fullscreen adds the room and the call controls as a pill beside them. It hides after five seconds; a tap toggles it. Phones stay upright outside fullscreen, which turns with the phone and keeps the screen on. On the iPad the share tile is as tall as its picture, so it sits right above the cards.
- **Phone audio**: both phones play the room as media, Bluetooth included (A2DP), so the volume goes down to silence and Bluetooth headphones stay in full quality; the phone's mic is used, never a headset's. The core's own echo canceller runs on every platform; iOS uses plain remote I/O, not the voice-processing unit. On Android, closing the app from the recent apps or the Picture in Picture window disconnects.
- **Out of sight**: a share isn't decoded while the app is in the background, unless Picture in Picture shows it. Android's MediaCodec closes with its surface; the iOS app hides the views with `Core::set_views_hidden`, and the Apple decoder skips units without a view. Coming back asks for a keyframe.
- **App icons**: iOS has an asset catalog with the icon as a full square, rendered from `packaging/icons/hicolor/scalable`; Android has an adaptive icon with a monochrome layer, drawn from the same SVG at 64 dp so the eyes fit a round mask.

## Verified

On an M4 Mac, a Pixel 8 (GrapheneOS), a Galaxy Tab S6 Lite (LineageOS, no AV1 hardware), an iPhone 15 Pro through TestFlight, an iPhone 12 mini by cable, and the iPhone 17 and iPad Air simulators, against a real server:
- Watching, switching shares, fullscreen in and out, turning, the lock screen with voice running, picture in picture on Android.
- AV1 in VideoToolbox on the M4 (the test stream and a live 1440p60 share) and on the iPhone 15 Pro.
- Phone audio on the Pixel and the iPhone 12 mini: the volume buttons take the room to silence, Bluetooth headphones play over A2DP in full quality and come and go mid-call, and no echo is heard on the speaker. On Android, swiping the app away leaves the room and the server.
- On the tablet: 1080p60 AV1 smooth in dav1d, 1440p60 AV1 with some lag and the pill, 1440p60 HEVC in hardware.
- Still screens load at once from macOS and on the desktop.
- Picture in picture and backgrounding, the lock screen included, on the Pixel and the iPhone 12 mini. Watching AV1 on the 12 mini (dav1d) takes about 85% of a core in front and about 19% in the background, voice and the incoming stream.
- On both phones' samples: the pin-mismatch and identity-missing dialogs, the safety-number sheets, Mark as Verified, the volume sliders, the warning pills, a room of 29 in fullscreen, the overlay under the Ink and Paper themes.

## What is left

1. **Picture preparation**: a large photo as a profile picture, on a phone.
2. **Foldables**: on a foldable, open and closed, the window stays upright on the small screen and turns freely on the large one, and fullscreen still turns on both. Both apps draw the line at a shorter side of 600 dp or points and check it again when the screen size changes (`onConfigurationChanged` in `MainActivity`, `Turning.screenChanged` from `RootView`); the Android emulator has a foldable profile, iOS has no simulator for one.
3. **The macOS desktop by hand**: watch a Windows share, pop out, fullscreen; `sending_1440p60_takes_under_one_core` (ignored test in `throcc-client-core`) with moving content; HEVC low latency at a height that isn't a multiple of 16.
4. **A signed build**: `gh workflow run packaging.yml --ref feature/mobile-5`, then the dmg on a clean Mac without Homebrew ffmpeg, and the signed APK.
5. **VP9 on the tablet**, on its `OMX.Exynos.vp9.dec`: share with `THROCC_CODEC=vp9`.
6. **Windows still screens**: `repeat_timestamp` for the Windows capture.
7. **Before merging**: keep or drop `ci.yml` on pushes to `feature/**`; delete this file.

Smaller:
- On Android, a share in dav1d or libvpx (AV1 on the tablet, VP9 without a MediaCodec for it) keeps decoding in the background: the software path in `android/decode.rs` needs the same skip without a view as the Apple decoder.
- `crates/throcc-cli/tests/share.rs` calls `share::receive` with too few arguments; it builds only with `--features ffmpeg`, so CI misses it.
- `Shared.outputs` keeps a view set for a share never watched until the session ends.
- The Android manifest still asks for `MODIFY_AUDIO_SETTINGS`, which only the audio mode and communication device needed; nothing sets them now.
- On the iPad, `--screen room` and the other room samples stop at the server list with a spinner on Night Owls until it is tapped; the iPhone opens the room.
- Every TestFlight build asks the encryption questions again; `ITSAppUsesNonExemptEncryption` in the Info.plist would answer them, but it is a legal declaration for the human to make.
- iOS lists rebuild on a theme change (`.id(theme.line)` in `ThemedList`/`ThemedForm`) because separators once showed the default colour after switching themes; it was never reproduced, so watch for it.

Set aside on purpose: AV1 in several tiles on the sender, and a sender choosing a codec every viewer decodes in hardware (a protocol change).

## Working here

- Android builds limited to one ABI (`-Pandroid.injected.build.abi=arm64-v8a`) leave `outputs/apk` stale; install `build/intermediates/apk/debug/app-debug.apk` with `adb install -t`.
- The iOS simulator shows a tap's result about a second later, and often drops the first tap after a launch.
- Samples with a light theme: `--theme Paper` on iOS.
- An iPhone by cable: Developer Mode on, unlocked; build the core with `mobile/ios/build-core.sh device`, then outside the sandbox `xcodebuild -destination id=<udid> -allowProvisioningUpdates -allowProvisioningDeviceRegistration build` and `xcrun devicectl device install app` / `process launch` (`xcrun devicectl list devices` gives the id).
- A TestFlight build: raise `CURRENT_PROJECT_VERSION`, build the core with `mobile/ios/build-core.sh`, then outside the sandbox `xcodebuild archive -allowProvisioningUpdates` for `generic/platform=iOS` and `xcodebuild -exportArchive` with `method` `app-store-connect`, `destination` `upload` and `signingCertificate` set to the SHA-1 of the local Apple Distribution certificate (`security find-identity -v -p codesigning`). The cloud-managed distribution certificate can't be used: its remote signing writes the "ö" of the account holder's name decomposed into the designated requirement, which then never matches, and App Store Connect answers "Invalid Signature". This Mac is registered as the team's one device, which automatic signing needs for the archive.

## Where things are

- `crates/throcc-video/src/apple/`: `decode.rs`, `av1.rs`, `encode.rs`, `transfer.rs`, `pixel_buffer.rs`, `view.rs` (iOS), `tests.rs` (runs on macOS CI).
- `crates/throcc-video/src/android/`: `codec.rs` (MediaCodec), `decode.rs`, `window.rs` (the view, YV12 copies).
- `crates/throcc-video/src/software/`: `dav1d.rs`, `vpx.rs`; `switches.rs`.
- `crates/throcc-client-core/src/media/share.rs`: the sender loop and `receive`.
- `mobile/android/.../Share.kt`, `mobile/ios/throcc/ShareView.swift`, `PictureInPicture.swift`: the tile, the overlay, fullscreen and picture in picture.
- `mobile/android/.../Room.kt`, `mobile/ios/throcc/RoomView.swift`, `CenteredGrid.swift`: the room's centred cards.
- `mobile/android/.../VoiceService.kt`, `crates/throcc-client-core/src/media/devices/android.rs`: the room's audio on Android.
- `packaging/codecs/`: the codec builds; `deps/codecs/<triple>` is where they land.
