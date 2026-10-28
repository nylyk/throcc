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
- **`SetMedia` only bumps nothing**: a share change re-sends `RoomState` at the same epoch, so peers learn the codec without rekeying.
- **The X25519 encryption key's private half lives in a 0600 file** next to the keystore, with its creation time, and is replaced after 30 days. The identity key uses the OS keychain. Alternative: both in the keychain.
- **A kicked session's writer sends `Kicked`, finishes the stream and waits up to 2 s for the client to read it before closing.** Closing at once dropped the event.
