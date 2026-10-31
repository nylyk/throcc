# Decisions

Each entry: date, decision, why, alternatives.

## 2026-10-28 Human answers to the opening questions

- **CI:** the agent pushes to `origin` and reads results through the public GitHub REST API.
- **Server dependency check:** `scripts/check-server-deps.sh` fails if `throcc-crypto`'s `media` feature or any codec, capture, audio, HPKE or UI crate appears in `throcc-server`'s tree. `ring` stays allowed, because quinn's rustls needs it for TLS. Alternative: a separate AEAD crate for media, banned from the server.
- **ffmpeg:** development uses Homebrew's keg-only `ffmpeg@8` (a GPL build). The LGPL build is an M11 task. Alternative: build LGPL ffmpeg from source now.
- **UI:** the mockups are inspiration. Add whatever screens the product needs.
- **§7 defaults** apply: shares are opt-in, the HEVC software decoder ships, Wayland without GlobalShortcuts gets voice activation only, the macOS signing identity is asked at M11.

## 2026-10-28 Dependency versions

- **Decision:** the root `Cargo.toml` names every version once, and `Cargo.lock` is committed. Crypto uses the RustCrypto generation of curve25519-dalek 5 (`ed25519-dalek` 3, `x25519-dalek` 3, `hpke` 0.15, `hkdf` 0.13, `sha2` 0.11).
- **Why:** one place to bump. `hpke` 0.15 has the newer generation as optional features, so the tree stays small.
- **Alternatives:** the older dalek 4 generation, which would match `spake2` 0.4 but pull `hpke` 0.13 with `rand_core` 0.9. `spake2` 0.4 alone keeps curve25519-dalek 4 as a duplicate.

## 2026-10-28 Mockups

- **Decision:** `design/mockups/<Screen>.dc.html` are unpacked from the canvas export `throcc UI mockups.html`, one per board. `Main.dc.html` is the Lobby board.
- **Why:** PLAN.md §5.13 points screen work at one file per screen.

## 2026-10-28 S1 PAKE crate

- **Decision:** `spake2` 0.4.0 (RustCrypto), `Ed25519Group`, `start_a` on the client and `start_b` on the server. The code id goes into both identities.
- **Why:** the most used maintained Rust PAKE, MIT/Apache, pure Rust, and it exposes the 32-byte key. Confirmation and transcript binding are ours: HKDF-SHA256 over the key with the transcript hash as salt, two HMAC keys, HMAC over the full transcript.
- **Alternatives:** `cpace` 0.1.0 (dead since 2020), `pake-cpace` 0.1.7 (pre-CFRG draft), `pakery-cpace` 0.6.2 (draft-18, one author, unproven).
- **Caveats:** unaudited; no small-order point check (the transcript MAC covers it); the key is wrapped in `Zeroizing` by us. Revisit when `spake2` 0.5 ships stable.

## 2026-10-28 S3 gpui and the video surface

- **Decision:** `gpui-pre` 0.3.10 (a weekly crates.io republish of zed@08cbb88 by the gpui-component author) with `gpui-component` 0.8.1. macOS shows video with `gpui::surface(CVPixelBuffer)` in NV12 full range (`420f`). Linux and Windows show BGRA `RenderImage`s through `img()`, dropping the previous frame's image each time.
- **Why:** gpui-component depends on `gpui-pre`; crates.io `gpui` 0.2.2 predates the wgpu renderer. Linux (wgpu) and Windows (D3D11) have no surface primitive upstream. The image path ran 2560×1440 at the display's 120 Hz on macOS.
- **Alternatives:** a zed git rev (breaks gpui-component), the `gpui-fast` fork, a ~700-line NV12 surface patch for wgpu and D3D11, deferred until profiling asks for it.
- **Caveats:** 0.3.10 aborts on non-`420f` pixel buffers and can recycle a pool buffer before the GPU reads it (fixed upstream in 1808df0); bump when the next release ships. Without Xcode, the `runtime_shaders` feature is needed.

## 2026-10-28 S4 Video loss recovery

- **Decision:** every encoder gets a 4 s GOP cap and a forced IDR on request (`forced-idr`/`forced_idr=1` on NVENC, AMF and QSV). Intra refresh is added only on NVENC (all codecs) and QSV H.264/HEVC, escalating to IDR if one sweep doesn't recover. New viewers always get an IDR.
- **Why:** ffmpeg 8 exposes intra refresh only on NVENC, QSV H.264/HEVC and AMF H.264. VA-API, VideoToolbox and OpenH264 have none, so the fallback has to work everywhere.
- **Alternatives:** intra refresh everywhere (needs ffmpeg patches); choosing the codec by intra refresh support (conflicts with AV1 > HEVC > H.264).

## 2026-10-28 M1 transport

- **Decision:** `DATAGRAM_PAYLOAD_FLOOR` is 1162, measured with quinn 0.11.12 at a 1200-byte MTU with discovery off (`payload_floor_matches_quinn_at_1200_bytes`). TLS uses rustls with the `ring` provider, TLS 1.3 only.
- **Why:** the measured value matches PLAN.md §5.5. `ring` avoids aws-lc's C and CMake build on Windows.
- **Alternatives:** `aws-lc-rs` as the provider.

## 2026-10-28 M2–M4 protocol details

- **`PakeStep = Result<PakeReply, AuthError>`.** The server answers `Hello::Enroll` with this, so a rate-limited client gets a reason instead of a PAKE reply it can't parse. Postcard isn't self-describing, so the two shapes need one tagged type. Alternative: one handshake enum for every server-to-client handshake message.
- **`AuthResult::Ok` boxes a `Welcome` struct** with the spec's fields, to keep the enum small.
- **Failed enrollments count per source, whether the id is unknown or the secret is wrong.** Ten failures per 10 minutes per IPv4 address or IPv6 /64, then `RateLimited`. Unknown ids run the PAKE against a random decoy secret, so they fail at the same step as a wrong secret. Counting only unknown ids would let the limiter itself reveal which ids exist.
- **An invite's try is counted before the server sends `PakeReply`**, and the burn check runs whenever a run ends without success. A client that disconnects mid-run still uses a try.
- **The bootstrap invite has no minter**; whoever enrolls with it becomes Admin.
- **Only admins set roles, and only to a rank below their own.** The mockup offers only Manager and User, so there is one Admin. Cascade removal is refused if anyone in the invite tree has a rank at or above the actor's.
- **A second connection by the same user kicks the first**, because presence is keyed by user.
- **Lobby clients publish no presence.** `Presence.room` is always `Some`; leaving to the lobby sends `PresenceEnded`.
- **`SetEncKey` is verified by the server** against the identity key and is required before `SetRoom`. Changing it while in a room bumps the epoch.
- **`SetMedia` never bumps the epoch.** A share change re-sends `RoomState` at the same epoch, so peers learn the codec without rekeying.
- **The X25519 encryption key's private half lives in a 0600 file** next to the keystore, with its creation time, and is replaced after 30 days. The identity key uses the OS keychain. Alternative: both in the keychain.
- **A kicked session's writer sends `Kicked`, finishes the stream and waits up to 2 s for the client to read it before closing.** Closing at once dropped the event.

## 2026-10-28 M3 review

A fresh subagent reviewed `pake.rs`, `identity.rs`, `auth.rs`, `permissions.rs`, the handshake and the client's enroll path. It found channel binding, offline-guess resistance, replay protection, the pin verifier and rank checks sound. Its findings and what was done:

1. **High, fixed:** concurrent runs on one invite could each enroll a user. `Database::enroll` now re-checks the invite inside its transaction and requires the delete to remove exactly one row. `users.id` is `AUTOINCREMENT`, so a removed user's id is never reused. Test: `concurrent_runs_on_one_invite_enroll_at_most_one_user`.
2. **Medium, fixed:** parallel runs bypassed the per-source limiter, which counted only finished failures. A run now counts when it starts and is forgiven on success. Test: `parallel_runs_from_one_source_are_rate_limited`.
3. **Low, fixed:** handshake frames are capped at 4 KiB, at most 128 connections may be unauthenticated at once, and quinn's `max_incoming` is 256.
4. **Low, fixed:** an invite id that isn't 3 characters is treated as unknown, and only 8 characters of it are logged.
5. **Low, fixed:** `Hub::connect` re-reads the user, so one removed between authentication and connect is refused.
6. **Low, fixed:** minting gives up after 64 id collisions instead of looping with the hub locked.
7. **Low, fixed:** the limiter drops sources whose window is empty.
8. **Nit, accepted:** a known id costs one SQLite write before `PakeReply`; an unknown id doesn't. Each id yields at most 3 samples before it burns, and ids are only 15 bits, so learning which exist gains nothing.
9. **Nit, fixed:** a failed write of the try count refuses the run.

## 2026-10-28 M5 media path

- **Media AEAD is RustCrypto `aes-gcm` with `zeroize`**, not `ring` or `aws-lc-rs`. Its key schedule is wiped on drop, which §5.7's "zeroized" needs; `ring` keys can't be wiped. It is already in the tree through `hpke`. A property test checks every output against `ring`. `scripts/check-server-deps.sh` bans `aes-gcm` and `hpke` from the server.
- **Safety numbers use Signal's numeric fingerprint** (SHA-512, 5200 iterations, 30 bytes as six 5-digit groups per key, the lower half first), with the identity key as its own stable identifier. The test vector was computed independently with Python's `hashlib`.
- **A user whose identity key changed is left out of key distribution until the change is accepted**, matching the mockup's "Accept new key". A peer whose encryption key signature fails is left out too.
- **Sender keys that arrive before their room state are held for 2 s and applied with it.** The `Placed` reply and peers' `SenderKey` events travel separate paths in the client and can be processed in either order.
- **The receive loop checks the replay window before decryption**, as §5.12 lays out. Only the server can inject datagrams on the connection, and it can already deny service, so a forged high `seq` costs nothing new.
- **The SFU's video budget counts both ways:** queued bytes over the path rate quinn reports (congestion window over RTT), and the age of the oldest unit not yet sent. Either over 150 ms triggers drops.
- **The sender drops video older than 100 ms when its turn comes** (`SEND_BUDGET`), before it gets a `seq`, and tells the encoder to recover unless the unit was discardable.
- **The reorder window lives in `media/reorder.rs`**, shared by future video and the synthetic path, not under `media/video/`.

## 2026-10-29 M5 review

A fresh subagent reviewed the wire format, media AEAD, HPKE wrapping, safety numbers, key manager, media engine, SFU and egress. It found the header layout, nonce and AAD construction, nonce uniqueness, key derivation, HPKE context binding, the server's ownership, routing, watching and key-relay checks, and the hostile-input parsing sound. Its findings and what was done:

1. **High, fixed:** a sender rekeyed only when the epoch number changed, so a server could add a member without bumping the epoch and hand it the current secret. A sender now takes a fresh secret whenever the trusted peer set differs from the set the secret was wrapped to. Test: `a_peer_added_without_an_epoch_bump_gets_a_fresh_secret`.
2. **Medium, fixed (format change):** the signed encryption key now covers `"throcc/enc-key/1" || server SPKI hash || user id || not_after || key`, travels as `SignedEncKey`, and expires after 30 days. Server and clients reject expired keys. A replayed old key no longer works.
3. **Medium, fixed:** cached track keys of a departed peer outlived the grace period. The cache stores each key's owner and drops it with the owner's secret. Test: `a_departed_peers_cached_keys_are_dropped_after_the_grace_period`.
4. **Medium, fixed:** a started unit whose last fragment was lost stayed in the egress queue forever. The next first fragment on the track now closes it. Test: `a_unit_whose_last_fragment_was_lost_does_not_stay_queued`.
5. **Medium, fixed:** queued audio per receiver has a hard cap of 2000 datagrams, counted separately from the drop policy.
6. **Medium, documented:** media is **group-authenticated only**. Every member holds every sender's secret, so a member who colludes with the server can forge another member's media; the server's ownership check is the only barrier. HPKE auth mode authenticates the wrapped key, not the media. Per-sender signatures would cost a signature per datagram or a second key scheme; deferred, as for text chat and MLS.
7. **Low, fixed:** an old-epoch `SenderKey` is accepted only inside the grace window after the epoch moved on, and expires with it. Test: `a_replayed_old_sender_key_is_refused_after_the_grace_period`.
8. **Low, fixed:** replay windows survive unsubscribing and resubscribing, until the room changes.
9. **Low, fixed:** pending ciphertext is retried only on the 20 ms tick and only when the key generation changed; track owners are a map, not a scan.
10. **Low, fixed:** a `Placed` older than the current epoch is ignored, and a `RoomState` that arrives before its `Placed` is kept and applied after it. Test: `room_state_before_placed_is_kept_and_a_stale_placed_is_ignored`.
11. **Low, fixed:** keyframe aggregator entries are forgotten when their track's owner leaves the room.
12. **Low, fixed:** the early sender-key buffer holds at most 64 entries, one per sender and epoch.
13. **Nit, fixed:** a new contact is pinned only after their encryption key verifies.
14. **Nit, fixed (format change):** safety numbers hash `version || key || key`, the key as its own stable identifier, as Signal does with a stable id. New vector from Python's `hashlib`.
15. **Nit, fixed (format change):** the HPKE info includes the server's SPKI hash.

## 2026-10-29 Wire format and crypto frozen

The media header (§5.5), media AEAD and track-key derivation, HPKE sender-key wrapping and its info, the signed encryption key, and the safety-number construction are **frozen** as of this entry. Changing any of them needs a human decision.

## 2026-10-29 M6 egress and network tests

- **The egress budget is queueing delay:** video is dropped when the oldest unit not yet sending has waited more than 150 ms. An earlier version also estimated the drain rate as congestion window over RTT; its drops kept the connection app-limited, so the window never grew and a jittery link starved. Quinn's 64 KiB buffer now stays full and the controller keeps probing.
- **Loss detection tolerates reordering:** `packet_threshold` 64 and `time_threshold` 2.0 × RTT on both ends. Datagrams are never retransmitted, so a spurious loss only shrinks the window.
- **`tests/netem/`** runs four scenarios in a privileged container with network namespaces, `tbf` and `netem`: `tests/netem/run.sh`, or `tests/netem/run.sh receiver_limited`. CI runs it on ubuntu. Measured on colima (aarch64, kernel 6.8), 10 Mbps share plus 50 frames/s mic, 600 units:

| Scenario | Result |
| --- | --- |
| Receiver B limited to 3 Mbps | A and C 600/600 intact, 0 server drops. B: 425 whole units dropped by the server and reported in `LinkStats`, 0 corrupt, mic 490/500 with 0 server drops. Sender: keyframe requests ≥ 1000 ms apart. |
| 2% random loss | about 575/600 intact per receiver, 0 corrupt, keyframe requests ≥ 1000 ms apart. |
| 40 ± 30 ms jitter (normal), reordering | **Cubic collapses:** about 75/600 intact per receiver, about 520 units dropped by the server. **BBR (`--congestion bbr`):** two receivers 580/600, the third 125/600. Mic unaffected either way. |
| Sender uplink limited to 5 Mbps | sender drops 315 units locally before they get a `seq`; receivers see 0 server drops. |

- **Open:** under heavy reordering Cubic's window collapses. BBR does better but not reliably. Cubic stays the default as §5.1 says; the choice of default is a human decision once there is real-network data.
- **Open:** a receiver behind a 3 Mbps `tbf` still loses many datagrams on the path, because the server sends faster than the bottleneck until loss pushes the window down. Only about 1.7 Mbps of intact video arrives. Pacing to an estimated bottleneck rate would help; §5.5 leaves pacing to quinn unless measurement asks, and this is that measurement.

## 2026-10-29 Working without the human

The human said they won't be available and asked for every open question to be decided in the project's interest. Decisions from here on are made by the agent and recorded here as usual.

## 2026-10-29 M6 review

A fresh subagent reviewed `sfu.rs`, `egress.rs`, the routing code in `rooms.rs` and the netem harness. It found routing, rebuilds, ownership, task lifetimes, locking, drop order and keyframe aggregation sound. Findings and what was done:

1. **Medium, fixed:** a fragment joined the newest open unit of its track, so reordering on the sender's uplink merged and truncated units. Egress now keeps each track's last 32 unit starts and places a fragment by `seq`; a late fragment of a unit already sent is forwarded on its own and never counted as a drop. Tests: `fragments_reordered_upstream_join_their_own_unit`, `a_late_fragment_of_a_sent_unit_is_still_forwarded`; netem scenario `uplink_jitter`.
2. **Medium, fixed:** a started unit could grow without bound. Units are capped at 4096 fragments, and a started unit that gets nothing for 1 s is given up.
3. **Medium, fixed:** the netem check "audio to B is never dropped" could not fail. Audio dropped by the safety cap or a full inbox is now counted in `LinkStats.dropped_units` for its track, which the check reads.
4. **Low, fixed:** a replaced connection could still send requests for the user's new session. `Hub::handle` takes the connection id and refuses a stale one.
5. **Low, fixed:** stalled units after a track vanished; covered by the 1 s idle expiry.
6. **Low, accepted:** audio waits behind video already in quinn's 64 KiB buffer, about 300 ms at 1.7 Mbps. A separate audio path would need a second connection or stream priorities quinn's datagrams don't have.
7. **Low, fixed:** `place` bumps both epochs before changing any membership.
8. **Nit, fixed:** the audio cap comment.
9. **Nit, fixed:** `LinkStats` counters are taken only once a control-queue slot is reserved.

Netem results after the fixes: all checks pass. With 40 ± 30 ms jitter on the sender's uplink, the sender's own window collapses (492 of 600 units dropped locally, the same Cubic behaviour as on downlinks), and what does arrive is intact with no server drops.

## 2026-10-29 M7a voice pipeline

- **Jitter buffer:** our own (`media/jitter.rs`), chosen by the human over patching `neteq`. It decodes 20 ms Opus frames in timestamp order, recovers one lost packet from the next packet's in-band FEC, conceals up to 3 frames, then goes quiet and rebuffers. Target delay is 20 ms plus 3 × the RFC 3550 jitter estimate, capped at 200 ms. When the buffer runs more than 2 frames over target it skips a frame, at most every 200 ms. It exposes the playout timestamp for §5.10. Arrival times are parameters, so tests run on a simulated clock: the 5-minute-mute test reaches 60 ms one second after resuming.
- **Acceptance tests** (`media/audio.rs`) use a synthetic speech-like signal: a gliding harmonic series shaped into syllables. Echo path: four reflections through a `tanh`-overdriven loudspeaker. Measured: ERLE 64 dB once converged (floor 20 dB). Double talk: near-end speech kept at −6.0 dB with correlation 0.64 against the same speech processed alone (floors −8 dB and 0.6, set after measuring). FEC: a lost frame comes back at 9.2 dB SNR against the lossless decode, concealment at 0 dB.
- **Double talk is compared with the talker processed alone**, not the raw talker. AEC3's filterbank shifts phase by about 650 samples and the raw correlation drops to 0.5 even with the far end silent.

## 2026-10-29 M7 live voice

- **Device callbacks only move samples through `rtrb` rings** (`media/devices.rs`, which denies indexing, `unwrap`, `expect` and `panic`). Input is downmixed to mono, output copied to every channel; a full or empty ring is counted, never waited on.
- **One audio worker thread** (`media/voice.rs`) runs the jitter buffers, mixer, echo-canceller reference, capture chain and Opus encoder, keeping about 40 ms queued for the speakers. Devices open at 48 kHz when they support it; otherwise the worker resamples 10 ms blocks with `sonora-common-audio`'s `PushSincResampler`.
- **Deafen also mutes the mic, and unmuting undeafens**, as in TeamSpeak and Discord.
- **The real `Client`** owns a 2-thread tokio runtime. Its session task keeps `State` current, hears every peer's mic, reconnects with backoff (1 s doubling to 15 s) back into the same room, and refreshes the connection panel every 2 s with a one-sentence verdict per §5.11.
- **Push-to-talk keys** will use the `global-hotkey` crate (Carbon hotkeys on macOS, `RegisterHotKey` on Windows, `XGrabKey` on X11) and the GlobalShortcuts portal on Wayland, instead of a raw `CGEventTap` and low-level Windows hook. Carbon hotkeys don't need the Input Monitoring permission. Wayland without the portal gets voice activation only (§7 default).

## 2026-10-29 M8 desktop client

- **Built in the main session, not by per-screen subagents.** The screens share one state model and widget set, and splitting them would have meant re-explaining both nine times.
- **Buttons use gpui-component's built-in variants, with its theme set from our tokens through `Theme::update`.** Its custom variant paints any colour at about 20 % strength, which can't produce a solid accent. Inputs, switches and focus rings come from the same theme. Every colour still lives only in `theme.rs`.
- **Fonts:** the mockup export embeds Geist only as woff2 subsets, which gpui can't load. The theme picks the first installed family from Geist, then the platform UI font; the Geist TTFs get bundled in M11.
- **Icons:** Lucide SVGs from `gpui-kit-assets` (ISC), which has every icon the mockups use.
- **Screens beyond the mockups:** pin mismatch (with "use a fresh invite" or "trust the new key"), join errors in plain language, a reconnecting banner, empty states, and the remove-with-cascade confirm. The Appearance and popped-out share screens followed later (see 2026-10-31 Themes, room grid and pop-out).
- **Push-to-talk:** a global Alt+Space via `global-hotkey`, plus Space while the window has focus. Wayland gets the in-window key only.
- **Screenshots:** `scripts/screens.sh` opens each screen of the mock client (`--mock --screen <name>`) and captures its window into `docs/screens/`. `tests/screens/` is a Linux variant under Xvfb for machines without a display.
- **Listing known servers no longer opens the keychain**; only joining does, so the app doesn't ask for the keychain password at startup.

## 2026-10-29 M9 screen share (macOS)

- **Captured frames are copied to CPU NV12, not mapped as GPU `AVFrame`s.** Measured on an M-series MacBook: capture plus `hevc_videotoolbox` at 2560×1440, 57.6 fps and 10.4 Mbps takes 0.08 cores (`sending_1440p60_takes_under_one_core`). The `av_hwframe_map` path stays open if another platform needs it.
- **ScreenCaptureKit via the `screencapturekit` crate's Swift bridge.** Its build script only searches Xcode's toolchain for the Swift compatibility libraries, so `throcc-client-core/build.rs` adds the active toolchain's path from `xcrun`, and `.cargo/config.toml` adds the `/usr/lib/swift` rpath for its runtime dylibs.
- **Keyframes are sent twice** (`KEYFRAME_COPIES`), whole unit after whole unit, with identical sealed bytes. VideoToolbox has no intra refresh (S4), and its keyframes ignore the VBV: about 60 datagrams at 1080p, so at 2 % loss only about 30 % arrived whole and the picture stayed frozen. With two copies the receiver's replay window fills each lost fragment from its copy, and the server queues the copy as its own unit. Cost: one extra keyframe per GOP or recovery, about 130 kbps at 6 Mbps and a 4 s GOP. The wire format is unchanged. Voice is sent between the keyframe's datagrams, and the copy stops early once a newer frame waits and half the send budget is gone, so a repair never makes the sender drop the next frame. Forced recoveries on the sender are at least 1 s apart. The server counts a dropped copy as lost only when no other copy reaches the viewer. `keyframes_arrive_whole_through_two_percent_loss` expects 80 % of keyframes whole at 2 % loss: about 97 % arrive on macOS and Linux, 88 % on Windows, whose loopback loses packets in runs, and about 30 % without copies.
- **The freeze after a loss is bounded by the server's one-request-per-second cap plus the 50 ms reorder window**, about 1.1 s at worst. `two_percent_loss_brings_a_refresh_within_a_second` checks every keyframe request is answered within 1 s and the longest freeze stays under that bound.
- **Measured encode-to-decoded latency** through a local server at 1080p60: median 16 ms, p95 18 ms (`a_shared_picture_arrives_within_a_few_frames`). Capture and display add about one frame each.
- **The encoder fits pictures of another size** into its configured size with `swscale`, aspect kept and black bars around, because a portal stream can deliver the monitor's full size whatever was asked.
- **All three platforms capture** (Linux and Windows below); the "macOS to Linux and back" check needs two machines (`MANUAL_TESTS.md`).

## 2026-10-29 macOS build requirements

- **Building the client on macOS needs the macOS 26 SDK (Xcode 26 or its Command Line Tools).** `screencapturekit` pulls in `apple-metal`, whose Swift bridge uses Metal tensor APIs that older SDKs lack. CI runs on `macos-26`. The app's deployment target is unaffected. If that becomes a burden for contributors, `objc2-screen-capture-kit` binds ScreenCaptureKit without Swift.
- **CI installs `ffmpeg@8` after `brew update`**: Homebrew's `ffmpeg` is 9, and `rsmpeg` 0.18 targets 8.

## 2026-10-29 M9 screen share (Windows)

- **Windows Graphics Capture through the `windows` crate (0.62.2, MIT OR Apache-2.0) directly, not `windows-capture`.** `cpal` and `gpui` already pull `windows` 0.62.2, so this adds features, not a crate. `windows-capture` 1.5 builds on `windows` 0.61, a second copy, and brings `parking_lot` and `rayon` into its frame callback, which the realtime rule forbids.
- **The FrameArrived handler only moves frames.** On the free-threaded pool it takes the next `Direct3D11CaptureFrame`, stamps it, and `try_push`es it into an `rtrb` ring of 2 behind a `try_lock`. A full ring or a lock collision drops the frame and counts it. The pool holds one texture more than the ring.
- **The handler paces frames to the share rate.** `SetMinUpdateInterval` exists only from Windows 11 24H2, and the share thread encodes every frame it gets, so a 144 Hz display would otherwise be encoded at 144 fps. `Pacer` keeps one frame per interval on average, with a quarter-interval tolerance for jitter.
- **BGRA to NV12 and scaling run on the GPU** in one `ID3D11VideoProcessor` blit, full-range BT.709 out (`DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709`), aspect fitted and black-barred. The share thread copies the result to a staging texture, maps it and copies the planes into the `VideoFrame`. The map waits for the GPU on the share thread, never in the handler. The capture frame is closed only after that wait, so the pool can't refill a texture the blit still reads.
- **The D3D11 device is multithread-protected.** The frame pool uses the device from system threads while the share thread uses its immediate context.
- **The picker is parented to the foreground window**, checked to belong to this process, because `start` runs on a blocking pool thread with no window of its own. Borderless capture is requested and `IsBorderRequired` set to false; both are ignored where the OS refuses.
- **Not run on Windows.** `cargo clippy -p throcc-client-core --target x86_64-pc-windows-msvc --all-targets` is clean on macOS, with stub C headers and a stub archiver so `ring`'s build script runs. The picker, the blit and the readback need a Windows machine (`MANUAL_TESTS.md`).

## 2026-10-29 M9 screen share (Linux)

- **The portal is driven with `ashpd`'s `async-io` runtime and `async_io::block_on`.** gpui already pulls `ashpd` with `async-io`, and `ashpd` refuses both runtime features at once. `start` blocks on the portal like the macOS picker.
- **The portal session is closed explicitly when the capture drops.** `ashpd` keeps one shared D-Bus connection for the process, so the compositor would otherwise keep casting until the app exits.
- **The restore token lives in `<data dir>/screencast-restore-token`**, with `PersistMode::ExplicitlyRevoked`. Each share returns a fresh single-use token that replaces the file. A stale token makes the portal ask again.
- **Shared memory only (`MemPtr`/`MemFd`, mapped by PipeWire).** The format offers no DRM modifiers, so compositors fall back from dmabuf. dmabuf import with negotiated modifiers is left for when a GPU path needs it.
- **The PipeWire process callback only copies.** It moves the planes into one of four preallocated slots, recycled free → filled → free through two `rtrb` rings; the share thread converts to NV12. Slots start at the configured size × 4 bytes. A larger stream drops the first frames (counted) until the share thread has grown each slot once.
- **The requested size is a preference.** Mutter offers a fixed monitor size, so `next_frame` reports what the stream delivers, as on macOS. The max frame rate is capped at the configured fps.
- **Stream errors carry no message.** Copying PipeWire's error text would allocate in a callback; a stream that fails or a session the compositor ends just stops delivering frames. `PIPEWIRE_DEBUG=3` shows the cause.
- **The PipeWire path is tested without the portal** (`pipewire_source_delivers_converted_frames`): an in-test BGRx source node, a `pipewire` daemon and `wireplumber`, inside a Linux container.

## 2026-10-29 M10 share audio and sync

- **Playback is stereo end to end.** The playback ring carries interleaved left and right; the device callback puts them on the first two channels and their mean on the rest (or on the only one). Voices are mono, added to both sides. The echo canceller gets the mono mean of the whole mix, so share audio is cancelled from the mic like voices are.
- **The jitter buffer takes any channel count**; share audio uses one per watched share, stereo, outside the voice mixer, with no volume control yet.
- **Share audio keeps being pulled while deafened**, only silenced, so its playout position and the video held to it keep moving.
- **The playout position published to the video decoder is what reaches the speakers**: the jitter buffer's playout timestamp minus the playback queued ahead of it. The device's own buffer is not counted; it is a few milliseconds.
- **Video is held while it is ahead of the share audio being heard**, at most 32 frames and never more than 0.5 s ahead: past that the audio has stalled, and a frozen picture is worse than unsynced sound. Without share audio playing, nothing is held.
- **Capture backends report both media from one zero**: `Capture::next_audio` returns 20 ms of 48 kHz stereo with a 48 kHz timestamp counted from the same instant as the video's 90 kHz timestamps. On macOS both callbacks only queue the sample buffers with their host time; the share thread picks the zero, unpacks audio (planar or interleaved float) and re-chunks it to 20 ms, restarting the chunking when a buffer lands more than 10 ms away from where the previous one ended.
- **`ShareConfig.audio` is true only when the capture delivers audio**, so receivers never wait for audio that will not come.

## 2026-10-29 M11 server packaging

- **Static server binaries are built with musl in a container** (`tests/deploy/Dockerfile.static`), so the same command works on any Docker host. CI builds x86_64 on `ubuntu-latest` and aarch64 natively on `ubuntu-24.04-arm` and keeps them as workflow artifacts for a week; publishing a release is left to the human.
- **`tests/deploy/run.sh` stands in for a clean Debian VM**: a `debian:12` container with systemd as init installs the static binary and `packaging/throcc-server.service`, then checks the unit is active under a dynamic user, listens on UDP 8476, logs the first invite, keeps its key in the state directory and keeps its identity over a restart.
- **The container image's runtime stage is `distroless/cc-debian12:nonroot`** with `/data` owned by its user, so a fresh named volume is writable. The `dev` stage builds into its own `/target` volume, so a bind-mounted checkout is never written to.
- **cargo-chef 0.1.78 and watchexec-cli 2.8.0** are pinned in the Dockerfile.

## 2026-10-29 M10 share audio (Windows)

- **Process loopback in exclude mode on throcc's own process tree, for every share.** `ActivateAudioInterfaceAsync` on `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` with `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` and `std::process::id()`. Include mode on the shared window's process tree is not possible yet: `GraphicsCaptureItem` from the picker exposes only `DisplayName` and `Size`, not the window or its process. A window share therefore also carries other apps' sound. Getting the window would need our own window picker (`TryCreateFromWindowId`), not the system one.
- **48 kHz stereo 32-bit float is requested** with `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY`, so WASAPI resamples whatever the mix format is. The client is event-driven, shared mode, with a 100 ms buffer.
- **A dedicated `throcc-share-audio` thread waits on the WASAPI event** (100 ms timeout to see the stop flag), splits each buffer into packets of at most 20 ms and `push`es them into an `rtrb` ring of 32. A full ring drops the packet and counts it in the capture's `dropped`. Silent buffers queue zeros; buffers flagged `TIMESTAMP_ERROR` are skipped. The packets are fixed-size, so the thread does not allocate while running.
- **Both media are stamped from one QPC zero on the share thread.** The frame handler queues `SystemRelativeTime` and the audio thread queues `GetBuffer`'s QPC position, both in 100 ns units. `ShareZero` takes the first one the share thread stamps as zero, and `AudioChunker` re-chunks to 20 ms with the macOS slip rule (restart past 10 ms). Frames and audio stamped before the zero are dropped. Both live in `capture/clock.rs`, compiled for Windows and for tests, so their tests run on every platform. The pacer only thins frames by their capture times.
- **Share audio failing to start does not stop the share.** If activation fails (Windows older than 10 2004) or takes over 3 s, the share runs video-only with a warning, and `captures_audio` is false, so `ShareConfig.audio` stays false.
- **`windows-core` 0.62.2 is a direct dependency** (already in the tree through `windows`): the `#[implement]` macro for the activation completion handler expands to `::windows_core` paths.
- **Not run on Windows.** `cargo clippy -p throcc-client-core --target x86_64-pc-windows-msvc --all-targets` is clean on macOS with the stub headers and archiver. Activation, the capture thread, the QPC alignment and the echo exclusion need a Windows machine (`MANUAL_TESTS.md`).

## 2026-10-29 M10 share audio (Linux)

- **A private PipeWire capture stream linked by hand, as Vesktop's venmic does.** The stream connects without autoconnect, so the session manager never links it. A registry listener on the same thread links the output ports of every `Stream/Output/Audio` node of another process to its `FL`/`FR` input ports, as nodes and ports appear; PipeWire mixes the links on each input port and converts to the 48 kHz stereo float the stream asks for. `FL`/`SL`/`RL` feed the left input, `FR`/`SR`/`RR` the right, `LFE` nothing, and any other channel both. Links are created with `object.linger = false` and kept as proxies, so they go when a port goes or the capture stops. Alternatives: the default sink's monitor, which would carry throcc's own playback; a virtual sink apps are moved to, which changes where the person hears them.
- **A stream belongs to this process when its client's `application.process.id` is our pid.** Native PipeWire stream nodes carry no process id of their own; the client does. Checked in a container: an ALSA-plugin client reports the player's pid, and `pipewire-pulse` gives each PulseAudio app its own client with the app's pid, while `pipewire.sec.pid` there is the `pipewire-pulse` daemon's. So both ways cpal's ALSA output can reach PipeWire are recognised. Clients are bound to read their info; a playback stream is linked once its client's info has arrived.
- **Both media are stamped with PipeWire time**: `pw_time.now` of the graph cycle that delivered the buffer or frame, in CLOCK_MONOTONIC nanoseconds, read with `Stream::time()` in each process callback. The video callback used `Instant::now()` before; one source for both lets the share thread pick a single zero, as on macOS. The audio re-chunking to 20 ms and its 10 ms slip rule are the macOS ones.
- **The process callback copies into two `rtrb` rings**: interleaved samples (one second) and one entry per buffer with its time and frame count (64). Either ring full drops the buffer and counts it on the capture's drop counter.
- **Audio failing to start leaves a silent share**, with a warning; `captures_audio` is then false, so `ShareConfig.audio` is too.
- **Tested in a Linux container** (`desktop_audio_has_other_apps_but_not_ours`): `pipewire`, `wireplumber` and a null sink; `pw-play` plays a 1 kHz tone from another process while a stream in the test process plays 300 Hz. The capture has the 1 kHz tone at full level and the 300 Hz one 35 dB down (onset splatter, the stream is not linked), in steady 960-tick steps, within 40 ms of the video's timestamps. A real desktop is in `MANUAL_TESTS.md`.

## 2026-10-29 Profile pictures

- **Pictures are fetched lazily by hash and checked against it.** The first `ClientApi::avatar(hash)` queues one `GetAvatar`; the bytes are kept only if their SHA-256 is the hash, then `Event::AvatarReady` redraws. A missing or mismatched picture is not asked for again in that session.
- **The client shrinks any PNG or JPEG to a centred 128 px square** before upload, as PNG, or as JPEG at quality 85 when the PNG would pass the server's 64 KiB cap. Decoding and encoding use the `image` crate's `png` and `jpeg` features.
- **A rename resends the current picture's hash**, since `SetProfile` sets both fields at once.
- **Settings has a Profile page** (name, picture, remove). The server takes one profile change per 10 s and the page says so.

## 2026-10-30 M11 client packaging

- **ffmpeg is built from source as LGPL shared libraries** by `packaging/ffmpeg/build.sh`: release 8.1.3, checksum pinned (it matches Homebrew's), autodetection off so nothing unexpected links in, and no filters, devices, muxers, demuxers or protocols (throcc only encodes, decodes and scales), and only dav1d, OpenH264 (if present), VideoToolbox on macOS, and NVENC, QSV and AMF where their headers exist. The script fails if the configured build is GPL.
- **macOS ships unsigned for now**, as the human chose: `packaging/macos/bundle.sh` builds `throcc.app` with the ffmpeg and dav1d dylibs in `Contents/Frameworks`, rewritten to `@rpath`, ad hoc signed, and packed into `throcc.dmg`. Gatekeeper asks on first open (right-click, Open). A Developer ID and notarisation slot in later at the `codesign` lines.
- **The minimum macOS is 14**, which the system share picker needs.
- **The Arch package carries its own LGPL ffmpeg** in `/usr/lib/throcc`, found through an `RPATH` (`--disable-new-dtags`, since a `RUNPATH` does not cover the libraries ffmpeg loads itself), instead of depending on Arch's `ffmpeg`, which is a GPL build and may move to another major version than `rsmpeg` targets. It uses the system `dav1d`, and builds with `rustup` so the pinned toolchain applies.
- **Windows encodes with Media Foundation's hardware encoders when NVENC, QSV and AMF aren't built**: `hevc_mf` and `h264_mf` with `hw_encoding=1`, the `display_remoting` scenario and CBR. They need no SDK and use whichever GPU is present. The encoder drains its output and retries once when a frame is refused, which these asynchronous encoders do.
- **The Windows package is built on CI**: dav1d 1.5.4 with meson and MSVC, ffmpeg with `--toolchain=msvc` from an MSYS2 shell (with MSYS2's `link.exe` removed so MSVC's is found), NVENC headers n13.0.19.0, then `throcc.exe` zipped with the DLLs. ffmpeg installs its import libraries next to the DLLs in `bin`, so that is what `FFMPEG_DLL_PATH` names. QSV (libvpl) and AMF headers are not built in yet.
- **Linux and Windows packages pass pre-generated ffmpeg bindings** (`FFMPEG_BINDING_PATH`, from `packaging/ffmpeg/bindings.sh` with bindgen-cli 0.72). `rusty_ffmpeg` 0.16.7 is locked to bindgen 0.71.1, which with libclang 22 (Arch, Fedora) emits `AVFormatContext` as an opaque struct, so `rsmpeg` fails to compile. bindgen 0.72.1 with the same options gets it right. The script mirrors `rusty_ffmpeg`'s options and leaves out the same platform headers. macOS keeps the pkg-config path, since Apple's clang is unaffected. A newer `rsmpeg` built on `rusty_ffmpeg` 0.17 would make this unnecessary.

## 2026-10-30 CI layout

- **Two workflows.** `ci.yml` (lint, tests on three systems, the video tests, netem) runs on every push. `packaging.yml` (deployment test, static servers, macOS app, Arch package, Windows zip) runs on tags, when packaging files change, every Monday, or by hand, because each of its jobs builds ffmpeg and a release client.
- **A newer push cancels the older run** of the same workflow and branch.
- **ffmpeg builds live in `deps/`, outside `target/`, and are cached**: on macOS keyed on the build script and Homebrew's dav1d version, on Windows (with dav1d, the NVIDIA headers and the generated bindings) keyed on the build scripts.
- **ffmpeg is patched** (`packaging/ffmpeg/patches/`): in 8.1.3 a Media Foundation encoder that finds no hardware MFT shuts Media Foundation down and uninitialises COM on its error path, then `mf_close` does both again. The patch frees only when an MFT was created, and should go upstream.
- **Encoder options are set on the codec context, never passed as a dictionary to `open`.** rsmpeg 0.18's `AVCodecContext::open` keeps its own pointer to the dictionary when `avcodec_open2` fails, but ffmpeg's `av_opt_set_dict2` has already freed it and put a new one in its place, so rsmpeg frees it again. Any encoder failing after that point (NVENC without an NVIDIA driver, Media Foundation without a hardware encoder) crashed the process: SIGSEGV on Arch with an AMD GPU, an access violation or heap corruption on the GPU-less Windows runner. `an_encoder_that_fails_to_open_is_cleaned_up_safely` reproduces it on macOS with an invalid size. Options unknown to an encoder are skipped, as a dictionary would leave them. This should be reported to rsmpeg.

## 2026-10-31 Linux window backend

- **The client enables gpui's `wayland` backend only.** gpui's Linux windowing backends are opt-in features; without one it starts headless and shows no window, which the first Arch install did. X11 is left out by the human's choice. `tests/arch/run.sh` starts the installed app under a headless Weston with `WAYLAND_DEBUG=1` and checks it creates a toplevel titled "throcc".

## 2026-10-31 VA-API in place of QSV and AMF

- **The encoder ladder is NVENC, then VA-API on Linux, Media Foundation on Windows or VideoToolbox on macOS, then OpenH264 when it is built in.** QSV and AMF are gone: AMF on Linux needs AMD's proprietary runtime, which Mesa systems lack, and VA-API reaches both AMD (Mesa) and Intel GPUs on Linux. On Windows, Media Foundation covers Intel and AMD. This revises S4.
- **VA-API encoders take frames in GPU surfaces**: each encoder opens the default render node, keeps a pool of 8 NV12 surfaces, and every picture is uploaded into one before encoding. Rate control is CBR with `async_depth=1`; recovery is an IDR, since VA-API has no intra refresh in ffmpeg.
- **The software fallback is VP9 through libvpx** (BSD): real-time deadline, `cpu-used=8`, no lag, row multithreading on every core, `tune-content=screen`. Machines without a hardware encoder are the weak ones, and real-time AV1 in software asks too much of them. Receivers decode it with ffmpeg's own `vp9` decoder. A 720p test pattern takes about 5 ms a frame on an M-series Mac in release. It runs constant bitrate (`minrate` equal to the bitrate, which libvpx needs for CBR). It has no size or frame-rate cap: where the CPU cannot keep up, the capture ring drops frames, so the share gets fewer frames a second rather than more delay, while `ShareConfig` still announces the preset's rate.
- **`Codec::Vp9` is a wire-format change after the M5 freeze, made by the human**: older clients cannot read a share announced as VP9, which is acceptable this early. It is appended last, so the existing values keep their encoding. An older client in a room with a VP9 share cannot read the room state and is disconnected, and an older server disconnects a client that announces VP9, so servers upgrade first.
- **libvpx is built in on macOS and Arch; Windows has no software fallback yet**, since libvpx would need an MSVC build in CI. Windows machines with any GPU use Media Foundation.

## 2026-10-31 Themes, room grid and pop-out

- **The client is themeable from eight colours**, as the human asked and the Appearance mockup shows. `theme.rs` holds the palette for the UI thread and mixes every other colour from it in Oklab, matching the mockup's `color-mix(in oklab, …)`. Paper switches gpui-component to its light mode. The chosen palette is saved as one line of hex values in the `appearance` file in throcc's data directory. `--theme <preset>` picks a preset for screenshots, and the mock client ignores the saved file.
- **The room is a grid of everyone.** Tiles are 16:9 and sized from the window: the column count is the one that makes them largest. A share is drawn with its image taken out of layout (absolutely positioned, `object-fit: contain`), so the picture's own size can never stretch or crop its tile; the earlier stage let it push the flex layout and cut off its bottom.
- **Our own share is not watched through the server.** Our tile offers **Preview**, which shows the local self-view.
- **A popped-out share keeps being watched;** its window closes when the share ends, we stop watching or leave. The window opens one effect later, because its first frame reads the app, which is still being updated by the click. There is no keep-on-top toggle: gpui sets a window's level only when it opens.
- **Server pages are their own screen,** apart from this computer's settings, as the human asked: profile, keys and members belong to one server; audio, appearance and known servers to the machine.
- **Clicking outside a dialog closes it**, and the Connection dialog has a Close button.


## 2026-10-31 Share state and Linux frame times

- **A presence update keeps what the room state said.** The server sends a room's state, then the sharer's presence; the presence handler rebuilt the person and dropped the share, so a share started mid-call only showed after rejoining. Presence now keeps sharing, verification, speaking and volume, and one function marks who shares: room peers that say so, and us while we share.
- **Linux video frames are stamped with CLOCK_MONOTONIC when PipeWire hands them over.** The stream's own time didn't advance for at least one compositor's screencast, so every frame carried the first one's timestamp, and viewers, which redraw on a new timestamp, kept showing the first frame. Desktop audio stays on PipeWire's time, which counts on the same clock. Frame timestamps are also forced to rise, as encoders reject repeats.
- **Profile changes report back:** the session emits saved or refused for each name or picture change, and the profile page shows a chosen picture at once.

## 2026-10-31 Profile kept on the client

- **Name and picture belong to this computer, not a server,** as the human asked. They live in `profile_name` and `profile_picture` in throcc's data directory, and the session gives them to each server when it connects and after each change. With nothing kept yet, connecting changes nothing, so a picture set on a server before this keeps showing. A server that refuses a change (its one-per-10-seconds limit) leaves the kept profile as it is; the next connect tries again.

## 2026-10-31 Hardware decoding and stream fullscreen

- **Decoding tries the GPU first:** FFmpeg's own decoder for the codec with a hardware device (VideoToolbox on macOS, VA-API then CUDA on Linux, D3D11VA then CUDA on Windows), and the software decoders only when no device opens. FFmpeg's default format choice picks the device's format, and the decoded surface is copied back to NV12. A hardware decoder that fails on a unit is replaced by software, which decodes that same unit, since FFmpeg's own AV1 decoder has no software path. The ffmpeg build enables NVDEC wherever it enables NVENC.
- **Fullscreen means the share fills the window,** not just a fullscreen window around the normal UI, and looks like a video player rather than the app, as the human asked: black around the picture in every theme, who and what at the top and the buttons at the bottom, each over a dark fade. They show for three seconds after the pointer moves; Escape leaves. Pop-outs do the same. Every stream tile has the button, in the grid too.
- **Settings screens share the main screen's top bars:** a back arrow and "Settings" or "Server settings" over the page links, and the page's title over the page. Leaving a server stays on the main screen's header only.

## 2026-10-31 No slow-sender verdict

- **The connection verdict no longer blames the sender's upload when a share arrives below its set bitrate,** as the human asked. A still desktop encodes far below the set rate and fills up again when something moves, so the gap alone was a false alarm. The per-track table still shows set and arriving rates side by side.

## 2026-10-31 Shares keep their screen's shape; no presets

- **A share is encoded at its screen's aspect ratio,** as the human asked, so a 16:10 MacBook no longer arrives with black bars baked in. The chosen resolution is a 16:9 box; the sender fits the screen's shape in it and never goes above the screen's own size. macOS asks ScreenCaptureKit for that shape using the picker's content size, Windows sizes its GPU scaler from the capture item, and Linux already delivers the native size. The sender waits up to 2 s for the first frame and opens the encoder at its fitted size, falling back to the box. `ShareConfig` already carried width and height, so the wire format is unchanged. A window that changes shape mid-share is still fitted with bars into the first size.
- **Presets gave way to resolution, frame rate and a stepped bitrate,** as the human asked. The bitrate cap per resolution and frame rate is a judgement call: past it, screen content shows no visible gain. The slider runs over the offered steps' indices and is rebuilt when the cap changes, because gpui-component's slider can't change its range.

## 2026-10-31 Nightly Arch package

- **Every push to `main` publishes the Arch package to a rolling `nightly` release,** so the human can update without compiling. The Arch job moved out of `packaging.yml` into `nightly.yml`, which runs on every push to `main` that touches more than docs and cancels an older run. `tests/arch/run.sh` takes an optional directory to copy the checked package into, and the workflow uploads it under one fixed name, replacing the last one. The release's tag stays where it was first made; its notes name the commit.
- **The package is versioned by commit** (`0.1.0.r<commits>.g<hash>`, from `pkgver()`), so pacman sees each nightly as an upgrade.
- **Install by downloading first, then `pacman -U` the file:** pacman wants a signature for packages given by URL, and these aren't signed.
- **Voice activation gates on the processed mic level in dBFS, not on speech probability.** The human chose a plain loudness threshold, set with a slider over a live level meter, like other voice apps. AGC2's voice-activity detector and its crates are gone. Keyboard noise loud enough to cross the threshold opens the gate; noise suppression keeps most of it below.
- **The server has a name and picture that admins set** (`ServerProfile` in `Welcome`, `Request::SetServerName` and `Request::SetServerAvatar`, `Event::ServerProfileChanged`). Name and picture are two requests that each change one field, so two quick edits can't undo each other. The human approved this change to the frozen wire format. It is additive, but `Welcome` gained a field, so `PROTOCOL` went to 2 and older clients get a clean version mismatch instead of a garbled welcome. The picture travels like a profile picture: uploaded with `SetAvatar`, fetched with `GetAvatar`, and the server refuses a hash it doesn't hold. A blank name clears it, and clients then show the address.

## 2026-10-31 Vendored gpui for NV12 surfaces on Linux and Windows

- **`gpui-pre`, `gpui-pre-wgpu` and `gpui-pre-windows` 0.3.10 are vendored in `third_party/`** and used through `[patch.crates-io]`, with a surface element that takes NV12 frames off Apple platforms and a renderer for it in wgpu (Linux) and D3D11 (Windows). Received shares are drawn from a Y and a CbCr texture per stream, written only when a new frame arrives, instead of a BGRA image converted on the UI thread and re-uploaded to the atlas every frame. `third_party/README.md` lists every change.
- **Why:** the BGRA path cost 37 ms of UI-thread CPU per 2560×1440 frame (conversion plus 4-byte upload), more than a 60 fps frame lasts; the NV12 path costs 2 ms (measured with `bench_bgra_image_against_nv12_surface` on lavapipe). S3 deferred this patch until profiling asked for it; a 1440p60 share now does.
- **The API:** `gpui::surface(Nv12Frame)`, where `Nv12Frame` names its stream (`SurfaceId`, from gpui), a generation that changes with the pixels, the size and an `Arc<dyn Nv12Planes>` that hands out the two planes. throcc wraps its `Arc<VideoFrame>`, so a frame is never copied on the UI thread. Renderers drop a stream's textures when a frame's scene no longer draws it. The shader is full-range BT.709, as throcc captures and encodes; upstream's wgpu shader assumed BT.601.
- **macOS keeps the BGRA image path** for now. The Metal renderer lives in `gpui-pre-apple`, which is not vendored, and reads only a `CVPixelBuffer`; the NV12 variant therefore exists only off Apple platforms.
- **Tests:** `crates/throcc-client/tests/surface.rs` renders through `gpui-pre-wgpu`'s headless renderer on lavapipe (a Linux-only dev-dependency on its `test-support` feature, which brings `futures-concurrency` and `postage` into the lockfile for tests only). `tests/surface/run.sh` screenshots the mock room under headless sway. `tests/surface_windows.rs` draws in a real window on the CI runner's WARP adapter and reads it back.
- **Refreshing on a gpui update:** download the new `gpui-pre`, `gpui-pre-wgpu` and `gpui-pre-windows` (`cargo fetch` after bumping the pins, then copy `~/.cargo/registry/src/*/gpui-pre*-<version>`), replace the three directories with the unmodified crates and commit that alone, then re-apply our commits on top (`git log -p -- third_party/` lists them; `git cherry-pick` usually applies) and update `third_party/README.md`. If upstream has gained an NV12 or YUV surface on Linux and Windows, drop the vendored copies and the patch section instead. Check that `Cargo.lock` lists the three without a `source`, and that the tests above still pass.
- **Alternatives:** a zed git dependency (breaks gpui-component, which needs `gpui-pre`); converting to BGRA on a worker thread (still 4 bytes a pixel to upload and an atlas churn per frame); uploading NV12 as an image and converting in a custom shader (gpui has no custom shaders).
- **The server image is published to `ghcr.io/nylyk/throcc`** by the `image` workflow on every push to `main` (`latest` and `sha-<commit>`) and on `v*` tags (`<version>`). amd64 and arm64 build on native runners, each pushed by digest, and a last job ties them into one multi-platform tag; QEMU would make the arm64 build many times slower. Buildx caches layers in the Actions cache, so the cargo-chef dependency layer is reused until dependencies change.

## 2026-10-31 One Save per profile page

- **The profile and server pages stage the name and picture and send nothing until Save.** Choosing or removing a picture only previews it; Save sends every change in one go (`Command::SetProfile`, `Command::SetServerProfile`) and stays grey while nothing differs or a save is in flight. One save is one `SetProfile`, so the server keeps its single limit of one profile change per 10 s.
