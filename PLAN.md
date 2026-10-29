# throcc — implementation plan

throcc is a self-hosted voice and screen-share app in the spirit of TeamSpeak, written in Rust:

- Persistent rooms with low-latency voice.
- Screen share up to 1440p60 with its system audio, always relayed by the server and never peer to peer.
- Mandatory end-to-end encryption for all media.
- One server binary. Joining takes an address and a short invite code.

This file is the whole brief. Read it end to end before writing code, then work milestone by milestone.

---

## 0. How to work

You are Claude Code on Opus 5.5 and you can spawn subagents. Use them deliberately.

**Ground rules**

- Work in milestone order. Each milestone has tasks, a *Done when* list and notes on what can run in parallel. A milestone is done only when every *Done when* item is demonstrated by a test or a command you ran.
- Tick the checkboxes in this file as you go, and commit at least once per milestone with a message naming it.
- Record every non-obvious decision in `docs/DECISIONS.md`: date, decision, why, alternatives.
- **The media wire format and the crypto are frozen at the end of M5.** Changing them afterwards needs a human decision. Everything else can move.
- Never weaken a security property in this file to make a test pass. If one seems wrong, stop and ask.
- Don't add dependencies with GPL or AGPL licences. `cargo deny` enforces this from M0.
- Prefer small, boring code. No framework where a function will do.

**Subagents**

| Use a subagent for | How |
| --- | --- |
| Independent platform backends (capture per OS, hotkeys per OS) | One subagent per platform, `isolation: "worktree"`. Give it the trait, the relevant spec section and the *Done when* list |
| Research spikes (crate maturity, an OS API's behaviour) | A read-only subagent that returns findings and a recommendation, nothing else |
| UI screens after the shell exists | One subagent per screen, pointed at its mockup file and the tokens in §5.13 |
| **Review of crypto, protocol and the SFU** | A fresh subagent that did not write the code. Brief it with the spec section and tell it to try to break the code: nonce reuse, replay, missing checks, panics on hostile input. Required at the end of M3, M5 and M6 |
| Test harnesses (netem scenarios, fuzz targets) | A subagent per harness, in parallel with the feature work |

The main agent owns `throcc-proto`, all cross-crate interfaces and every merge. Subagents never change `throcc-proto` without the main agent's approval, because every other crate depends on it.

**Ask the human only at the decision points in §7**, or when real hardware is needed (macOS and Windows capture, GPU vendor checks). Batch those requests and keep working on what doesn't depend on them.

**Machines.** The human develops on a MacBook Pro M4 (macOS), runs Arch Linux desktops and Debian servers. CI covers `ubuntu-latest`, `macos-14` and `windows-2022`. Anything that needs a display, GPU or audio device on macOS or Windows gets a test the human can run with one command, and is listed in `docs/MANUAL_TESTS.md`.

---

## 1. Product

**Goals**

- Persistent rooms with low-latency voice, push-to-talk and voice activation.
- Screen share up to 1440p60 with system audio, relayed by the server.
- Viewers choose which shares to watch. Nobody pays bandwidth for a share they aren't watching.
- All media end-to-end encrypted. The server holds no media key.
- Join with an address and an invite code. Run a server with one binary and one data directory.

**Non-goals**

- Web or mobile clients.
- Rooms of more than about 20 people.
- Recording, transcoding or simulcast.
- A TCP fallback.
- Text chat (not now; the crypto section notes what it would need).

---

## 2. Architecture

```
 Sender client                      Server                         Receiver client
┌──────────────────────┐    ┌──────────────────────────┐    ┌──────────────────────────┐
│ Key manager          │───▶│ Key relay (opaque blobs) │───▶│ Key manager (HPKE open)  │
│ Capture + encode     │    │ Rooms, auth, presence    │    │ Decode + present (GPU)   │
│ Fragment + seal ─────┼───▶│ Forward: ownership check,│───▶│ Open + reassemble        │
│ (AES-GCM/datagram)   │    │ per-receiver queues      │    │ (dedup, 50 ms reorder)   │
└──────────────────────┘    └──────────────────────────┘    └──────────────────────────┘
        one QUIC connection per client: a control stream for keys/state, datagrams for media
```

The server routes ciphertext by its plaintext header and relays key blobs it cannot open.

**Crates**

| Crate | Depends on | Holds |
| --- | --- | --- |
| `throcc-proto` | No async runtime, no media, no I/O | Messages, ids, frame header, framing, fingerprints |
| `throcc-crypto` | `throcc-proto` | PAKE, identity signatures, HPKE wrapping, media AEAD, safety numbers. No I/O. Feature `media` is off in the server |
| `throcc-server` | proto, crypto (no `media`), quinn, rusqlite | Sessions, auth, rooms, SFU, key relay, storage |
| `throcc-client-core` | proto, crypto, quinn, ffmpeg (`rsmpeg`), cpal, sonora, ropus, neteq | Connection, keystore, key manager, media pipelines |
| `throcc-cli` | client-core | Headless test client |
| `throcc-client` | client-core, gpui, gpui-component | UI and the video surface |

The server's dependency tree contains no codec, no capture library and no media AEAD. A CI check asserts this with `cargo tree`.

**Layout**

```
throcc/
├── Cargo.toml                      # workspace, all versions pinned here
├── rust-toolchain.toml
├── deny.toml
├── CLAUDE.md                       # conventions for future sessions
├── PLAN.md                         # this file
├── docs/  DECISIONS.md  MANUAL_TESTS.md
├── design/mockups/                 # exported .dc.html mockups (see §5.13)
├── tests/netem/                    # network-impairment scenarios
└── crates/
    ├── throcc-proto/       lib.rs ids.rs messages.rs frame.rs framing.rs fingerprint.rs
    ├── throcc-crypto/      pake.rs identity.rs hpke.rs media.rs safety.rs
    ├── throcc-server/      main.rs lib.rs identity.rs tls.rs session.rs auth.rs
    │                       database.rs rooms.rs sfu.rs egress.rs keyrelay.rs permissions.rs
    ├── throcc-client-core/ lib.rs connection.rs keystore.rs keys.rs state.rs health.rs
    │   └── media/          mod.rs fragment.rs transform.rs audio.rs share_audio.rs
    │       └── video/      encode.rs decode.rs reorder.rs present.rs
    │           └── capture/ mod.rs linux.rs macos.rs windows.rs
    ├── throcc-cli/
    └── throcc-client/      main.rs app.rs theme.rs video.rs views/
```

---

## 3. Milestones at a glance

| # | Milestone | Depends on | Can run alongside |
| --- | --- | --- | --- |
| M0 | Workspace, CI, conventions | – | – |
| M1 | QUIC transport and server identity | M0 | Spikes S1–S4 |
| M2 | Control protocol and CLI | M1 | UI shell against a mock client (M8a) |
| M3 | Identity, PAKE enrollment, roles | M2 | M8a |
| M4 | Rooms and presence | M3 | M8a |
| M5 | Media wire format and E2EE (**freeze**) | M4 | M7a (audio pipeline offline), M9a (encode/decode offline) |
| M6 | SFU egress and network tests | M5 | M7, M9a |
| M7 | Voice end to end | M5 | M6, M9a |
| M8 | gpui client wired to core | M4 (shell from M2) | M6, M7 |
| M9 | Screen share video | M6, M8 | M10 prework |
| M10 | Share audio and sync | M7, M9 | – |
| M11 | Packaging and release | all | – |

"a" milestones are the parts that need no network: pipelines that run against files and synthetic input, and a UI shell against a mock `Client`.

---

## 4. Spikes (start during M1, one read-only subagent each)

Each spike ends with a short entry in `docs/DECISIONS.md`.

- [x] **S1 PAKE crate.** Is there a maintained CPace (Ristretto255) crate? If not, confirm RustCrypto `spake2` supports what §5.3 needs: key confirmation, associated data, symmetric roles. Recommend one.
- [ ] **S2 Audio crates.** Confirm `sonora`, `ropus` and `neteq` build on all three targets. Confirm `neteq` can report the timestamp it is currently playing, which §5.10 needs. If it can't, propose the smallest patch.
- [x] **S3 gpui video surface.** On macOS `paint_surface` takes a `CVPixelBuffer`. Find the current state of surface or YUV rendering on Linux (wgpu) and Windows (D3D11) upstream, including zed discussion #45866. Recommend: carry a small patch on a pinned tag, or use what upstream has.
- [x] **S4 Encoder recovery.** Which hardware encoders expose intra refresh through ffmpeg: NVENC, AMF, QSV, VA-API per vendor, VideoToolbox? Assume VideoToolbox does not until shown otherwise.

---

## 5. Specification

### 5.1 Transport

One QUIC connection per client (quinn). A single bidirectional stream carries control. DATAGRAM frames (RFC 9221) carry media. The listener is UDP/8476 with ALPN `throcc/1` and no TCP fallback.

| Setting | Value | Why |
| --- | --- | --- |
| Keep-alive | 5 s | Holds NAT bindings while idle in the lobby |
| Idle timeout | 30 s | Detects a dead peer fast enough to retract presence |
| Datagram send buffer | ~100 ms at the configured send rate, clamped to 64–512 KiB | quinn drops the oldest queued datagram when full. A large buffer turns congestion into seconds of delay |
| Congestion control | Cubic. BBR behind a flag for measurement | Cubic is quinn's stable default |
| Datagrams | Required | The handshake fails with a clear message if the peer offers none, or less than the payload floor |

**Server identity.** `server_key` (Ed25519) is created at mode `0600` in the data directory on first start. The self-signed certificate is derived from it with `rcgen` on every start and never stored. Clients pin the SPKI hash. The server prints its fingerprint at startup. Losing `server_key` changes the server's identity for every client.

**Client pinning.** A custom `rustls` verifier compares the SPKI hash against `known_servers` and checks nothing else: no chain, no name, no expiry. SNI carries a fixed placeholder.

### 5.2 Control protocol

Each frame is a `u32` length plus a postcard body, capped at 1 MiB. The length is checked before allocating, and an oversized frame closes the connection. Every request carries a `u32` id that the response echoes. Events carry no id. The client may have many requests in flight and applies replies in arrival order on the one task that reads the stream. An unknown reply id is a protocol error and closes the connection. Id counter wrap is fatal.

```rust
// handshake (server speaks first)
struct ServerHello { nonce: [u8; 32], protocol: u16 }

enum Hello {
    Auth   { pubkey: [u8; 32], client_nonce: [u8; 32], want_room: Option<RoomId>, signature: [u8; 64] },
    Enroll { code_id: String, pubkey: [u8; 32], client_nonce: [u8; 32], pake_a: Vec<u8> },
}
// Enroll continues: server -> PakeReply { pake_b, confirm_b }, client -> PakeFinish { confirm_a, signature }

enum AuthResult {
    Ok { me: UserId, role: Role, rooms: Vec<Room>, presence: Vec<Presence>, placed: Placed },
    Err(AuthError),   // UnknownKey, BadSignature, BadInvite, ProtocolMismatch, Banned, RateLimited
}

enum ClientMessage { Request { id: u32, req: Request } }
enum ServerMessage { Response { id: u32, res: Response }, Event(Event) }

enum Request {
    SetRoom(Option<RoomId>),                                       // -> Placed
    SetProfile { name: String, avatar: Option<Hash> },             // full replace, 1 per 10 s
    SetAvatar(Vec<u8>),                                            // -> AvatarHash, max 64 KiB
    GetAvatar(Hash),                                               // -> Avatar
    SetEncKey { key: [u8; 32], sig: [u8; 64] },                    // X25519 key signed by identity key
    SetMedia(MediaState),
    SetWatching(Vec<MediaId>),                                     // share tracks this client receives
    RequestKeyframe { media_id: MediaId, kind: Recovery },
    PublishKey { epoch: Epoch, wrapped: Vec<(UserId, Vec<u8>)> },  // E2EE sender secret, one blob per peer
    CreateRoom { name: String },
    RenameRoom { room: RoomId, name: String },
    DeleteRoom(RoomId),
    CreateInvite,                                                  // -> Invite { code, expires }
    ListUsers,                                                     // admin only
    SetRole { user: UserId, role: Role },
    RemoveUser { user: UserId, cascade: bool },
}

enum Response { Ok, Err { code: ErrorCode, message: String }, Placed(Placed),
                Invite { code: String, expires: u64 }, Users(Vec<User>), AvatarHash(Hash), Avatar(Vec<u8>) }

enum Event {
    PresenceChanged(Presence), PresenceEnded(UserId),                      // everyone connected
    RoomCreated(Room), RoomRenamed { room: RoomId, name: String }, RoomDeleted(RoomId),
    RoomState { room: RoomId, epoch: Epoch, peers: Vec<RoomPeer> },        // one room
    SenderKey { from: UserId, epoch: Epoch, wrapped: Vec<u8> },            // one recipient
    KeyframeWanted { media_id: MediaId, kind: Recovery },                  // the sender only
    LinkStats { tracks: Vec<TrackStats> },                                 // one receiver, every 2 s
    UserAdded(User), RoleChanged { user: UserId, role: Role }, UserRemoved(UserId),
    InviteBurned { code_id: String },                                      // the invite's minter
    Kicked { reason: String },
}

struct Placed     { room: Option<RoomId>, epoch: Epoch, tracks: Option<Tracks>, peers: Vec<RoomPeer> }
struct MediaState { mic: bool, deafened: bool, share: Option<ShareConfig> }
struct ShareConfig { audio: bool, kbps: u32, codec: Codec, width: u16, height: u16, fps: u8 }
enum   Codec      { Av1, Hevc, H264 }                     // announced by the sender, never negotiated
enum   Recovery   { Refresh, Idr }
struct Share      { video: MediaId, audio: MediaId }
struct Tracks     { mic: MediaId, shares: Vec<Share> }    // one share today; a Vec so a second monitor is additive
struct Presence   { user: UserId, name: String, avatar: Option<Hash>, room: Option<RoomId>,
                    mic: bool, deafened: bool, sharing: bool }
struct RoomPeer   { user: UserId, tracks: Tracks, share: Option<ShareConfig>,
                    identity_key: [u8; 32], enc_key: [u8; 32], enc_key_sig: [u8; 64] }
struct TrackStats { media_id: MediaId, forwarded: u32, dropped_units: u32 }
```

Avatars travel on the control stream, capped at 64 KiB. Clients fetch them lazily by hash for rows on screen and cache them across sessions.

### 5.3 Identity and enrollment

**Client key.** An Ed25519 identity key is generated on first launch and stored in the OS keychain (Keychain, Secret Service, Windows Credential Manager), with a `0600` file as fallback. A separate X25519 encryption key is signed by the identity key and rotated every 30 days (see §5.7).

**Invite codes.** Format `K7Q-M2XPD`: a 3-character public lookup id and a 5-character secret, Crockford base32 (`O`→`0`, `I`/`L`→`1`, case-insensitive, dash optional).

- Secret space: 32⁵ ≈ 33.5 million.
- Each invite allows **3 failed PAKE runs**. On the third failure it is deleted and the minter receives `InviteBurned`.
- Unknown ids get the same refusal as a wrong secret. They are rate-limited per source address (per /64 on IPv6) and logged. There is no global lockout.
- An invite lives 24 h and is single use. Members can mint 3 per rolling 24 h, counted as live invites plus enrollments they caused in the last 24 h. Admins are uncapped.
- The server stores the secret in plaintext for at most 24 h, because a balanced PAKE needs it. Anyone with the database also has `server_key`.

**Enrollment** (first connect, no pin yet):

1. The client completes TLS accepting any certificate, and records the server's SPKI hash and the TLS exporter value (`export_keying_material`, label `"throcc/exporter/1"`).
2. The server sends `ServerHello`.
3. The client sends `Enroll { code_id, pubkey, client_nonce, pake_a }`.
4. The server runs the PAKE with that invite's secret and replies `pake_b` plus `confirm_b`. `confirm_b` is a MAC under the PAKE key over the transcript: both nonces, `code_id`, `pubkey`, the SPKI hash and the TLS exporter.
5. The client verifies `confirm_b`. A MITM fails here, because its TLS exporter differs from the real server's. On success the client sends `confirm_a` and an Ed25519 signature over the transcript.
6. The server verifies both, inserts the user with `invited_by`, deletes the invite, and returns `AuthResult::Ok`. The client stores the pin.

A MITM gets one online guess per run, which also uses one of the invite's 3 tries. It can neither learn the secret offline nor relay the exchange.

**Returning clients** sign `"throcc/auth/1" || server_nonce || client_nonce || tls_exporter` with their identity key.

**Pin mismatch** is a hard failure that shows both fingerprints. There are two ways out: re-enroll with a fresh invite (the PAKE proves the new server, and the existing key is re-bound), or explicitly trust the new key with a warning.

**Bootstrap.** With an empty allowlist the server logs a bootstrap invite on every start, replacing the previous one. The first enrollment becomes Admin.

### 5.4 Rooms, presence, roles

- Membership is one value per connection, `Option<RoomId>`, changed only by `SetRoom` in one transaction. `None` is the lobby, and clients start there. There is no default room and no auto-join. A reconnecting client may pass `want_room`.
- In the lobby a client sees all rooms and everyone in them. It publishes no presence and opens no capture device.
- Entering a room returns `Placed` in one round trip. Mic and share start off. A failed `SetRoom` leaves the client where it was. Deleting an occupied room moves its occupants to `None`.
- **Media ids.** Every membership change allocates fresh ids (mic, share video, share audio) from one persisted `u32` counter. Ids are never reused. Exhaustion is a fatal startup error.
- **Epoch.** Each room has a persisted `u32` epoch, bumped on every membership change. It selects E2EE keys.
- **Presence.** `PresenceChanged` upserts the whole record and `PresenceEnded` removes it. Both go to every connected client. `RoomState` sends the full peer list to one room. Who is speaking is computed locally from decoded audio and is never sent. The allowlist reaches admins only, via `ListUsers`.

| Role | Rank | Can |
| --- | --- | --- |
| User | 0 | Join rooms, mint 3 invites a day |
| Manager | 1 | Create, rename and delete rooms |
| Admin | 2 | Mint invites without a cap, list users, change roles, remove users |

You act only on strictly lower ranks. `RemoveUser` closes the user's live connection. `cascade` also removes everyone they invited, transitively. Without cascade, their invitees' `invited_by` is cleared.

### 5.5 Media wire format (frozen at M5)

**Header**: 13 bytes, fixed width, big endian, hand-rolled (never postcard).

| Bytes | Field | Notes |
| --- | --- | --- |
| 0–3 | `media_id: u32` | One track of one membership session, never reused |
| 4–7 | `epoch: u32` | Room generation, selects the key |
| 8–11 | `seq: u32` | Per `media_id`, never resets, wrap is fatal |
| 12 | `flags: u8` | bit0 keyframe, bit1 first fragment, bit2 last fragment, bit3 discardable, bits 4–7 reserved |

The server reads all four defined bits and ignores reserved ones. Clients reject unknown bits. `discardable` marks a frame nothing later references.

**Payload.** Everything after the header is AEAD ciphertext (§5.7). Inside it, the first fragment of every access unit starts with a 4-byte **media timestamp**: 48 kHz for audio, 90 kHz for video. The clock runs in real time from track start, including while muted or gated. Every audio packet is one access unit. Timestamps wrap with serial-number arithmetic (RFC 1982).

**Size budget.** The datagram size is a compile-time constant, never read from a live `max_datagram_size()`, because MTU discovery can raise that mid-call.

| Item | Bytes |
| --- | --- |
| DATAGRAM payload floor (1200-byte UDP, pinned quinn version and CID length) | 1162 (re-measure and assert in M1) |
| Header | −13 |
| AEAD tag | −16 |
| **Ciphertext body per datagram** | **1133** |
| Media timestamp, first fragment only | −4 |

**Fragmentation.** An access unit is cut into consecutive `seq` values with first and last flags. A receiver assembles an unbroken run from first to last and discards the unit on any gap. Fragments are `Bytes::slice`s of the encoder's buffer.

**Dedup window.** A 2048-bit bitmap per `media_id` behind the highest `seq` seen. Anything already seen or older than the window is dropped. The window also gives per-track loss counts and acts as the replay check.

**Sender queueing**

- Two queues feed one send task: audio first, then video.
- Video is enqueued in whole access units. A unit that doesn't fit the latency budget is dropped whole, before `seq` is assigned.
- After dropping a reference frame, the sender forces its own recovery (next frame IDR, or restart the intra-refresh sweep). Dropping a `discardable` frame needs no recovery.
- Every local drop is counted.
- quinn already paces to the congestion window, so the app does no pacing of its own unless measurement asks for it.

**Realtime rule.** `cpal` callbacks and capture callbacks never block, allocate, lock or await. Every boundary between them and tokio is a bounded channel with `try_send`, drop on full, and a counter. No panics can cross FFI: no `unwrap`, no indexing, no allocation in those callbacks.

### 5.6 SFU

The server never reads past byte 13.

**Ingress** (one loop per connection):

1. Parse the header.
2. Ownership check: `media_id` must be one of this connection's current tracks. On failure, drop and count.
3. Look up subscribers in an `arc-swap` forwarding snapshot, rebuilt on membership and subscription changes.
4. `try_send` to each subscriber's egress task. Never await a receiver.

**Subscriptions.** Everyone in a room receives every mic track except deafened clients, who receive no audio. Share video and audio are **opt-in per viewer** via `SetWatching`. Starting to watch sends an `Idr` request on the viewer's behalf.

**Egress** (one task per receiver):

- An audio queue that always drains first, and a video queue of whole access units grouped by the first and last flags.
- Feeds quinn with `send_datagram_wait` and a 64 KiB quinn buffer, so the app queue decides what gets dropped.
- **Budget: 150 ms of queued video.** Over budget, drop in order: discardable units not started, then the oldest whole units not started. A unit that is partly sent is always finished.
- The receiver sees the `seq` gap and asks for recovery through the normal path.
- Audio is never dropped by policy.

**Keyframe aggregation.** Loss sends `Refresh`. A new viewer sends `Idr`. The server forwards **at most one `KeyframeWanted` per second per `media_id`**, however many receivers asked, and `Idr` beats `Refresh` within a window. A sender without intra refresh treats `Refresh` as `Idr`.

**Capacity.** Server egress is roughly the sum of bitrate × viewers. An 8 Mbps share watched by 10 people is 80 Mbps out.

### 5.7 End-to-end encryption

All media is encrypted end to end, with no plaintext mode.

| Key | Type | Lifetime | Held by |
| --- | --- | --- | --- |
| Identity | Ed25519 | Per install | The user. The server stores the public half |
| Encryption key | X25519, signed by identity | 30 days, and on reinstall | The user. The server stores and relays the public half and signature |
| Sender secret | 32 random bytes | One member, one epoch | The sender and the room's current members |
| Track key | AES-128-GCM key = HKDF-SHA256(sender secret, `"throcc/media/1" \|\| media_id`) | One track, one epoch | Derived |

**Media AEAD** (`media/transform.rs`): AES-128-GCM per datagram via `ring` or `aws-lc-rs`. Nonce = header bytes 0–11. AAD = all 13 header bytes. Plaintext = everything after the header, including the media timestamp. The tag uses the 16 reserved bytes. Uniqueness holds because a track key covers one `media_id` in one epoch and `seq` never repeats within a `media_id`.

**Key distribution**

1. A membership change bumps the epoch. `RoomState` carries the new epoch and each peer's identity key, encryption key and signature.
2. Each member verifies every peer's encryption-key signature against that peer's **pinned** identity key, then generates a new sender secret.
3. It wraps the secret to each peer with **HPKE auth mode** (DHKEM-X25519, HKDF-SHA256, AES-128-GCM), with info = `room || epoch || from || to`. Auth mode proves the sender.
4. It sends `PublishKey`. The server checks that the epoch is current and every recipient is in the room, then delivers `SenderKey` to each recipient.
5. The sender switches to the new epoch immediately. Receivers buffer ciphertext for a missing key for up to 500 ms per track, then count a decrypt failure. Old-epoch keys are kept 2 s, then zeroized. Keys are never written to disk.

**Contacts.** The client pins each user's identity key per server on first sight, warns loudly when it changes, and supports out-of-band verification with a 60-digit safety number derived from both identity keys (12 groups of 5 digits, Signal-style construction). A member hidden from you never receives your key.

**What the server can still do:** see metadata (who, which room, when, codec, bitrate, packet sizes, frame boundaries); add a member, who is visible to everyone; substitute keys, which is caught by pinning; deny service. Not end-to-end encrypted: profiles, avatars, room names, the allowlist.

Text chat or rooms well beyond 20 people would justify MLS (`openmls`). Until then, pairwise HPKE with a fresh secret per epoch is enough.

### 5.8 Voice

**Send:**

1. `cpal` input, resampled to 48 kHz with `sonora-common-audio` only if the device refuses 48 kHz.
2. `sonora` on 10 ms frames: high-pass, AEC3, noise suppression (default moderate), AGC2. Two frames per Opus frame.
3. Gating: push-to-talk, or voice activation using sonora's VAD with a 300 ms hangover. Gated audio sends nothing.
4. `ropus`: 48 kHz mono, VoIP mode, 20 ms, 32 kbps default, FEC on, DTX on. Expected-loss follows measured loss.
5. One datagram per frame.

**Receive:**

- One `neteq` per remote mic, fed `(seq, media timestamp)`.
- The `cpal` output callback pulls 10 ms per stream, applies per-user volume, sums and soft-clips.
- The final mix, including others' share audio, is the AEC reference.
- Deafened clients receive no audio from the server.

**Push-to-talk:** `CGEventTap` on macOS (needs Input Monitoring), a low-level keyboard hook on Windows, the GlobalShortcuts portal on Wayland (voice activation only where it's missing), `XGrabKey` on X11.

**Acceptance tests live in the tree:**

- ERLE above a floor once converged, against a synthetic room with a nonlinear loudspeaker.
- Near-end speech survives double-talk.
- FEC recovers a dropped frame.

The fallbacks are `webrtc-audio-processing` and `opus` bindings.

### 5.9 Screen share

**Capture** (one trait yielding an `AVFrame` already on the GPU; the `av_hwframe_ctx_init` + `av_hwframe_map` unsafe code lives only in `capture/`):

| Platform | Source | Frame |
| --- | --- | --- |
| macOS | ScreenCaptureKit, picked with `SCContentSharingPicker` | `CVPixelBuffer` |
| Windows | Windows Graphics Capture, border off where allowed | `ID3D11Texture2D` |
| Linux | xdg-desktop-portal ScreenCast + PipeWire, persisted `restore_token` | dmabuf with negotiated modifiers |

**Encode** (ffmpeg via `rsmpeg`, LGPL build, no x264 or x265):

- Pick the best hardware encoder in the order AV1, HEVC, H.264 and announce it in `ShareConfig`.
- No B-frames, no lookahead, low-latency tuning, VBV of about 2 frames.
- Recovery: intra refresh with an unbounded GOP where supported. Otherwise a 4 s GOP cap plus IDR on request.
- No hardware encoder: OpenH264 at up to 1080p30, or sharing disabled.

| Preset | Resolution | fps | Bitrate |
| --- | --- | --- | --- |
| Low | 1280×720 | 30 | 2.5 Mbps |
| Default | 1920×1080 | 60 | 6 Mbps |
| High | 2560×1440 | 60 | 10 Mbps |
| Extreme | 2560×1440 | 60 | 18 Mbps |

**Receive:**

1. Reassemble.
2. Reorder window of 50 ms: emit in `seq` order, and wait at most 50 ms for a missing unit before abandoning it and sending `Refresh`.
3. Sync hold, only when the share has audio (§5.10).
4. Decode and show the newest frame. PTS is ignored.

The renderer channel is a `watch` of a refcounted GPU handle. A frame is released only after the GPU has finished sampling it.

**Decode and present**, in order of preference:

1. Hardware decode (VideoToolbox, D3D11VA, VA-API, NVDEC), then GPU convert and scale (`scale_vt`, `scale_d3d11`, `scale_vaapi`, `scale_cuda`), then import.
2. Software decode (`dav1d`, ffmpeg HEVC/H.264), then one upload.
3. Never hardware decode, download, then upload.

Self-view takes a second reference before the encoder and never touches the network.

### 5.10 Share audio and sync

| Platform | Capture that excludes throcc's own playback |
| --- | --- |
| macOS | ScreenCaptureKit audio on the same stream, `excludesCurrentProcessAudio = true` |
| Windows | Process loopback (Windows 10 2004+): include mode on the shared window's process tree, exclude mode on throcc's own tree for a full screen |
| Linux | A private PipeWire node linked to every playback stream except throcc's own, relinked as streams appear (Vesktop's venmic is prior art) |

- Encoding: Opus music mode, 48 kHz stereo, 128 kbps, 20 ms, FEC on. No AEC, NS or AGC.
- Receive: its own `neteq`, mixed into the AEC reference.
- **Sync within a share only.** Share audio and video are stamped from one capture clock with the same zero point. Playback is audio-led: a frame is shown once its timestamp is at or behind the share audio's playout position. A share without audio skips the hold.
- Clock mapping: on macOS both use `CMTime` host time; on Linux both use PipeWire time; on Windows, convert WGC `SystemRelativeTime` and WASAPI's QPC position to one QPC zero.
- Mic and shares are never synced to each other.

### 5.11 Diagnostics

| Signal | Source | Meaning |
| --- | --- | --- |
| Local send drops | Sender queue counters plus quinn `stats()` | Your upload can't carry your bitrate |
| Server egress drops | `LinkStats` every 2 s | Your downlink can't carry what you watch |
| Path loss | `seq` gaps the server didn't cause | Loss between the server and you, or upstream |
| Late fragments | Fragments arriving after their unit was abandoned | The reorder window is too tight |
| Configured vs arriving | `ShareConfig.kbps` against measured rate | Sender uplink vs your own link |
| Decrypt failures | AEAD failures and missing keys | Key distribution broken or lagging |

The connection panel (mockup *Connection*) turns these into one plain-language verdict.

### 5.12 Client architecture

```rust
pub struct Client { commands: mpsc::Sender<Command>, events: broadcast::Sender<Event>, runtime: Runtime }
impl Client {
    pub fn connect(addr: &str, keystore: Keystore, enroll: Option<InviteCode>) -> Result<Self>; // blocks until AuthResult
    pub fn command(&self, c: Command);
    pub fn events(&self) -> broadcast::Receiver<Event>;
}
```

- `Command` and `Event` are core's own types, never `throcc-proto`'s.
- Core runs tokio on its own thread. gpui awaits the `tokio::sync` channels directly.
- Core tasks: control stream, datagram receive, one task per remote track (reassemble, decrypt, decode), egress, key manager.
- The receive loop parses the header, checks dedup and `try_send`s to the track's task, so one stalled decoder starves only its own track.
- Reconnect re-runs the handshake with `want_room`. Media ids and keys renew automatically.
- `throcc-client/video.rs` is the only place GPU handles meet gpui.
- Define a `ClientApi` trait implemented by both `Client` and `MockClient`. The UI is built against `MockClient` from M2 onward.

### 5.13 UI

The mockups are a Design canvas the human iterates on. When they are final, the human exports them into `design/mockups/`, one `.dc.html` per screen. The markup uses inline styles, so the files are the exact spec for spacing, colour and type. Treat them as the source of truth, and this section as the summary.

**Look:** dark, quiet, minimal. One accent. No gradients, no shadows beyond focus rings, no emoji.

| Token | Value | Use |
| --- | --- | --- |
| `bg` | `#0E0F11` | Window background |
| `sidebar` | `#121417` | Sidebars, nav |
| `surface` | `#16181B` | Cards, dialogs, inputs |
| `raised` | `#1D2024` | Selected nav row, subtle fills |
| `control` | `#24272C` | Secondary buttons, also the default border |
| `border-strong` | `#2A2E33` | Inputs, dialog borders |
| `text` | `#E8EAED` | Primary text |
| `text-2` | `#C9CDD2` / `#9AA0A6` | Secondary text |
| `text-3` | `#8A9097` | Captions (still ≥4.5:1 on `surface`) |
| `accent` | `#7C9CFF`, text on it `#0E0F11` | Primary buttons, selection, focus |
| `link` | `#9DB4FF` | Links, inline highlights |
| `speaking` | `#3DDC97` | 2 px ring on avatars, signal |
| `ok` | bg `#14261D`, text `#6FE3AE` | Status chips |
| `warn` | bg `#2A2114`, border `#5A4520`, text `#F2C27A` | Warnings, key changes |
| `danger` | bg `#3A1F24`, text `#FF8A8A` | LIVE badge, leave, remove |

- Type: Geist and Geist Mono (OFL, bundled with the app). Sizes 12/13/14/16/18/20/24, weights 400/500/600. Mono is used for addresses, codes, fingerprints, numbers and codecs.
- Radius 8 (rows), 10 (buttons, inputs), 12 (tiles), 14 (cards), 18 (dialogs), pills 999.
- Spacing on a 4 px grid. Touch targets at least 44 px.
- Avatars are initials on muted hues.
- Put every token in `throcc-client/theme.rs` and nowhere else.

**Screens** (mockup file → behaviour):

| Mockup | Screen | Behaviour |
| --- | --- | --- |
| `Join.dc.html` | Join | Address plus invite code. The code input upper-cases, inserts the dash and normalises Crockford characters. Lists known servers |
| `Main.dc.html` | Lobby | Sidebar: rooms with occupants, LIVE badges, mute and deafen icons, speaking rings, profile bar. Main: room cards with Join |
| `Room.dc.html` | In room | Header: E2EE badge and RTT (opens Connection). Stage: the watched share with an info chip and volume, stop-watching and fullscreen controls. Rail: other shares with **Watch**. Participant tiles. Control bar: mic, deafen, share, settings, leave |
| `Share.dc.html` | Share dialog | Source picker (uses the OS picker where the OS requires one: `SCContentSharingPicker`, the portal). Preset, share-audio switch, encoder line |
| `Settings.dc.html` | Audio settings | Devices, voice activity or push-to-talk with a live level meter and threshold, PTT key, AEC, noise suppression level, AGC, mic test |
| `Connection.dc.html` | Connection | RTT, loss each way, frames dropped for you, one plain-language verdict, per-track table, sending status |
| `Verify.dc.html` | Safety number / key changed | 12×5-digit safety number, mark verified. Key-changed warning with old and new fingerprints |
| `Admin.dc.html` | Members | Invite code with copy and remaining daily count. Members with role, invited-by and key fingerprint. Remove, with optional cascade |

Screens not mocked that you design in the same language: pin mismatch, enrollment errors (wrong code, burned invite, rate-limited), reconnecting banner, empty states, the remove-with-cascade confirm. Keep them consistent with the tokens, and add a mockup request to `docs/DECISIONS.md` if any is non-trivial.

### 5.14 Storage and packaging

```sql
users    (id INTEGER PRIMARY KEY, pubkey BLOB UNIQUE, enc_key BLOB, enc_key_sig BLOB,
          name TEXT, avatar_hash BLOB, role INTEGER, invited_by INTEGER NULL, created_at INTEGER)
rooms    (id INTEGER PRIMARY KEY, name TEXT, epoch INTEGER)
invites  (code_id TEXT PRIMARY KEY, secret TEXT, minted_by INTEGER, expires_at INTEGER, failures INTEGER)
blobs    (hash BLOB PRIMARY KEY, bytes BLOB)
counters (name TEXT PRIMARY KEY, value INTEGER)   -- next media_id
```

The data directory (`server_key` plus the SQLite file) is the only state to back up.

**Debian:** a statically linked binary plus this unit:

```ini
[Unit]
Description=throcc server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/throcc-server --data /var/lib/throcc --listen [::]:8476
DynamicUser=yes
StateDirectory=throcc
StateDirectoryMode=0700
Restart=on-failure
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

**Container:** one Dockerfile at `crates/throcc-server/Dockerfile` with stages `chef` (pinned cargo-chef), `planner`, `build`, `dev` (watcher over a bind mount) and `runtime` (`distroless/cc:nonroot`, binary only). Document `-p 8476:8476/udp` and the data volume.

---

## 6. Milestones

### M0 Workspace, CI, conventions

- [x] Cargo workspace with the six crates, `rust-toolchain.toml` pinned, all dependency versions pinned in the root `Cargo.toml`.
- [x] `deny.toml`: deny GPL and AGPL, deny unknown registries, flag duplicate major versions.
- [x] CI on ubuntu, macos-14 and windows-2022: `fmt`, `clippy -D warnings`, `test`, `deny`, plus a check that `throcc-server`'s tree has no ffmpeg, cpal or media AEAD.
- [x] `CLAUDE.md`: build commands, the crate rules above, the realtime rule, "wire format frozen after M5", where decisions go.
- [x] Empty `docs/DECISIONS.md` and `docs/MANUAL_TESTS.md`.

**Done when** CI is green on all three platforms with empty crates and the dependency-tree check fails if you add `cpal` to the server.

### M1 QUIC transport and server identity

- [x] `server_key` load-or-create at `0600`, certificate derived with `rcgen`, fingerprint printed.
- [x] quinn server and client configs with the transport settings in §5.1. ALPN `throcc/1`.
- [x] Client pinning verifier and the `known_servers` store.
- [x] Datagram assertion at handshake. Measure the real payload floor for the pinned quinn version and set the constant.
- [x] Spikes S1–S4 dispatched to subagents.

**Done when:**

- A client connects to a local server.
- Deleting `server_key` makes the next connect fail with both fingerprints shown.
- A peer with datagrams disabled fails with a clear error.
- The payload-floor constant has a test that fails if quinn changes it.

### M2 Control protocol and CLI

- [x] `throcc-proto`: ids, every message in §5.2, framing with the 1 MiB cap.
- [x] Request ids, pending map, `ServerMessage` tagging, protocol-error handling.
- [x] `throcc-cli` with subcommands: `enroll`, `connect`, `rooms`, `join`, `leave`, `invite`, `users`.
- [x] Fuzz target for framing (cargo-fuzz), run 10 minutes in CI nightly.
- [ ] **Parallel (M8a):** a subagent builds the gpui shell and the Join, Lobby and Room screens against `MockClient`.

**Done when:**

- Two CLI clients exchange requests concurrently and replies match by id.
- An oversized frame closes the connection without allocating.
- An unknown reply id closes the connection.
- Fuzzing finds no panics.

### M3 Identity, enrollment, roles

- [x] `throcc-crypto::pake` per S1. `identity.rs` for signing and verification.
- [x] Enrollment flow §5.3 with channel binding, invite storage, 3-strike burn, per-source limiter, `InviteBurned`.
- [x] Returning-client auth. Bootstrap invite. Daily cap. Roles and rank checks in one module, `permissions.rs`.
- [x] Keystore using the OS keychain with a file fallback.
- [x] **Review subagent** on `pake.rs`, `auth.rs` and `permissions.rs`.

**Done when:**

- Enrollment works through the CLI.
- An integration test with a TLS-terminating MITM proxy that relays the PAKE fails at step 5 and consumes one attempt.
- The third failure deletes the invite and notifies the minter.
- A 4th invite in 24 h is refused for a User.
- A Manager can't remove an Admin.
- The review findings are fixed or recorded.

### M4 Rooms and presence

- [x] `SetRoom` transaction, media id allocation, epoch bump, `Placed`, `RoomState`, presence events.
- [x] Room create, rename and delete with permissions. Deleting an occupied room moves people to `None`.
- [ ] `SetProfile` rate limit, avatars with lazy fetch.
- [x] Reconnect with `want_room`.

**Done when:**

- Three CLI clients see consistent presence through joins, moves and disconnects.
- A killed connection retracts presence within the idle timeout.
- Media ids never repeat across a server restart (test).

### M5 Media wire format and E2EE (freeze)

- [x] `frame.rs` header encode/decode. Flags. Size constants with `const` assertions.
- [x] Fragmenter and reassembler, media timestamp, 2048-bit dedup window.
- [x] `throcc-crypto::media` (AES-128-GCM transform) and `hpke.rs` (auth-mode wrap and unwrap).
- [x] Client key manager: per-epoch secrets, `PublishKey`, `SenderKey`, 500 ms pending buffer, 2 s grace, zeroize.
- [x] Server key relay checks: current epoch, recipients in room.
- [x] Contact pinning, key-change detection, safety-number derivation with test vectors.
- [x] CLI `send-synthetic` and `recv` that move fake access units of configurable size and rate through the real path.
- [x] Fuzz targets for header parsing and reassembly.
- [ ] **Parallel:** M7a (audio pipeline against WAV files) and M9a (encode/decode against recorded frames) start now in worktrees.
- [x] **Review subagent** on everything in this milestone.

**Done when** these property and integration tests pass:

- (media_id, epoch, seq) never repeats under one key.
- A modified header byte fails decryption.
- A member who joins at epoch N can't decrypt epoch N−1.
- A member who leaves at epoch N can't decrypt epoch N+1.
- A substituted encryption key with a bad signature is rejected.
- Duplicate and reordered datagrams are handled.
- 10,000 synthetic units at 20 Mbps arrive intact over localhost.

Then mark the wire format **frozen** in `DECISIONS.md`.

### M6 SFU egress and network tests

- [x] Per-receiver egress tasks with audio priority, whole-unit queueing, 150 ms budget, drop order.
- [x] `SetWatching`, the `Idr` on start, keyframe aggregation.
- [x] `LinkStats`.
- [x] `tests/netem/`: Linux network namespaces with `tc netem` and `tbf`. Scenarios: one receiver limited to 3 Mbps; 2% random loss; 30 ms jitter with reordering; sender uplink limited.
- [x] **Review subagent** on `sfu.rs` and `egress.rs`.

**Done when:**

- With a 10 Mbps synthetic share and receiver B limited to 3 Mbps, receivers A and C see zero server drops, and B sees only whole-unit drops.
- The sender gets at most 1 `KeyframeWanted` per second.
- Audio to B is never dropped.
- `LinkStats` reports B's drops.

### M7 Voice

- [x] M7a (from M5): `cpal`, `sonora`, `ropus` and `neteq` pipeline against WAV input and output, with the three acceptance tests from §5.8.
- [ ] Live pipeline over the network, mixing, per-user volume, deafen.
- [ ] Voice activation and push-to-talk per platform (subagent per platform).
- [x] Mute or gate for 5 minutes then resume.

**Done when:**

- The acceptance tests pass.
- After a 5-minute mute, the receiver's `neteq` buffer is back under 100 ms within 1 s of resuming.
- Two CLI clients hold a call on the human's machines (`MANUAL_TESTS.md` entry).

### M8 gpui client

- [ ] Shell and theme from M8a, wired to the real `Client`.
- [ ] Every screen in §5.13 matching its mockup, plus the unmocked states.
- [ ] Video surface per S3 (macOS first, then Linux, then Windows).
- [ ] Screen subagents work from `design/mockups/<Screen>.dc.html` and `theme.rs` only.

**Done when:**

- Each screen matches its mockup side by side. Take a screenshot per screen and put it in `docs/screens/`.
- Keyboard navigation works on every screen.
- The app holds a voice call between macOS and Linux.

### M9 Screen share video

- [ ] Capture backends, one subagent per platform, each delivering a GPU `AVFrame` and a `MANUAL_TESTS.md` entry.
- [ ] Encoder ladder, presets, recovery per S4, self-view.
- [ ] Reorder window, decoder fallback ladder, presentation.
- [ ] Glass-to-glass latency test: render a frame counter, share it, and read it back on the receiver.

**Done when:**

- 1440p60 at 10 Mbps flows from macOS to Linux and back.
- LAN glass-to-glass latency is under 150 ms at 1080p60.
- Killing 2% of datagrams causes a visible refresh within 1 s, not a frozen picture.
- The CPU stays under one core on the sender at 1440p60 with hardware encode.

### M10 Share audio and sync

- [ ] Own-process-excluding capture per platform (subagent per platform).
- [ ] Music-mode Opus track, its own `neteq`, AEC reference mixing.
- [ ] Audio-led sync hold, clock mapping per platform.

**Done when:**

- Sharing a screen while in a call never echoes the call's voices back to the room (manual test per platform).
- A clap test video shows A/V offset under 40 ms on the receiver.

### M11 Packaging and release

- [ ] Static server binary for Linux x86_64 and aarch64, the systemd unit, the container image.
- [ ] Client builds with an LGPL ffmpeg for macOS (signed and notarised, needs the human's certificate), Windows and Arch (PKGBUILD).
- [ ] `README.md`: run a server, back up the data directory, open UDP/8476, join.

**Done when** a clean Debian VM runs the server from the unit file, and fresh clients on macOS and Arch enroll and talk.

---

## 7. Decision points for the human

Ask these once, early, and record the answers. Until answered, use the default.

| Question | Default |
| --- | --- |
| Shares opt-in per viewer, or auto-watched on join? | Opt-in |
| Ship ffmpeg's HEVC software decoder in client builds? Every receiver must decode whatever a sender announces, so without it HEVC senders must be disabled | Ship it. Revisit before any commercial distribution, because of HEVC patents |
| PAKE crate (from S1) | Whatever S1 recommends |
| gpui surface approach (from S3) | Whatever S3 recommends |
| Wayland without the GlobalShortcuts portal: voice activation only? | Yes |
| macOS signing identity for M11 | Ask at M11 |

---

## 8. Testing summary

- **Unit and property tests** (`proptest`) for framing, the header, fragmentation, dedup, crypto and key rotation.
- **Fuzzing** (`cargo-fuzz`) for framing, header parsing and reassembly. Nightly in CI.
- **Integration:** a server plus N CLI clients in one test process over localhost.
- **Network:** the `tests/netem/` scenarios on Linux in CI.
- **Crypto:** known-answer tests for HPKE and AEAD, safety-number vectors, and the MITM enrollment test.
- **Audio:** the three acceptance tests in the tree.
- **Manual:** everything needing real devices, in `docs/MANUAL_TESTS.md`, each runnable with one command.
