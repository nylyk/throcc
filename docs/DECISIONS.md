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
