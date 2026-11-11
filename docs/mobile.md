# Mobile apps

Native iOS and Android clients on top of `throcc-client-core`. Same servers, same end-to-end encryption.

## Decisions

| Topic | Decision |
| --- | --- |
| Core binding | A new crate, `throcc-mobile`, exposes the core through UniFFI. The core gains no UniFFI types |
| Frontends | iOS in Swift and SwiftUI. Android in Kotlin and Jetpack Compose with Material 3 Expressive |
| Minimum OS | iOS 26 (Liquid Glass everywhere). Android 12, API 31 (about 78% of devices in Oct 2026) |
| Look | Native components, system font, system mono for keys, fingerprints, safety numbers and invite codes. System colours, except the accent |
| Accent | Pickable: the desktop presets' accents plus a custom colour. The default matches the desktop default |
| Features | Voice, rooms, watching shares and sending shares on both platforms. Room admin, profiles and verification as on desktop |
| Talking | Voice activity with a mute toggle. No push-to-talk |
| Echo | The platform's voice-processing I/O: VoiceProcessingIO on iOS, `VOICE_COMMUNICATION` on Android. sonora's echo canceller is skipped there |
| Codecs | Platform decoders for all four codecs, with dav1d and libvpx filling the gaps. No ffmpeg, no codec negotiation. See [Codecs](#codecs) |
| Devices | At onboarding, a phone either creates its own identity or imports one from another device by QR. One device per identity is online at a time. See [Devices](#devices) |
| Invites | `https://thro.cc/join#<address+code>` on every platform. Verified links open the mobile apps; desktop gets `throcc://`. See [Invite links](#invite-links) |
| iOS distribution | App Store, TestFlight for betas |
| Android distribution | Signed APK on the GitHub release, and F-Droid's main repo with reproducible builds so it ships the same signature. IzzyOnDroid is out: it rejects apps partly written by generative AI |
| Releases | Separate versions and tags per platform: `ios-vX.Y.Z`, `android-vX.Y.Z` |
| License | AGPL-3.0 plus a section 7 additional permission for app store distribution |
| Protocol | One bump, to 2: a "signed in on another device" event and identity keys in presences. `MIN_PROTOCOL` stays 1: every server version supports the current protocol and the one before it. It ships as a server release before the mobile apps. See [Protocol 2](#protocol-2) |

## Layout

```
crates/throcc-mobile/     UniFFI layer: cdylib for Android, staticlib for iOS
mobile/ios/               Xcode project: app, broadcast upload extension
mobile/android/           Gradle project: app
```

`throcc-mobile` wraps `Client` and `MockClient` in one exported object. It mirrors `State`, `UiCommand` and `UiEvent` as UniFFI records and enums with `From` impls, so the core stays free of binding code. The app registers a listener; on every state change the layer sends the new state across. Video and audio never cross UniFFI: media stays in Rust from the network to the decoder, and the platform only hands over a surface to draw into.

The key manager wraps our sender secret only to people `State` shows (`Call::start_shown`), so nobody gets our key unseen. The apps keep that promise: every person `State` puts in a room appears on that room's screen, including blocked people and people in a room we're watching a share from fullscreen.

The iOS build makes an XCFramework from `aarch64-apple-ios` and `aarch64-apple-ios-sim`. The Android build runs cargo-ndk from Gradle for `arm64-v8a` and `x86_64` into one APK. There is no 32-bit build. UniFFI's Kotlin bindings use JNA.

`throcc-mobile` gets a `README.md` like every crate, and `--mock` sample screens exist on both platforms: a launch argument on iOS, an intent extra on Android, both backed by `MockClient`.

## Core changes

Each one follows the existing pattern of a backend per OS behind the same type.

1. **Paths.** `directories` has no Android support. The app passes its data and cache directories in at start; desktop keeps `ProjectDirs`.
2. **Keystore.** On iOS, the identity key goes to the keychain as a this-device-only item, readable after first unlock so a background reconnect on a locked phone works. Whether `keyring` 4 has an iOS store needs checking first; otherwise the keychain is called directly. On Android, the identity file in app-private storage is wrapped with an Android Keystore key. Both apps exclude their data from backups: a restore would roll the epoch floors back under the same key.
3. **Audio I/O.** New iOS and Android backends next to cpal: a VoiceProcessingIO audio unit, and AAudio with the voice-communication usage and input preset. Callbacks follow the same rules as desktop.
4. **Video without ffmpeg.** Video sits behind the `ffmpeg` feature today: `media/video/` is ffmpeg only, and `session.rs` gates video on it in 22 places. A `video` feature splits out from the ffmpeg backend, so a build has video with ffmpeg on desktop and with platform codecs on mobile. This is the first step of phase 5.
5. **Video decode.** VideoToolbox through `objc2` on iOS, MediaCodec through the `ndk` crate on Android. MediaCodec decodes straight into the Surface the app hands over. On iOS, decoded pixel buffers go to an `AVSampleBufferDisplayLayer` uncopied, as they reach gpui uncopied on macOS. VideoToolbox takes length-prefixed NAL units and a format description built from the parameter sets, so H.264 and HEVC access units are converted from what desktop sends. dav1d and libvpx output NV12 into pixel buffers for the same path. A decoder rebuilds when the codec, size or pixel format changes, and never writes a picture into a buffer made for another format.
6. **Video encode and capture.** Hardware HEVC, then H.264.
   - Android: MediaProjection feeds a MediaCodec input surface directly; AudioPlaybackCapture gives system audio.
   - iOS: a ReplayKit broadcast upload extension, Swift only to stay under its memory cap, encodes with VideoToolbox and passes access units and app audio to the main app over a Unix socket in the App Group container. The socket path must fit 104 bytes. The extension can't connect by itself: a second sign-in would kick the main app. The core gains an ingest path for these pre-encoded access units; the main app seals and sends them as usual and passes keyframe requests back. Broadcasting needs the main app in a room, which keeps it alive in the background.
7. **Reconnects.** See [Devices](#devices). This changes desktop too.
8. **Identity export and import.** See [Identity transfer](#identity-transfer).

## App work

- **iOS audio session:** `playAndRecord` with `voiceChat` mode, the background audio mode, interruptions and route changes.
- **Android audio:** `MODE_IN_COMMUNICATION`, audio focus, Bluetooth routing. A foreground service of type `mediaPlayback|microphone`, started with only the types granted, so someone who denies the mic still listens in a room. Type `mediaProjection` is added while sharing.
- **iOS local network:** `NSLocalNetworkUsageDescription`, for servers on the LAN.

## Codecs

Senders announce their codec and receivers must decode it. Desktop sends AV1, HEVC or H.264 in hardware, and VP9 in software only when it has no hardware encoder.

| Codec | Android decode | iOS decode |
| --- | --- | --- |
| H.264 | MediaCodec, hardware | VideoToolbox, hardware |
| HEVC | MediaCodec, hardware | VideoToolbox, hardware |
| AV1 | MediaCodec, since Android 10; the platform's software decoder where the chip has none | VideoToolbox on A17 Pro, M3 and newer; dav1d otherwise |
| VP9 | MediaCodec, since Android 4.4; hardware or the platform's software decoder | libvpx; iOS has no public VP9 decoder |

Each decoder is picked at runtime from what the device reports. On Android that includes the size: the platform's software AV1 and VP9 decoders may not take 2560 wide, so the app checks `isSizeSupported` and falls back to a bundled dav1d or libvpx. dav1d and libvpx are BSD-licensed and build from source on both platforms.

Phones send hardware HEVC, then H.264. They never encode AV1 or VP9.

## Devices

The server signs out an identity's earlier connection whenever it signs in again. That makes switching devices work, and needs three things:

- **Reconnects ask after a long gap.** A client signs in again on its own only while less than 5 minutes have passed since the link was last alive. The client stamps "last alive" in wall-clock time on every health tick and compares `SystemTime` against it before each automatic sign-in. `Instant` and tokio timers stop while a machine sleeps, so a window measured with them would never expire for a laptop waking up. That laptop also never receives a kick, so this check is its only guard. Past the window, the client shows a Reconnect button. On mobile, bringing the app to the foreground counts as that tap: iOS suspends the app outside a room, and opening it is a deliberate act.
- **A kick of its own.** The server tells the replaced connection `SignedInElsewhere` and closes it with a close code of its own. The session ends, shows "Signed in on another device" and never signs in again on its own. `leave_server` counts only `REMOVED` as proof the account is gone, so another device signing in during a Leave no longer makes the client forget the server. See [Protocol 2](#protocol-2).
- **Rate limits.** Every sign-in publishes the device's encryption key, and a changed key re-keys the room. The server's burst for key changes rises from 3 to 6, still refilling one per minute, so switching devices back and forth doesn't run into it.

## Protocol 2

One bump, shipped as a server release before the mobile apps. Desktop and CLI move to it in the same release.

- `Event::SignedInElsewhere`, last in `Event`, and a new close code in `throcc_proto::transport::close`.
- `Presence` carries the person's identity key, so clients refer to people by identity everywhere, not only in a shared room. The key is the server's word, not signed: it decides how someone is drawn and whether a block applies. Trust and verification still come only from the signed encryption key in a shared room. Every member of a server now sees every member's identity key.
- `PROTOCOL` becomes 2 and `MIN_PROTOCOL` stays 1. Every server version supports the current protocol and the one before it. The server keeps each session's protocol after the hello, and sends a protocol 1 session the protocol 1 shapes: `RemovedFromServer` in place of `SignedInElsewhere`, and presences without identity keys.
- The encryption-key change burst rises from 3 to 6.

## Blocking

App Store Guideline 1.2 asks apps with user content for filtering, reporting, blocking and contact information. Media is end-to-end encrypted, so nothing can be filtered. The first submission ships the minimum: blocking, a support contact in the listing, and a review note that moderation lies with each server's admins, who can kick, remove and set roles. Reporting waits until review asks for it.

- **What a block does:** the person is silenced, their shares are hidden, and their name and picture are not shown. Blocking is one-way: they still hear us and see our shares, and sender keys are still wrapped to them.
- **Only the drawing changes:** a blocked person stays in `State`. The key manager trusts only people `State` shows, so taking them out would cut them off from our key without anyone seeing it.
- **In a room:** a blocked person shows as a grey "Blocked person" row with no name or picture, so nobody listens unseen. Tapping the row offers Unblock.
- **Settings:** a Blocked list shows each blocked person's name and picture as they were when blocked, and the server, with Unblock. It is the only place their name appears.
- **By identity:** blocks are kept by identity key in `people.json`, like playback, so a block holds on every server where the key appears. With identity keys in presences, a blocked person is hidden in every room and the lobby, not only in a shared room.
- **Storage:** Adding them is the file's first migration, version 1 to 2: `add_blocked` inserts an empty `blocked` object. A 0.1 desktop client or CLI then refuses the file rather than dropping the blocks on its next write.
- Desktop gets blocking too. It needs protocol 2 for presences with identity keys.

## Identity transfer

Import is a choice at onboarding, next to creating a new identity. A phone that has an identity never imports another.

The phone shows a code; the user types it on the source device. The source then shows an animated QR, and the phone scans it. The QR and the code never appear on the same screen, so one photo or screen share can't capture both. The source refuses to export while it is sharing its screen.

- **Payload:** the identity key, each known server's address and pinned fingerprint, each verified key with its stored name and the server and user-id pairs it was seen under, and every standing key-changed warning, so a warning isn't lost by switching devices. No encryption key, settings, floors or volumes.
- **Encryption:** the payload is sealed under a key derived from the code, about 60 bits, with a memory-hard KDF.
- **Framing:** fixed-size frames that the source cycles through. A small payload is one frame. The phone collects frames in any order.
- **After import:** the phone always makes a new encryption key, as enrollment does, and publishes it on sign-in. Sender keys from before were wrapped to the other device's encryption key, so the phone can't open them, and leaving the floors out loses nothing. The transfer is a snapshot; later verifications don't sync.
- **Lost device:** whoever holds the identity key can sign new encryption keys and sign in indefinitely; it can't be revoked. A "Signed in on another device" kick is the only warning. An admin of each server removes the account, the owner starts a new identity, and the people who verified them verify the new one.
- The source can be desktop or phone; the target needs a camera.

This is security-critical code. A subagent that didn't write it reviews it before merge.

## Invite links

- Links are `https://thro.cc/join#<address+code>`. The secret stays in the fragment, which browsers never send.
- The mobile apps open them as verified links: Android App Links through `/.well-known/assetlinks.json`, iOS Universal Links through `apple-app-site-association`, both on thro.cc. No other app can claim them, unlike a custom scheme.
- On desktop, `thro.cc/join` is a static page that opens `throcc://join#...` and otherwise offers downloads. It has a strict CSP and no third-party scripts, because anything running on it can read the fragment.
- Verified links don't open the app from in-app browsers or pasted URLs. On a phone, the page then shows the address and code with copy buttons, and store links.
- The desktop client registers `throcc://`: `CFBundleURLTypes` on macOS, the installer's registry entries on Windows, `x-scheme-handler/throcc` in the flatpak's desktop file.
- The desktop client runs as a single instance. A link opened while it runs goes to the running app: macOS delivers it as an Apple event; on Windows and Linux a second process hands the link over through a local socket and exits. A second process that signed in would kick the first.
- The desktop's create-invite dialog shows the link next to the code. Mobile shows it with the platform share sheet.
- Opening a link fills in the join screen. It never joins without a tap.

## Phases

1. **Groundwork.** Update the scope line in `CLAUDE.md`, and add the rule that a server supports the current protocol and the one before. Add the section 7 permission. Create `throcc-mobile` and both projects. Injected paths and the mobile keystores, so the core cross-builds for all four targets in CI from the start. Register the app ids under the Apple team and create the Android signing key, which the verified links need.
2. **Protocol 2, devices and blocking.** `SignedInElsewhere`, identity keys in presences, per-session protocol on the server, the higher key-change burst, and a server release. The reconnect window, the `leave_server` fix, blocking and the `people.json` migration. Desktop gets all of it.
3. **Shell on mock data.** Starts once phase 2 lands, since it mirrors the `State` that phase changes. The UniFFI layer and every screen against `MockClient` on both platforms: onboarding, server list, rooms, room, person sheet, profile, settings, verified people, server pages.
4. **Voice.** Audio backends and the app audio work. Join a real server and talk.
5. **Watching shares.** First the `video` feature split, then platform decoders, dav1d and libvpx, fullscreen and rotation.
6. **Invite links.** The web page with its phone fallback and verification files, links on all five platforms, the desktop's single instance and dialog change.
7. **Identity transfer.** Export on desktop and mobile, import on mobile, security review.
8. **Sending shares.** Android first, then the iOS broadcast extension.
9. **Distribution.** App Store Connect upload from CI with an API key. The Android signing key in CI secrets, APK on the release. F-Droid recipe, reproducible builds, merge request to `fdroiddata`.

From phase 3 on, the iOS and Android work in each phase can run in parallel.

## Releases

A mobile release follows the desktop steps with its own tag. `ios-vX.Y.Z` uploads to App Store Connect for review. `android-vX.Y.Z` attaches the signed APK and `SHA256SUMS` to a GitHub release; F-Droid picks up the tag itself. The Android update check looks for `android-v` releases and is off by default, since F-Droid users get updates there. iOS has no update check.

## Risks

- **F-Droid and AI.** [fdroid/admin#641](https://gitlab.com/fdroid/admin/-/issues/641) proposes an anti-feature tag for apps that use AI. Unsettled; a tag wouldn't block inclusion.
- **Software decoding.** 1440p60 AV1 or VP9 in software may drop frames and drain battery on older phones.
- **Portrait shares.** A phone share is portrait, at most 1440 tall. Desktop tiles, fullscreen and pop-outs have to handle it.
- **iOS extension memory.** Broadcast extensions are capped at about 50 MB. Encoding stays in hardware and buffers stay small.
- **iOS share audio.** VoiceProcessingIO may duck or degrade share audio while a call runs. ReplayKit's app audio may include throcc's own voice playback, which desktop keeps out of shares.
- **App Store review.** Review needs a reachable demo server and fresh single-use invites. Guideline 1.2 may need reporting on top of blocking. Background audio for voice needs a clear justification. Export compliance needs answering for the end-to-end encryption.
