# Mobile clients

Native throcc apps for iOS/iPadOS and Android. Each platform gets its own UI in its own language and framework, over one shared Rust core. The server, the protocol and the crypto stay as they are.

This lifts "mobile clients" out of the not-in-scope list in `CLAUDE.md`. Web clients stay out.

## Decisions

| | |
| --- | --- |
| Platforms | iOS and iPadOS 26+, Android 10+ (API 29). Target the current Android SDK |
| Form factors | Phones and tablets on both platforms in the first release |
| UI | SwiftUI on Apple platforms, Jetpack Compose with Material 3 on Android |
| Core | The existing Rust crates, exposed through a new `throcc-mobile` crate with UniFFI |
| Colour | System backgrounds, text and materials. Accent, good, warning and bad come from the throcc palette |
| Android distribution | F-Droid and a signed APK on GitHub releases. No Google Play Services anywhere |
| Order | Android first, then iOS |

### Open

- **Apple distribution.** App Store, TestFlight only, or self-build only. Anything past building it yourself needs a paid Apple Developer account. The app is built to pass App Review either way (see [App Review](#app-review)), so this can wait until the iOS phase.
- **Google Play.** Undecided. Nothing in the plan blocks it. It would add Play's foreground-service declarations and a review of the permissions.

## What stays the same

- **Protocol:** no changes to `throcc-proto`. A phone joins with the same messages as a desktop client.
- **Server:** no changes.
- **Transport:** raw QUIC over UDP 8476, the same ALPN, the same pinned Ed25519 server certificate. Both platforms allow plain UDP sockets.
- **Crypto:** `throcc-crypto` compiled for the device. The frozen parts are not reimplemented anywhere.
- **Codec choice:** codecs are announced by the sender and never negotiated. A mobile viewer must decode AV1, HEVC, H.264 and VP9. A mobile sender picks a codec its hardware encodes.

## Architecture

```
crates/
  throcc-mobile/      UniFFI surface: Client, MockClient, state records, platform callbacks
mobile/
  android/            Gradle project, Kotlin, Compose
  ios/                Xcode project, Swift, SwiftUI; the app, the broadcast extension, a Swift package for the bindings
```

### `throcc-mobile`

A separate crate, so `throcc-client-core` stays free of UniFFI and desktop builds never compile it.

- **Builds:** `cdylib` for Android and `staticlib` for iOS. UniFFI proc-macro mode, with `uniffi-bindgen` as a small bin in the crate.
- **Surface:**
  - A `Client` object with one method per `Command`.
  - State as plain records. A `StateListener` callback interface gets each new `State` and each `Event`.
  - `mic_level` and safety numbers as plain calls.
  - `MockClient` behind the same interface, so previews and screenshot tests run on the sample data the desktop uses.
- **What stays out of UniFFI:** audio samples and video frames never cross it. Audio runs inside Rust on the platform's real-time callback. Video moves as native handles: an `ANativeWindow`, or a `CVPixelBuffer` or `CMSampleBuffer`. These cross through a few hand-written `extern "C"` functions in the platform modules.
- **Runtime:** the tokio runtime is owned by the core, as on desktop. UniFFI calls are non-blocking and send to it.

### Changes to `throcc-client-core`

Each change is a seam that desktop keeps implementing as it does today. Desktop behaviour doesn't change.

1. **Directories.** The host passes the config, data and cache directories in at start. On mobile, `directories` is not used.
2. **Key storage.** A `KeyStorage` trait with the desktop's `keyring` implementation. The mobile implementation calls back into the app (see [Keys](#keys)).
3. **Audio I/O.** `media/devices.rs` gets one backend per platform behind a small trait:
   - **Android:** AAudio through the `ndk` crate, with usage `VOICE_COMMUNICATION` and input preset `VOICE_COMMUNICATION`.
   - **iOS:** the `VoiceProcessingIO` audio unit.

   Both use the platform's echo cancellation, so `sonora`'s echo canceller is off on mobile. Noise suppression stays a setting; on mobile it picks between the platform's suppressor and `sonora`'s. The callback rules in `CLAUDE.md` apply unchanged: no blocking, allocating, locking, awaiting or panicking.
4. **Video codecs.** `media/video/` gets an encoder and decoder trait. ffmpeg stays behind it on desktop. Mobile backends:
   - **Android:** `AMediaCodec` through the NDK.
     - Decode renders straight to the viewer's `Surface`.
     - Encode reads from an input `Surface` that MediaProjection draws into.
     - Software fallbacks come with the OS: `c2.android.av1` and `c2.android.vp9`.
   - **iOS:** VideoToolbox through `objc2-video-toolbox`, for HEVC and H.264 decode, and AV1 decode where the chip has it (A17 Pro, M3 and later). dav1d (BSD-2) and libvpx (BSD-3) decode in software where VideoToolbox can't: AV1 on older chips, and VP9.

   No ffmpeg on mobile. That avoids LGPL linking questions on iOS and MediaCodec's weak encode path through ffmpeg.
5. **Pre-encoded share input.** `media/share.rs` takes access units that were already encoded, along with recovery requests sent back to the encoder. The iOS broadcast extension needs this (see [Screen share on iOS](#screen-share-on-ios)).
6. **Palette.** `Palette`, the presets and the Oklab mixing move out of `throcc-client/theme.rs` into a gpui-free `throcc-client-core::palette`. `theme.rs` keeps everything gpui-specific and stays the only place desktop colours live. `CLAUDE.md` is updated to say so.

### State in the apps

- **iOS:** one `@Observable` model on the main actor, replaced from each `StateListener` call. Views read slices of it.
- **Android:** one `StateFlow<State>` in an application-scoped holder. Each screen's `ViewModel` maps it to UI state with `stateIn`.
- **Both:** commands go straight to `Client`. Nothing in the apps duplicates the core's logic or keeps a second source of truth.

## Platform integration

### Android

- **App structure:** one activity, Compose, and Navigation Compose with typed routes. Kotlin coroutines and `StateFlow`. AndroidX libraries only.
- **Calls:** being in a room is a call.
  - It's registered through Jetpack `core-telecom` (`CallsManager`), so Bluetooth headsets, car audio and wearables route and control it the way they do other VoIP calls.
  - A foreground service runs while in a room, with type `phoneCall|microphone`. `mediaProjection` is added while sharing.
  - Its notification uses `NotificationCompat.CallStyle`, with mute and leave actions.
- **Audio routing:**
  - On API 31 and up, `AudioManager.setCommunicationDevice` with a route picker in the room toolbar.
  - Below API 31, speakerphone and Bluetooth SCO.
- **Network changes:** `ConnectivityManager.NetworkCallback` tells the core to rebind its QUIC socket, which migrates the connection.
- **Permissions:** asked only when needed, each with a short reason first:
  - `RECORD_AUDIO` on first join.
  - `POST_NOTIFICATIONS` (API 33+) on first join.
  - MediaProjection consent on each share.
- **Edge-to-edge and back:** edge-to-edge everywhere, and predictive back.
- **Picture in picture:** for a watched share. `setAutoEnterEnabled` on API 31+, `enterPictureInPictureMode` on leave below that.
- **Backup:** the key files are excluded from cloud and device-to-device backup (`dataExtractionRules` and `fullBackupContent`). An identity never leaves the device it was made on.

### iOS and iPadOS

- **App structure:** SwiftUI app lifecycle, Swift 6 strict concurrency.
- **CallKit:** being in a room is a CallKit call, with outgoing calls only and `includesCallsInRecents = false`. This gives:
  - the system's handling of interruptions (a phone call, Siri), Bluetooth and car routing, and lock-screen controls;
  - the green mic indicator tied to a named call.
- **Audio session:** `AVAudioSession` category `playAndRecord`, mode `voiceChat`. It's set up when the call starts and given back when the call ends. Route changes and interruptions go to the core as device changes.
- **Background:** `UIBackgroundModes` `audio` and `voip`.
  - In a room, the active audio session keeps the app running.
  - In the lobby, the app disconnects on entering the background and reconnects on return. The lobby has nothing to keep alive.
- **Network changes:** `NWPathMonitor` tells the core to rebind on path changes.
- **Local network:** `NSLocalNetworkUsageDescription`, since self-hosted servers are often on the LAN. iOS asks the first time the app reaches a local address.
- **Picture in picture:** `AVPictureInPictureController` with an `AVSampleBufferDisplayLayer` content source, for a watched share.
- **Live Activity:** a Live Activity and Dynamic Island showing the room, the person speaking and a mute button, through an App Intent. This comes after the first iOS release.

### Keys

- **iOS:** the identity key bytes go in the Keychain with `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`. That makes them readable for a background reconnect and keeps them out of iCloud Keychain and backups.
- **Android:** Android Keystore only gets Ed25519 and X25519 on API 33, so the keys can't live there on API 29.
  - The key files are sealed with an AES-256-GCM key generated in Android Keystore, which uses StrongBox when the device has it.
  - The sealed files live in app-private storage.

### Screen share on Android

- **Capture:** MediaProjection's `VirtualDisplay` draws into the encoder's input `Surface`. Nothing is copied on the CPU.
- **Audio:** system audio comes from `AudioPlaybackCaptureConfiguration` through `AudioRecord` into the share-audio ring. Apps that opt out of capture are silent, and the share dialog says so.
- **Codec:**
  - HEVC if the device encodes it in hardware, else H.264.
  - AV1 where hardware encode exists and the device passes a quick self-test at start.
  - `KEY_LOW_LATENCY` on API 30+, CBR, and a sync frame on each recovery request.
- **Rotation:** turning the device changes the source size mid-share. The share restarts its encoder at the new size with a sync frame, as desktop does when a window resizes.

### Screen share on iOS

Sharing the screen is a ReplayKit broadcast upload extension, started from `RPSystemBroadcastPickerView` in the share dialog.

- **The extension stays small:**
  - It holds no throcc keys and opens no connection.
  - It encodes with a VideoToolbox `VTCompressionSession` (HEVC, else H.264, real-time, CBR). It sends the access units and the app-audio PCM to the main app over a Unix domain socket in the shared app group container.
  - Jitsi and other iOS conferencing apps use this pattern.
- **The main app does the rest:** it is kept alive by its call. It receives the access units through the pre-encoded share input, then fragments, encrypts and sends them. Recovery requests go back over the same socket.
- **Size and rate:**
  - Frames come at native screen size in portrait. They're scaled with `VTPixelTransferSession` to the share size the user picked, up to 1080p60.
  - The default is 720p30. The rate follows what ReplayKit delivers.
- **Memory:** the extension is kept well under the roughly 50 MB limit with a small encoder pool and no frame copies.

## UI

The goal is that someone who knows the desktop client finds the same places, names and states on a phone. The controls come from the platform.

### Shared across both apps

- **Same structure as desktop:**
  - servers, then a server's lobby of rooms with their members, then a room of tiles, plus settings and the same dialogs;
  - the same words: Lobby, Join, Watch, Mark as verified, Safety number.
- **Same states, same colours:**
  - speaking is a ring in *good*;
  - leave and destructive actions are *bad*;
  - key changes and unverified warnings are *warning*;
  - the primary action is the *accent*.
- **Accent contrast:** the accent is taken from the palette's preset. If it falls below 3:1 against the system background in the current appearance, it's adjusted in Oklab lightness. A dark preset's accent stays usable in light mode.
- **Type:**
  - The system font for interface text, so Dynamic Type and Android font scaling work everywhere.
  - Geist Mono, as on desktop, for addresses, invite codes, fingerprints, safety numbers, bitrates, latency and codec labels. It scales with the text size: `Font.custom(_:size:relativeTo:)` on iOS, `sp` units on Android.
- **Icons:** SF Symbols on iOS and Material Symbols on Android, with the same metaphors as desktop's Lucide set. Custom symbols only where the platform has none.
- **Mascot:** the desktop's mascot drawings, exported as vectors, appear in empty states: no servers, an empty lobby, alone in a room.
- **Quiet:** one accent, no gradients, no decorative shadows, no emoji. The same tone as desktop, restated for each platform below.
- **Accessibility:**
  - VoiceOver and TalkBack labels on every icon button.
  - A person's state (muted, deafened, sharing, verified) is read as part of their row.
  - Hit targets of at least 44 pt / 48 dp.
  - Reduce Motion, Increase Contrast and Bold Text are honoured.
- **Settings:** the same sections as desktop: Profile, Audio, Appearance, Servers.
  - Audio has voice activation sensitivity, noise suppression and the mic test. Mobile has no device lists; the route picker covers that.
  - Appearance picks a preset. Editing a custom palette stays desktop-only for now; a custom palette chosen on desktop doesn't sync.
- **Previews:** every screen and dialog has a preview on mock data, the mobile counterpart of `--mock --screen`. The preview names match the desktop sample names where a screen has a desktop twin: `welcome`, `join`, `joinfailed`, `lost`, `personmenu`, `addserver`, `removeserver`, `room`, `alone`, `emptylobby`, `focus`, `fullscreen`.

### iOS and iPadOS

Follows the Human Interface Guidelines and the iOS 26 design language.

- **Layout:**
  - On iPhone, a `NavigationStack`.
  - On iPad, and on iPhone in a regular width class, a `NavigationSplitView`. Its sidebar is the desktop sidebar: rooms with their members nested. Its detail is the lobby or the room.
- **Servers:** the root is the server list, with Add server. It looks like the desktop's welcome card, but as an inset grouped `List`. In a server, the navigation title is the server's name with a `toolbarTitleMenu` to switch servers, the same place as desktop's server dropdown.
- **Lobby:**
  - An inset grouped `List`, one section per room.
  - The section header carries the room name, member count, a lock for password rooms and a share indicator.
  - Rows show members with avatar and trailing mute, deafen and share symbols.
  - Tapping a room opens it with a prominent Join button. Swiping a room row offers Join directly. Admins get Rename and Delete in the row's context menu.
- **Room:**
  - An adaptive `LazyVGrid` of tiles, like the desktop grid: avatar tiles with a speaking ring, and share tiles.
  - A share tile shows Watch until watched. Once watched, it shows the picture and a Geist Mono chip with name, resolution and codec, as on desktop.
  - Tapping a watched share opens it full screen with pinch to zoom, landscape and PiP.
- **Controls:**
  - The room's controls sit in the bottom toolbar, which floats in Liquid Glass: mic, deafen, audio route (`AVRoutePickerView`), share, and leave in *bad*.
  - Outside the room screen while in a call, a compact glass bar sits above the bottom safe area. It shows the room, your status, mic and deafen, and is the counterpart of desktop's bottom-left self bar. Tapping it returns to the room.
  - Liquid Glass is used only for these floating controls and the system bars, never for content.
- **Person menu:**
  - A long press on a person opens a context menu with Volume, Verify, and for admins Kick and Change role.
  - Volume opens a sheet with `.medium` detent holding their voice and share volume sliders, replacing desktop's right-click menu.
- **Dialogs:** sheets with detents.
  - Safety number: the desktop grid of twelve five-digit groups in Geist Mono, the same explanation, and Mark as verified as the prominent button.
  - Destructive confirmations (Leave server, Remove user, Delete room) use `confirmationDialog`.
- **Join a server:**
  - A form with Address and Invite code, both Geist Mono. Address uses the URL keyboard, invite code the ASCII keyboard, and neither autocorrects or autocapitalises.
  - Then a confirmation step showing the server's fingerprint in Geist Mono, with the instruction to compare it to the server log.
- **Key changes:** the desktop key-change banner becomes an inline banner at the top of the lobby in *warning*. It opens the same explanation as a sheet.
- **Members and invites:**
  - A `List` with roles.
  - Create invite produces the code in Geist Mono with a `ShareLink`, plus a copy button that sets the pasteboard with a one-day expiration.
- **Haptics:** `sensoryFeedback` on mute, deafen, join and leave only.

### Android

Follows Material 3 and the Android core app quality guidelines.

- **Layout:** `NavigableListDetailPaneScaffold` from Material 3 adaptive.
  - On phones, it's list then detail.
  - On tablets, foldables and wide windows, it's two panes, like the desktop sidebar and main area.
  - Window size classes drive the grid's columns.
- **Colour:**
  - Dynamic colour is off.
  - The scheme keeps the Material 3 baseline neutral surfaces. `primary` and `primaryContainer` come from the accent, with the container mixed in Oklab by the core. `error` comes from *bad*.
  - Light and dark follow the system.
- **Servers:** the start destination is the server list with an Add server FAB. In a server, the top app bar title opens a dropdown menu of servers.
- **Lobby:** a `LazyColumn` of rooms.
  - Each room is a header item (name, count, lock, share indicator) followed by its members as list items with trailing state icons.
  - Tapping a room opens it with a filled Join button.
- **Room:** a `LazyVerticalGrid` of the same tiles as iOS and desktop.
- **Controls:**
  - The room's controls are a floating toolbar at the bottom: mic, deafen, audio route, share, and leave in the error colour.
  - Outside the room while in a call, a persistent bottom bar does what the iOS glass bar does.
- **Person menu:** a long press opens a `ModalBottomSheet` with volume sliders, Verify and admin actions.
- **Dialogs and confirmations:** Material 3 `AlertDialog` for destructive confirmations. Bottom sheets for safety numbers and the fingerprint step.
- **Snackbars:** for transient events: invite copied, reconnecting, reconnected.
- **Haptics:** `HapticFeedbackConstants` on mute, deafen, join and leave only.

## Build and CI

- **Android:**
  - Gradle runs `cargo ndk` for `arm64-v8a`, `armeabi-v7a` and `x86_64`, then `uniffi-bindgen` for Kotlin. The NDK version is pinned in `build.gradle.kts`.
  - CI builds a debug APK on every push and runs unit and Compose tests on an emulator.
  - Tags and the nightly build a release APK signed with a key from repository secrets, and attach it to the release as the desktop packages are.
- **iOS:**
  - A script builds `aarch64-apple-ios` and `aarch64-apple-ios-sim` and packs an XCFramework. It runs `uniffi-bindgen` for Swift into a local Swift package the Xcode project depends on.
  - CI builds and runs the unit and UI tests on a macOS runner on every push.
- **F-Droid:**
  - Fastlane metadata under `mobile/android/fastlane/metadata/android/`.
  - Builds come from source with the pinned Rust toolchain.
  - Builds are reproducible, so F-Droid can ship the APK under our signature and the GitHub APK and the F-Droid APK update each other.
  - No Google Play Services, Firebase or ML Kit in the dependency tree, checked in CI.
- **`cargo deny`:** covers `throcc-mobile`.
  - New Rust dependencies: `uniffi` (MPL-2.0), `ndk` (MIT/Apache-2.0), `objc2-*` (MIT), and bindings to dav1d and libvpx (BSD). All are on the allow list.
  - The Gradle and Swift package dependencies get the same no-GPL check through a licence report in CI.
- **Unchanged checks:** `check-server-deps.sh` is untouched. Nothing here reaches the server.

## Testing

- **Core:** unit tests run on the host as today, including the new seams with fake backends.
- **Bindings:** `throcc-mobile` gets UniFFI smoke tests in Kotlin and Swift. They connect a `MockClient`, read state and send commands.
- **Screenshots:**
  - Android: Compose previews plus Roborazzi.
  - iOS: Xcode previews plus snapshot tests.
  - Both in light and dark, on phone and tablet sizes, from mock data.
- **Interop:** a device or emulator joins a room driven by `throcc-cli`.
  - Voice in both directions.
  - Watching each of AV1, HEVC, H.264 and VP9 from a desktop sender.
  - A mobile share watched on desktop.
- **Manual matrix each release:**
  - a low-end Android phone, a recent Pixel, a Samsung;
  - an iPhone without AV1 decode, a recent iPhone Pro, an iPad.
  - Covers Bluetooth headsets, a phone call interrupting a room, switching between Wi-Fi and cellular, and locking the screen in a room.

## Phases

Each phase ends with something usable and committed.

1. **Core seams.**
   - Directories, `KeyStorage`, the audio and codec traits, pre-encoded share input, the palette moved to core.
   - Desktop unchanged and green.
2. **`throcc-mobile` and builds.**
   - The UniFFI surface over `Client` and `MockClient`.
   - Android and iOS build scripts, CI for both, and bindings smoke tests.
3. **Android voice.**
   - Servers, join, lobby, room tiles, the person sheet, verify, members and invites, settings.
   - Telecom, the foreground service, routing, network changes.
   - Phone and tablet layouts.
4. **Android watching.** MediaCodec decode to `Surface`, full screen, PiP.
5. **Android sharing.** MediaProjection, playback capture, MediaCodec encode.
6. **Android release.** F-Droid metadata, reproducible build, the signed APK in GitHub releases.
7. **iOS voice.**
   - The same screens as phase 3 on iPhone and iPad.
   - CallKit, the audio session, Keychain, network changes.
8. **iOS watching.** VideoToolbox, dav1d and libvpx fallbacks, PiP.
9. **iOS sharing.** The broadcast extension, the socket to the app, 1080p60 as an opt-in.
10. **iOS release and extras.** Distribution per the open decision. Live Activity and Dynamic Island.

## Risks

- **VP9 on iPhone.** Desktop falls back to VP9 in software when it has no hardware encoder. libvpx can't decode 1440p60 VP9 in real time on most iPhones. Such shares may play at a lower frame rate on iOS. Fixing this properly means viewers telling senders what they decode. That's a protocol change, and it needs a human decision.
- **ReplayKit frame delivery.** Frames come only when the screen changes, and the rate and memory headroom differ by device. Measure with a frame-counting extension before the iOS sharing phase.
- **MediaCodec differences between vendors.** Bitrate modes, low-latency flags and AV1 encoders behave differently from vendor to vendor. Keep an allow list of tested encoders, and fall back to H.264.
- **System audio on Android.** Apps can opt out of playback capture, so some shares will have no sound.
- **Background limits.** OEM battery savers can still kill a foreground service. Link to the system battery settings from the in-call notification when the app sees it was killed.

## App Review

These matter if the app goes to the App Store or TestFlight:

- **CallKit:** used for real calls. It's not available in the China storefront, so the app either leaves that storefront out or falls back to a plain audio session there.
- **Background audio and VoIP:** used only while in a room.
- **Encryption:** standard algorithms only. The export compliance answer is set in `Info.plist` (`ITSAppUsesNonExemptEncryption`), after checking whether end-to-end encryption needs the annual self-classification report.
- **Local network:** the usage string explains self-hosted servers.
- **Review account:** App Review needs a server to join. That means a demo server and invite, or a demo mode on `MockClient`.
