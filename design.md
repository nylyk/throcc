# Minimal Self-Hosted Voice + Screenshare Platform — Design

## Overview

One server, one QUIC connection, one room at a time — or none.

A client connects to a single server and authenticates once with a public key. That authenticated connection is the whole session: control messages travel on one bidirectional stream (length-delimited postcard), media travels on QUIC datagrams. Authenticating gets you connected, not placed: you land in no room, see the full roster and room list, and enter a room when you choose to. From then on you move between rooms freely with no further handshake. There is no per-room connection and no per-room auth.

The server is a dumb SFU. It parses a fixed-width plaintext frame header to decide where a datagram goes, and forwards the payload untouched — no depayloading, no parsing, no transcoding. It does not need to understand the media to route it, and it is written so that it never has the opportunity to.

Identity is an Ed25519 keypair generated on first launch and stored on disk. The server holds an allowlist of public keys in SQLite. Enrollment is by one-time invite code. Rooms are persistent and may sit empty.

Everything is Rust: `quinn` for QUIC, `gpui` + `gpui-component` for the client. Media is assembled from single-purpose libraries rather than a framework — `cpal` for audio devices, `ropus` for the voice codec, `neteq` for the jitter buffer, `sonora` for everything between them, ffmpeg (via `rsmpeg`) for video encode and decode only, and one hand-written screen capture backend per platform. Shares are hardware-encoded and stay on the GPU on both ends.

**The entire audio path is Rust, with no C and no build tooling.** `ropus` is a port of the reference codec rather than a binding, and `sonora` is a port of WebRTC's audio processing chain rather than a binding — so there is no libopus, no meson, no ninja, no `pkg-config`, and nothing to cross-compile. That is most of what makes shipping a media app on three platforms unpleasant, and it is simply absent here. ffmpeg, on the video side only, is the one remaining native dependency.

Both were measured rather than taken on description. `ropus` at 24 kbps mono on an M-series Mac: encode 74× realtime, decode 1131×, in-band FEC recovers a dropped frame's audio correctly — eight simultaneous decoders cost well under one percent of a core. `sonora` against a synthetic room with a nonlinear loudspeaker: 46 dB of echo return loss within the first second, 73 dB once converged, and near-end speech retained at −2.9 dB through double-talk.

**Single-purpose libraries, and what that costs.** Each piece is chosen for the job rather than for fitting a pipeline: `neteq` is the jitter buffer real calls use, `ropus` exposes in-band FEC decode and packet-loss tuning directly, `sonora` is the same echo canceller browsers ship, and RTP is not needed at all (see *Fragmentation*). Capture is per-platform because the good sources are per-platform — ScreenCaptureKit on macOS, a portal flow on Wayland, Windows Graphics Capture — and none of them is improved by being wrapped. What this costs is format negotiation: matching pixel formats and memory types between capture, encoder and renderer is code you write, and a mismatch surfaces as a runtime error rather than a startup failure that names the formats. That trade is taken deliberately, and *Presenting frames without a CPU round trip* is where it is paid.

## What security this provides

- **On the wire, media and control are protected by QUIC's TLS.** An observer between client and server sees encrypted QUIC and nothing else.
- **Access is by public key.** Only allowlisted keys connect. No passwords to reuse or leak, revocation is per-key and immediate, and a stolen database yields public keys and invite hashes rather than credentials.
- **The server is authenticated to the client** by a pinned self-signed certificate, trusted on first connect and enforced from then on. No DNS name and no CA, so standing up a server is one command, and joining one needs only an address and an invite code.
- **The server sees your media in plaintext** and has to be trusted. Anyone who controls the host can watch and listen. This is the design's one significant limitation and it is deliberate: E2EE is a large amount of key-distribution machinery and is not worth building until the rest works. The layout below is chosen so it can be added later without redesigning anything.
- **Every authenticated user sees the full allowlist** — all public keys and display names. Deliberate: it makes an unexpected participant visible, which is the only defence this design has against a misbehaving roster.

## Project structure

```
throcc/
├── Cargo.toml                  # workspace, deps pinned here
└── crates/
    ├── throcc-proto/           # types + pure codecs. no tokio, no media, no I/O
    │   ├── lib.rs              # Error, the ALPN token, the default port
    │   ├── msg.rs              # ServerHello, Auth, AuthResult, Req, Resp, Event
    │   ├── ids.rs              # UserId, RoomId, MediaId, Epoch newtypes
    │   ├── frame.rs            # media datagram header, hand-rolled fixed-width
    │   ├── fingerprint.rs      # cert DER -> SPKI hash, one shared implementation
    │   └── framing.rs          # length-delimited + postcard framing, size-capped
    ├── throcc-server/
    │   ├── main.rs             # clap arguments, tracing, bind then run
    │   ├── lib.rs              # Server: bind, accept loop, one task per connection
    │   ├── identity.rs         # server_key load or create, certificate derivation
    │   ├── tls.rs              # rustls and quinn server config
    │   ├── session.rs          # per-connection task: hello, auth, then req loop
    │   ├── auth.rs             # signature verify, invite redemption
    │   ├── db.rs               # sqlite: users, rooms, invites, blobs, counters
    │   ├── rooms.rs            # roster, membership transitions, media id allocation
    │   ├── sfu.rs              # datagram -> ownership check -> fanout
    │   └── perms.rs            # rank checks, one place
    ├── throcc-client-core/
    │   ├── lib.rs              # Client: cmd(Cmd) in, Stream<Event> out. owns the tokio runtime
    │   ├── connection.rs       # connect, pinning verifier, control stream, reconnect
    │   ├── identity.rs         # keystore: identity key, known servers, settings
    │   ├── state.rs            # mirrored roster/rooms the UI renders
    │   ├── health.rs           # link diagnostics: quinn stats, send drops, per-track loss
    │   └── media/
    │       ├── mod.rs          # handles: start/stop, mute
    │       ├── audio.rs        # cpal devices, ropus, neteq, sonora (echo/noise/gain)
    │       ├── video/
    │       │   ├── encode.rs   # ffmpeg hardware encoder ladder
    │       │   ├── decode.rs   # ffmpeg decode -> platform GPU handle
    │       │   └── capture/    # one backend per platform, one trait
    │       │       ├── linux.rs    # xdg-desktop-portal + PipeWire, dmabuf
    │       │       ├── windows.rs  # Windows Graphics Capture, ID3D11Texture2D
    │       │       └── macos.rs    # ScreenCaptureKit, CVPixelBuffer
    │       ├── fragment.rs     # access unit <-> datagram payloads
    │       └── transform.rs    # FrameTransform trait. Passthrough for now.
    ├── throcc-cli/      # test client. exists from day one, see impl guide
    └── throcc-client/
        ├── main.rs             # gpui_platform::application(), window, root entity
        ├── app.rs              # root entity: state, render, event-bridge task
        ├── views/              # rooms, in-room grid, settings, admin
        └── video.rs            # GPU handle -> gpui surface. the only interop in the frontend
```

`throcc-proto` must not depend on tokio, ffmpeg or any capture backend, or it stops being the cheap shared crate. The server depends on `throcc-proto` only, never on anything media-aware — it has no codec, no capture and no ffmpeg in its dependency tree at all.

## Protocol

```rust
// ---------- handshake. server speaks first, saves a round trip ----------

struct ServerHello { server_nonce: [u8; 32], proto: u16 }

struct Auth {
    pubkey: [u8; 32],
    client_nonce: [u8; 32],
    invite_code: Option<String>,      // only on first enrollment
    want_room: Option<RoomId>,        // None = connect without entering a room
    sig: [u8; 64],
}
// sig = ed25519(id_key, b"throcc-auth-v1" || server_nonce || client_nonce || tls_exporter)

enum AuthResult {
    Ok {
        me: UserId,
        role: Role,
        users: Vec<User>,             // full allowlist, so no bootstrap fetch
        rooms: Vec<Room>,
        placed: Placed,               // same type SetRoom returns. room is None unless want_room was set
    },
    Err(AuthErr),   // UnknownKey, BadSig, BadInvite, ProtoMismatch, Banned
}

// ---------- client -> server ----------

// every client->server message carries an id the server echoes in its reply.
struct ReqEnvelope  { id: u32, req: Req }
struct RespEnvelope { id: u32, resp: Resp }
// Event has no id: it is unsolicited and answers nothing.

enum Req {
    // membership is one piece of state: you are in one room, or none.
    SetRoom(Option<RoomId>),                    // -> Resp::Placed

    SetProfile { name: String, avatar: Option<[u8; 32]> },   // full replace
    SetMedia { mic: bool, screen: bool, share_audio: bool,
               screen_kbps: u32, codec: Codec },

    RequestKeyframe(MediaId),                   // routed to that sender

    CreateRoom { name: String },
    RenameRoom { room: RoomId, name: String },
    DeleteRoom(RoomId),

    CreateInvite { role: Role, ttl_secs: u32 },
    SetRole { user: UserId, role: Role },
    RemoveUser(UserId),
}

enum Resp {
    Ok,
    Err { code: ErrCode, msg: String },
    Placed(Placed),
    InviteCode { code: String, expires: u64 },
    AvatarHash([u8; 32]),                       // reply to an upload stream, see below
}

struct Placed { room: Option<RoomId>, epoch: Epoch,
                tracks: Option<Tracks>, peers: Vec<PeerState> }

// ---------- server -> client ----------

// one stream carries both, so what goes down the wire is tagged.
enum ServerMessage { Resp(RespEnvelope), Event(Event) }

enum Event {
    UserEntered { room: RoomId, epoch: Epoch, peer: PeerState },
    UserExited  { room: RoomId, epoch: Epoch, user: UserId },
    PeerMedia { user: UserId, tracks: Tracks, mic: bool, screen: bool,
                share_audio: bool, screen_kbps: u32, codec: Codec },
    ProfileChanged { user: UserId, name: String, avatar: Option<[u8; 32]> },
    KeyframeWanted(MediaId),
    RoomCreated(Room), RoomRenamed { room: RoomId, name: String }, RoomDeleted(RoomId),
    UserAdded(User), RoleChanged { user: UserId, role: Role }, UserRemoved(UserId),
    Kicked { reason: String },
}

// ---------- media ids ----------

// A share is video plus optionally its system audio, grouped so a receiver knows
// which audio belongs to which screen. Exactly one share today.
struct Share  { video: MediaId, audio: Option<MediaId> }
struct Tracks { mic: MediaId, shares: Vec<Share> }
enum Codec { Av1, H265, H264 }  // sender's choice, announced not negotiated
struct PeerState { user: UserId, tracks: Tracks, mic: bool, screen: bool,
                   share_audio: bool, screen_kbps: u32, codec: Codec }

// ---------- media datagram: 13 bytes, fixed width, big endian ----------

struct FrameHeader {
    media_id: MediaId,      // u32. identifies (user, room, track, membership session)
    epoch: Epoch,           // u32. room generation. see below.
    seq: u32,               // monotonic per media_id, never resets
    flags: u8,              // bit0 = keyframe, rest reserved
}
// everything after the header is an opaque payload the server never reads
```

The header is hand-rolled fixed-width rather than postcard: the SFU parses it on every packet, and postcard's varints would make field offsets data-dependent for no benefit.

**Why the header is 13 bytes and not 17.** `media_id || epoch || seq` is 96 bits, which is exactly a ChaCha20-Poly1305 nonce, so a `u32` `seq` is the largest field that still leaves the nonce padding-free. Uniqueness does not depend on the width: `media_id` is never reused for the lifetime of the deployment and `seq` never resets within one, so `seq` alone distinguishes every packet a track ever sends, and `epoch` is along for the ride purely because it has to be in the header anyway (see below). A `u32` `seq` wraps after roughly 4.3 billion packets — about three months of continuous sending on a saturated 5 Mbps video track, on a `media_id` that is discarded on any membership change or reconnect. Treat wrap as fatal, like the other counters, and never let it happen.

Four bytes matters more than it looks: on a 90-byte Opus packet, 17 bytes of header is 19% overhead and 13 is 14%, and the header is the one part of this system that is genuinely painful to change once clients are deployed. Get it right at M5 and never touch it.

Reserved flag bits: the server reads bit0 only and ignores the rest, so a bit can be introduced later without a server upgrade. Clients reject unknown bits, so a client never silently misinterprets a frame it doesn't understand.

`Placed` is one type used by both `AuthResult::Ok` and `Resp::Placed`, and `Auth` carries `want_room`, so a reconnecting client lands directly back in the room it was in. Without that, reconnect is "connect, then move", which is an extra round trip before anyone's audio comes back and a visible gap in the roster for everyone else. A client that was in no room reconnects with `want_room: None` and the whole question does not arise.

`SetProfile` replaces the whole profile rather than patching fields. A patch needs `Option<Option<_>>` to distinguish "leave alone" from "clear", which is a wart, and profiles are two small fields.

`SetMedia` carries `screen_kbps`, the sender's own configured bitrate, and the server relays it in `PeerMedia` without acting on it. It is a diagnostic (see *Link diagnostics*), not a negotiation — nothing changes it at runtime and no receiver may ask for a different value.

`codec` is announced the same way, and for the same reason: the sender picks the best hardware encoder it has, in the order AV1, H.265, H.264, and everyone else copes. Receivers must be *told* rather than sniffing it: there is no payload type on the wire to sniff, and the alternative — probing the bitstream — means parsing exactly the bytes this design keeps opaque. What makes announce-don't-negotiate safe is not that everyone has hardware decode — it's that software decode of all three is universally available, so the worst case is CPU cost rather than a black rectangle.

### Framing limits

`framing.rs` caps a control frame at 1 MiB and drops the connection on anything larger, checked before allocating. Without that, the length prefix is a one-line memory-exhaustion vector.

Avatar uploads do not go on the control stream. That stream carries membership changes and roster events, and blocking it behind a multi-megabyte image is exactly the head-of-line problem media datagrams exist to avoid. An upload opens a fresh unidirectional stream and sends a small postcard header — `{ req_id: u32, len: u32 }`, with `req_id` from the same counter as `ReqEnvelope` — then the bytes. The server replies with a normal `RespEnvelope { id: req_id, resp: AvatarHash(..) }` on the control stream, so the upload completes through the same pending-request map as everything else. Server-side size cap, and re-encode to fixed dimensions before storing.

### Request correlation

Every request carries a `u32` id from a monotonic client-side counter, and the server echoes it. Replies over one QUIC stream do arrive in order, so matching by position would work, but an explicit id is four bytes and removes the need for anyone to know that. It also makes a mismatch loud: a client receiving an id it has no pending request for logs a protocol error and drops the connection, where a position-matched design would quietly pair a reply with the wrong request and corrupt state in a way that is very hard to trace.

The client keeps a `HashMap<u32, oneshot::Sender<Resp>>` and completes by id, so requests may be in flight concurrently — which is what stops an upload or an invite generation blocking a room change. Counter wraparound is unreachable in a session; treat it as fatal rather than reusing ids.

`Event` has no id. It is unsolicited and there is nothing to correlate with, so giving it one would only invite code that tries to match events to requests. Telling a reply from an event is the `ServerMessage` tag's job, not the id's — both share the one control stream, and a reader that has to guess from the shape of what it decoded is a reader that will eventually guess wrong.

## Auth

The server sends `ServerHello` with a nonce immediately on accept. The client signs a domain prefix, both nonces, and the QUIC TLS keying-material exporter, then sends `Auth`.

- The domain prefix stops the identity key from ever producing a signature valid in another context added later.
- The server nonce stops replay.
- The client nonce is cheap hygiene against signing bytes the server chose entirely.
- The TLS exporter binds the signature to this exact connection, so a captured or relayed signature is useless.

The server verifies the signature, then checks the key against the allowlist, or redeems the invite code and inserts a new user. `AuthResult::Ok` ships the full roster, the room list, and your placement, so the UI renders from one message with no bootstrap fetch.

### Server identity

The server generates its own identity on first start and self-signs. No DNS name to own, no ACME, no reverse proxy, no renewal hook, no certificate to install. Running the binary is the entire setup, and it works unchanged on a public IP, a LAN address, or a Tailscale interface.

**The persisted secret is a keypair, not a certificate.** `server_key` is created in the data directory at mode `0600` if absent, and the certificate is derived from it in memory at every startup. The certificate is a deterministic function of the keypair — a fixed subject and SAN, a validity window nobody checks, and the public key — so persisting it would be storing state that is already derivable from state we have to persist anyway, in a variable-length encoding rather than 32 fixed bytes. Derived state on disk is a second thing to lose, corrupt, or let fall out of sync with its source, and it buys nothing but a skipped `rcgen` call at startup.

What this preserves is the freedom to change the certificate later. Nothing environment-specific is in it today, but if one ever has to be — a real SAN for a client that insists on checking names, a shorter validity window — the pin is on the SPKI and does not move, so reissuing stays a non-event rather than a warning in front of every user. That property comes from hashing the SPKI rather than the whole certificate; deriving rather than persisting is what keeps it cheap to exercise.

Back that file up. Losing it is not data loss but it is an identity change, and every client has to re-accept through a warning.

**The client pins the public key.** The keystore holds a `known_servers` map from address to SPKI hash. The client installs a `rustls` verifier that compares that hash and checks nothing else — no chain, no hostname, no expiry. Ignoring expiry is correct, not lazy: an expiry date exists so a relying party can stop trusting an issuer's attestation, and there is no issuer and no revocation here. The pin is the trust anchor; leave expiry checking on and the app breaks by itself on a date in the future, which is the worst class of bug to ship. Because names are not checked, `server_name` on connect is a fixed placeholder and SNI carries nothing.

A pin mismatch is a hard failure showing both fingerprints, with an explicit re-accept action in the UI. That action has to exist: rebuilding a server or losing its data directory is a normal operator event, and the recovery path must not be hand-editing a config file.

**First contact is trust-on-first-use.** Joining takes two things: an address — a domain or an IP, port 8476 unless stated — and an invite code. Nothing else. There is no connect string, no pasted fingerprint, no out-of-band token to transcribe. The client accepts whatever answers on first connect, stores that key, and enforces it forever after.

This is a deliberate trade of first-contact authentication for an onboarding that people complete. A fingerprint in the address would authenticate the first connection, but it makes the thing an admin sends over chat a 90-character string, and the failure mode of an unusable flow is that people paste from wherever is convenient and stop reading it anyway.

What limits the damage is that the invite is single use and the auth signature covers the TLS keying-material exporter. An attacker who terminates TLS in the middle cannot forward the client's signature to the real server, so it cannot impersonate the user and cannot transparently proxy the session. The worst outcome is a dead-end fake server that burns one invite code and shows an empty room — noticed immediately, fixed by issuing another. What TOFU does not survive is an attacker in position *during* first contact who then stays there, which is the same exposure SSH has carried for thirty years.

The server prints its fingerprint at startup anyway. It is not needed to connect, but it is the only source for an operator who wants to verify a pin out of band, and it is what the mismatch dialog compares against.

QUIC requires an ALPN token — use your own, `throcc/1`, unrelated to HTTP — and the listener is UDP. The default is **UDP/8476**: unprivileged, unassigned by IANA, and free of the ambiguity of sharing a port with whatever HTTPS or HTTP/3 an operator already runs.

That is a deliberate step away from the more reachable choice. UDP/443 passes more networks, because a firewall permitting HTTP/3 cannot distinguish this traffic from it by port alone; 8476 is blocked wherever egress is filtered to well-known ports. Since some corporate and hotel networks block UDP outright and this design has no TCP fallback, those calls were never going to connect anyway — so the port buys reachability only in the narrower case of a network that allows UDP but only to 443. An operator who wants that case back should publish 443 on the host and map it, since nothing in the protocol assumes a port: the address an operator hands out carries one, and the pin is keyed to it.

### Invites and roles

An invite code is a short opaque string and nothing more — no key material, no server identity, no role encoding, no structure a client parses.

Admins generate one from the client UI with a TTL. The code is six characters from `OsRng` over the full alphanumeric alphabet — the ten digits and the twenty-six uppercase letters, `K7QM2X`. Six characters is short enough to read over the phone, type from memory, or put in a text message without anyone reaching for copy-paste, which is the entire point: this plus an address is the whole join flow. Input is uppercased before lookup, so case never matters. The server stores only a hash, so reading the database yields nothing redeemable. Single use, default TTL 24 hours. The row is kept and marked redeemed rather than deleted, so "who enrolled with which invite" survives as an audit trail; expired and redeemed rows are pruned on a timer.

Thirty-six symbols rather than a confusable-free subset is a deliberate choice for a code this short: dropping `0`/`O` and `1`/`I` would cost about a bit and a half of an already small budget. The cost is paid at the point of entry instead — the client shows the code in a font that distinguishes them, and a failed redemption says "check O versus zero" rather than only "invalid".

Redemption is the whole mechanism: a client presents a valid unexpired code alongside the public key it generated on first launch, and the server inserts that key into the allowlist with the role recorded against the invite. The key persists; the code is gone. Revocation therefore stays per-user forever, and a leaked invite grants exactly one enrollment rather than standing access.

**The attempt limit is what makes six characters safe, so it is not optional.** Thirty-six to the sixth is 2.18 billion codes, a little over 31 bits — enough that nobody guesses one by hand, and not enough to leave unguarded. A script at a thousand attempts a second covers about four percent of that space in the twenty-four hours a code is live, and a hit is an allowlisted account rather than a failed login.

So the budget is global, not per-connection and not only per-address: a per-connection limit is defeated by reconnecting and a per-address one by rotating through a botnet or an IPv6 prefix. Limit failed attempts per source address *and* keep a server-wide failure budget that, once exceeded, refuses all redemption until an operator clears it. Refusing redemption is a mild failure — an admin reissues a code — while the alternative is silent enrollment. Log every failed attempt with its source; a burst is the signal that someone is scanning.

If a deployment needs a wider margin, the lever is TTL rather than length: an invite that lives one hour instead of twenty-four cuts the exposure by the same factor as adding a character, and costs nothing to read aloud.

The first-run bootstrap code is written by the headless server to a `0600` file rather than stdout, so it does not land in a shared journal.

Three roles, each with an integer rank:

| Role | Rank | Can |
| --- | --- | --- |
| `User` | 0 | Be in rooms |
| `Manager` | 1 | Create, rename, and delete rooms |
| `Admin` | 2 | Invite users, change roles, remove access |

The only targeting rule: you can act on strictly lower ranks. That gets "admins can't remove other admins" for free without a fourth tier or per-role special cases. If more flexibility is needed later, make it capability flags per role in the server config — an integer scale cannot express "can invite but cannot create rooms".

Removing a user deletes the allowlist row and closes any live connection holding that key. Because identity is the key and not a name, there is no orphaned session to reason about.

## Rooms and membership

Membership is a single value per connection: `Option<RoomId>`. There is no join and no leave, only `SetRoom`, applied by the server as one transaction. A move is otherwise a leave plus a join that can fail halfway or interleave with someone else's, and "which room am I in if the second half failed" is a question with no upside. A failed `SetRoom` leaves you exactly where you were.

**Connected and in no room is a first-class state, and it is where you start.** On auth you are placed in `want_room` if you asked for one — which only a reconnecting client does — and otherwise in `None`. There is no default room and no `is_default` column: a default room exists only to auto-join people, and auto-joining is the behaviour being removed. `None` is not a degenerate case to be escaped; it is the lobby. You have the full roster and the room list, you can see who is where, you can change your profile and generate invites, and no capture device is open.

Auto-joining was wrong for a voice tool the same way a hot mic is wrong: connecting is something a client does on launch, on wake, and on every reconnect, and each of those would have dropped you into a room with people already in it, announced by a `UserEntered` fanout, without you having chosen anything. Making entry explicit also collapses three states — starting up, present in the lobby, deliberately alone — into one honest one, and it means the client can hold a connection open in the background at the cost of a QUIC connection and nothing else.

Entering a room means entering its media session, so **mic and screen both start off** and `SetMedia` turns them on. Entry is one round trip and nothing else — no negotiation, no per-room setup. `Placed` carries your freshly allocated `Tracks` and the peers already present, and the server fans out `UserEntered`.

`SetRoom(None)` leaves without disconnecting, and is the exact state you authenticated into. Closing the connection is equivalent for everyone else's purposes. Empty rooms cost a SQLite row and are expected to sit empty. Any room may be deleted; deleting an occupied one moves its occupants to `None` and fans out the event, which needs no special case now that `None` is an ordinary place to be.

### Media id allocation

Every membership change allocates a **fresh `Tracks`** — one `MediaId` for mic and one for the share — from a single monotonic `u32` counter persisted in SQLite, **never reused** across rooms, reconnects, or restarts.

This is stricter than routing needs, deliberately. Per-track rather than per-user ids give the server a flat `MediaId -> subscribers` map with no per-packet branching on media kind, let a receiver drop one track without touching the others, and route a departed peer's in-flight packets to nobody rather than to the wrong subscribers. It is also the single precondition for adding encryption later without a nonce-reuse footgun: one id per user plus a kind bit would give two tracks a shared nonce space with two independent counters, which is keystream reuse under a shared key — invisible until it is catastrophic.

Counter exhaustion is ~1.4 billion membership changes. Treat it as a fatal startup error rather than wrapping, because wrapping silently breaks the never-reuse property everything above depends on.

**Shares are a `Vec`, with one entry, on purpose.** Dropping the camera track already showed that a fixed set of named fields is not a stable shape, and the next thing anyone asks for in a screenshare tool is the second monitor. `Tracks` is the part embedded in the wire format, the routing table and the SFU's ownership check, so it is the part that has to be able to grow; the vector makes adding an `OpenShare -> MediaId` request later a new enum variant rather than a wire break. `SetMedia`'s boolean becomes a count or a per-id map at that point, which is a cheap change by comparison because nothing routes on it.

### Epoch

The server keeps a per-room `epoch` in SQLite, bumps it on every membership change, and includes the current value in `Placed` and in membership events. Senders stamp the latest value they have seen into the frame header. Receivers ignore it.

That is dead weight today, about fifteen lines. It is worth them because the wire format is the one thing genuinely painful to change once clients are deployed, and the epoch column plus the header field are what a future key-rotation scheme needs: with per-epoch room keys, the epoch in the header is how a receiver picks which key to try. It is also what fills the nonce out to 96 bits, which is why `seq` gets to be a `u32`. Persist it, never decrement it, treat overflow as fatal.

## Media

Media goes on QUIC datagrams (RFC 9221), never streams. Streams are reliable and ordered, so a lost packet produces retransmits and head-of-line blocking on frames that are already too late to display.

### Datagram size is fixed, not negotiated

Use one compile-time payload budget for the whole deployment rather than each sender sizing to its own `max_datagram_size()`.

This matters because the SFU forwards bytes unmodified and cannot re-fragment. If A negotiates a larger datagram size than B, A's packets are simply undeliverable to B — silently, with no error at either end, and only for some pairs of participants, which is about the worst failure shape available. Pick a conservative frame (1200-byte datagrams is safe across essentially all paths), assert at handshake that the peer's `max_datagram_size()` is at least that, and fail loudly with a clear message if it isn't or if datagram support is absent entirely.

Mind what the 1200 refers to. It is the UDP datagram; a QUIC DATAGRAM frame's payload is what remains after QUIC's own packet overhead, which quinn reports as **1162 bytes** at default settings. Payload budget is therefore `1162 - 13 - transform.overhead()`, i.e. **1133 bytes** today, of which one is the fragment byte — so **1132 bytes** of codec output per datagram. Since 1200 is QUIC's floor for the UDP datagram, 1162 is a floor as well — which is what makes it safe to freeze in a `const`. Do not take the figure from a live connection: MTU discovery can raise it mid-connection, and a sender that used the raised value would emit packets that some receivers cannot accept, which is the exact failure this section exists to prevent.

### Fragmentation, and why there is no RTP

The payload is the encoder's own bytes, fragmented by us. Byte 13 — the first byte after the frame header — is a fragment byte: bit0 marks the first datagram of an access unit, bit1 the last, the rest reserved. Everything after it is codec output, untouched.

There is no fragment index, because `seq` already is one. It is monotonic per `media_id` and never resets, so the datagrams of one access unit are a contiguous run of `seq` between a first-marked and a last-marked packet. A receiver buffers by `media_id`, assembles when it holds an unbroken run, and discards the whole access unit on a gap — which is the correct behaviour anyway, since a video frame missing a slice is not decodable and a missing Opus frame is what FEC and concealment are for.

Audio never fragments: a 20 ms Opus frame at 24–64 kbps is 60–160 bytes against a 1132-byte budget, so both bits are always set.

**Bit2 means a capture timestamp follows.** Four bytes, 90 kHz ticks, on the first fragment of an access unit only — so it costs 4 bytes per frame, not per packet, and nothing at all on tracks that do not set it. It exists for one purpose: keeping a screen share's audio in step with its video (see *System audio*). Mic audio does not set it, because nothing is ever synchronised to a microphone.

**Why not RTP.** The earlier design carried RTP inside the payload to inherit working payloaders and a jitter buffer. Neither reason survives:

- **Nothing foreign ever sees this stream.** Codecs are announced rather than negotiated, the SFU is ours and never reads past byte 13, and there is no gateway, no recording sink, and no third-party client. RTP payload formats exist to hand a codec bitstream to a depayloader written by someone else — FU-A for H.264, OBU fragmentation for AV1 — and every one of those rules is unnecessary when both ends are the same codebase. Blind fragmentation is codec-agnostic: it works unchanged for AV1, H.265, H.264 and anything added later, with no per-codec payloader to write or test.
- **The jitter buffer does not want RTP either.** `neteq` takes packets you construct, not wire bytes: an `RtpHeader { sequence_number, timestamp }` you fill in yourself. Both fields come free from `seq` — one datagram is one 20 ms frame, `seq` never resets, so `timestamp = seq * 960` at 48 kHz is exact by construction rather than by parsing.
- **It deletes the redundancy.** RTP's 16-bit sequence number alongside the header's 32-bit one was two bytes per packet of the same information, and the previous version of this document spent three paragraphs justifying the overlap. One sequence number, one authority on ordering.

What is given up: you can no longer point a standard tool at a captured stream and have it depayload, and a third-party SFU could never be dropped in. Both were already ruled out by the codec-announcement model and by the server never reading past byte 13.

**The encryption story is unchanged, and slightly better.** Everything from byte 13 onward is opaque and later encrypted — the fragment byte included, which is the point of putting it in the payload rather than spending two of the header's reserved flag bits on it. Those bits are in the clear and would have handed the server exact frame boundaries and frame sizes: traffic analysis against the one party the encryption is meant to defend against. One byte inside the encrypted region costs 0.09% of the budget and keeps the header carrying nothing but routing. SFrame reached the same layout for the same reason.

The frame header itself is passed to the AEAD as additional authenticated data. It stays plaintext so the server can route on it, but it cannot be altered in flight without failing the tag.

### Audio

Three libraries and no framework: `cpal` for devices, `ropus` for the codec, `neteq` for the jitter buffer — and the `sonora` family for everything between them. All Rust, no C, no build tooling.

`sonora` is a port of WebRTC's audio processing rather than a binding to it, and it is a port of the *whole* thing, so it covers more than the obvious chain:

| From | What it gives |
| --- | --- |
| `sonora` | The processing chain: AEC3, noise suppression, AGC2, high-pass filter — four fields on one config |
| `sonora-common-audio` | WebRTC's `PushResampler`, for devices that refuse 48 kHz |
| `sonora-agc2` | An RNN voice activity detector, already running inside AGC2 |

Taking the resampler from here rather than from a separate crate is not about saving a dependency — it is that a resampler feeding an echo canceller is part of that canceller's signal path, and two vendors' idea of group delay is exactly the kind of mismatch that degrades cancellation in a way nothing points at. Verified in isolation: 441 samples in, 480 out per 10 ms block, energy per sample preserved to four decimal places.

The VAD is not used yet. It is worth knowing about because a "who is talking" indicator is the obvious next request and the cheap implementation is entirely receiver-side — run the detector over each peer's decoded audio locally, with no protocol change and nothing added to `PeerState`.

**Send.** `cpal` input callback → (resample if the device insists) → echo cancellation, noise suppression, gain control → `ropus` at 48 kHz mono, VoIP mode, in-band FEC on, 20 ms frames → one datagram per frame. Mute stops sending rather than sending silence: it saves bandwidth and makes mute a real property instead of something inferred from an absence of packets.

**Receive.** One `neteq` per remote `media_id`. The datagram loop hands it packets; the `cpal` output callback pulls 10 ms frames from it and mixes. `neteq` is the whole timing story on the receive side — it decides adaptively how much buffer the current network needs, stretches and shrinks audio to hold that target without pitch artefacts, and conceals what never arrives. It is the algorithm behind every browser call, and reimplementing even a bad version of it would be weeks.

`neteq` takes packets you construct rather than RTP off the wire, and both fields it needs come free: `sequence_number` is `seq` truncated, and `timestamp` is `seq * 960` at 48 kHz, exact because one datagram is one 20 ms frame and `seq` never resets. The Opus decode itself is `ropus`, which `neteq` already depends on — one codec implementation in the tree rather than two.

**Capture cleanup is one module, not four.** The four stages are fields on one config rather than four dependencies:

| Stage | Why it is on |
| --- | --- |
| Echo cancellation | A meaningful fraction of desktop users are on speakers, and without it everyone else hears themselves. Needs the playout signal as its reference, which is the one piece of audio plumbing spanning the send and receive paths. |
| Noise suppression | Fans, keyboards, and the room. Four levels trading noise reduction against speech distortion; moderate is the default and the aggressive settings audibly chew consonants. |
| Gain control | Users should not have to ride their own input level, and a peer who is inaudible reads as a broken app. |
| High-pass filter | Rumble and handling noise. Enabling noise suppression force-enables it anyway. |

Ordering inside the chain is the module's business, not ours, and that is a reason to take all four from it rather than assembling them: noise suppression that runs before echo cancellation degrades the echo canceller's estimate, and getting that wrong produces echo that comes and goes with the speaker's voice — a bug that is very hard to attribute.

**It costs nothing to build.** The obvious choice here is `webrtc-audio-processing`, a binding to the C++ original, which drags meson and ninja into the build on all three platforms and would have been the heaviest build dependency in the project. `sonora` is a port rather than a binding: no `build.rs`, no C++, and its module structure is a faithful transcription of the upstream tree — `echo_path_delay_estimator`, `matched_filter`, `nearend_detector`, `clockdrift_detector`, `erle_estimator` are the upstream file names.

**The risk it carries instead is maturity, and that is the one to manage.** It is a young crate implementing a subtle algorithm, and echo cancellation fails in ways users describe as "it sounds weird" rather than as a bug report. So the acceptance test goes in the tree, not in a spike: a synthetic room with a nonlinear loudspeaker, asserting echo return loss above a floor once converged, and asserting near-end speech survives double-talk. That test is thirty lines, it is what proved the crate in the first place, and it is also exactly the harness for evaluating a replacement. The fallback is `webrtc-audio-processing`, whose config API `sonora` mirrors closely enough that switching is mechanical — you pay the meson dependency and nothing else changes.

**The chain runs on 10 ms frames**, fixed. Opus encodes 20 ms, so capture processes two APM frames per encoded frame — a detail that has to be right in the buffering or every second frame is silently unprocessed.

### Bitrate is set by the sender and never changes

No adaptation, no simulcast, no receiver-side requests to turn it down. A sender picks a bitrate in settings, the encoder runs at it, and the same encoding goes to everyone. This is the TeamSpeak model and it is the right one here: adaptation means one weak downlink degrades the picture for every other participant in the room, which for a handful of people who mostly know each other is a worse outcome than the weak peer simply having a bad time.

The consequence to design around is not quality, it is silence. When a path cannot carry the configured bitrate, packets are dropped — locally, by congestion control, with no error at either end. That failure looks identical to a bug. So the thing to build in place of adaptation is telemetry.

This bites harder than it would for a talking-heads call. Motion-heavy content at 1440p60 wants somewhere between 10 and 25 Mbps to look good, and plenty of domestic uplinks cannot sustain the top of that range. The default should therefore be conservative rather than flattering, and the diagnostics below are not optional garnish — they are the only thing standing between a badly chosen bitrate and an unexplainable stutter.

### Link diagnostics

Three signals, none of which changes the SFU or the frame format.

**Sender side.** quinn's outgoing datagram queue is bounded, and when the congestion window is full, datagrams are dropped rather than erroring — `send_datagram` returning `Ok` is not evidence that anything left the machine. Sample `Connection::stats()` on a timer for rtt, congestion window and lost packets, and keep your own counter of packets the send channel refused. Nonzero drop rate over a few seconds is a plain-language warning: your upload cannot carry the bitrate you chose, lower it in settings.

**Receiver side.** Free. The dedup window already tracks the highest `seq` seen per `media_id`, so counting the gaps that never fill gives per-track loss with no protocol addition at all. Every client computes it locally for every peer it receives from.

**Both together.** This is what `screen_kbps` in `SetMedia`/`PeerMedia` is for. Knowing the sender configured 4 Mbps and measuring 600 kbps arriving distinguishes "their uplink is saturated" from "my downlink is" — two problems with different fixes that are indistinguishable from either end alone. It is four bytes on a message that is already being sent.

The point of all three is that "screenshare is choppy" resolves to a number and a direction instead of a support thread.

### The media/tokio boundary

Two worlds meet here and neither may block the other. Audio runs on `cpal`'s device callbacks, which the OS drives on a realtime thread; capture and encode run on their own threads; tokio owns the QUIC connection. The rule at every one of those boundaries is the same: bounded channel, `try_send`, drop on full, count the drop.

This gets simpler without a framework in the way — the threads are ones you spawned and the callbacks are ones you registered — but the constraint is stricter, not looser. **A `cpal` output callback is a realtime audio callback**: no `.await`, no `blocking_send`, no `block_on`, no allocation, no lock that another thread can hold across a syscall. Miss the deadline and the device underruns, which is an audible click that looks like anything except a channel. `neteq`'s `get_audio()` is designed to be called from exactly this position and returns a fixed-size frame, so the callback's whole job is: pull one frame, copy it out, return.

**Outbound.** Encoded output — an Opus frame from the audio thread, an access unit from the encoder thread — is fragmented, each fragment gets the header and the fragment byte, `outbound` runs, and the fragments are `try_send` onto a bounded `mpsc` (64 entries is ample). One tokio task owns the receiver and is the only code that calls `send_datagram`. On `TrySendError::Full`, drop and bump the counter feeding the sender-side diagnostic above. Dropping is correct rather than regrettable: a media packet you cannot send now is worthless in 20 ms, which is the same argument that put media on datagrams. Drop whole access units rather than individual fragments — half a frame is not decodable, so sending the rest is bandwidth spent on something the receiver will discard.

**Inbound** is the mirror image and the worse trap, because the receive path is shared: one datagram loop serves every peer. It must never do work that can block on any single peer. Its whole job is parse the header, check the dedup window, run `inbound`, and `try_send` to that `media_id`'s own task — which owns the reassembly buffer, the decoder and, for audio, the `neteq` instance. A peer whose decoder stalls then starves only itself. Getting this wrong is the classic symptom: one participant's bad stream making the whole room's audio stutter.

A panic in a `cpal` callback or an ffmpeg callback unwinds across an FFI boundary, which is undefined behaviour. Keep those callbacks small enough that there is nothing in them to panic — no indexing, no `unwrap`, no allocation.

**Throughput is not the concern here; copies are.** A share at 20 Mbps is roughly 2,100 datagrams a second, and a tokio `mpsc` will do millions of sends a second, so the packet channels sit three orders of magnitude clear of trouble and no plausible bitrate changes that. What costs is the per-packet work either side of the channel: one allocation and one memcpy per fragment, 2,100 times a second. Fragment straight into `bytes::Bytes` slices of one encoder output buffer rather than building a `Vec<u8>` each time — `send_datagram` takes `Bytes`, so a `Vec` is a second copy at the send site, and `Bytes::slice` over a shared buffer makes fragmentation refcount arithmetic instead of copying.

The decoded-frame path is where the data volume actually is, and it is not a channel problem either — a `watch` moves a handle, not pixels. It is a *memory bandwidth* problem, and the answer is not to move less data through channels but to keep the data on the GPU end to end. See *Presenting frames without a CPU round trip*: at 1440p60, the naive decode-download-upload path costs about 1.8 GB/s of pure overhead, and since the content is games and video there is no framerate cap available to shrink it. That section is the load-bearing one for this app's performance; this one is only about why the channels aren't the thing to worry about.

### Sequence numbers

`seq` is monotonic per `media_id` and never resets. Since a `media_id` is never reused, `(media_id, seq)` is globally unique for the lifetime of the deployment.

Receivers keep a 64-entry sliding window per `media_id` and drop anything already seen or below the window. That gives dedup and reorder rejection for free, it is the same window a replay check would need later, and it is where receiver-side loss measurement comes from.

### System audio travels beside the share, not inside a container

Sharing a screen without its sound is half a feature — a game, a video, anything being demonstrated. So a share is video plus optionally its system audio, grouped in `Share { video, audio }` so a receiver knows which sound belongs to which screen.

**The instinct to mux them into one container is right about the goal and wrong about the mechanism.** What a container buys is a shared timeline: presentation timestamps that let a receiver put the picture and the sound back together. What it costs, on this transport, is everything else:

- **It couples loss.** Video is bursty and occasionally lossy; audio is small and must survive. In one container a lost fragment damages whatever was interleaved with it, so a stuttering picture takes the sound down with it. As separate tracks they degrade independently, which is the behaviour anyone would actually want: audio keeps going while the picture recovers.
- **It breaks the SFU.** The server routes on `media_id` and never reads past byte 13. A muxed stream would either force it to demux — ending the dumb-SFU property and the encryption story with it — or make routing all-or-nothing, so a receiver who has the share off screen cannot stop paying for the video.
- **The two want different loss policies.** Audio wants in-band FEC and concealment; video wants keyframe repair and whole-frame discard. One container means one policy for both.
- **It reintroduces head-of-line blocking.** A 1440p frame is ~37 datagrams. Interleaved into one stream, an audio packet waits behind that burst — precisely the problem media datagrams exist to avoid.
- **We already have framing.** Container framing would be a second layer of it, paying per-packet overhead to re-supply something the frame header already provides.

**So take the timestamp and leave the container.** Both tracks of a share are stamped from one clock at capture, carried in bit2's four bytes, and the receiver aligns on them. That is the useful half of what a container does, at four bytes per frame, with no coupling.

**Sync happens within a share, never across sources.** This is the rule, and it resolves what looks like a contradiction with *No audio/video sync* elsewhere in this document:

- **Within a share, sync matters and is done.** Gunshot and muzzle flash, or a video's lip sync, are *inside the content* — an offset there is the content looking broken, and a viewer will blame the app.
- **Across sources, sync is meaningless and is not attempted.** Your microphone and someone else's screen have no common timeline and never did. Nothing aligns them, exactly as before.

Alignment is audio-led, because audio is the clocked side: share audio plays out through `neteq` as usual, and share video is held until its timestamp matches what the audio is currently playing. The cost is that video inherits the share-audio buffer depth as latency. Two things keep that acceptable — the share-audio track is a clean digital tap with far less arrival jitter than a microphone, so `neteq` should settle on a shallow target; and when a share carries no audio, nothing is held and frames display on arrival exactly as before.

**Clock mapping is the platform-specific part.** macOS is the easy case: ScreenCaptureKit delivers video and system audio from one `SCStream`, already on one clock. Linux gets both from PipeWire, which timestamps in one domain. Windows is the awkward one — Windows Graphics Capture and WASAPI loopback are separate subsystems with separate clocks, so both have to be mapped onto QPC before they mean anything together. Expect that to be where the sync bug lives.

### Video gets a reorder buffer, not a jitter buffer

Audio has `neteq`; video deliberately has no equivalent, and the reason is that the two have different failure modes rather than different tolerances.

**Audio is clocked and video is not.** The sound card demands a frame every 10 ms whether or not the network cooperated, so a late packet is a *hole* — silence, a click, a gap. There is no way to not answer. That is why audio needs an adaptive buffer that trades latency for the ability to always have something to play, and why it is worth time-stretching audio to hold that target. Video has no such clock: nothing asks the client for a frame. A late frame means the previous one stays on screen a few milliseconds longer, which is judder, not a gap. Buffering to smooth judder buys smoothness with latency, and for a screenshare where someone is being talked through what they are seeing, latency is the thing that hurts.

There is also nothing to time-stretch. NetEq's whole trick — expand and compress audio without changing pitch — has no video analogue, so a video jitter buffer is a plain delay line with none of the cleverness.

**What is actually needed is ordering, and that is not the same thing.** Datagrams reorder, so fragments of frame N+1 can complete before frame N does. A decoder must be fed in decode order — and since this design disables B-frames and lookahead, decode order is capture order, so the buffer only has to sort, never reorder around dependencies. The rules:

- Reassemble per access unit; a unit is complete when it holds an unbroken `seq` run from a first-marked to a last-marked fragment.
- Emit units in increasing `seq` order, never out of order.
- If a unit is incomplete but a later one is ready, wait **50 ms**. One constant, no framerate arithmetic.
- On that deadline, abandon the incomplete unit, emit the next, and request a keyframe (rate-limited, see *Loss repair*).
- Drop fragments belonging to a unit older than the last one emitted. They are stale by definition and nothing downstream wants them.

**Three stages, and it is worth naming them separately** because "the video buffer" otherwise means three different things in three different conversations:

| Stage | When | Depth |
| --- | --- | --- |
| Reassembly | Always | One access unit — fragments of the frame being assembled |
| Reorder window | Always | **50 ms**: emit in `seq` order, wait no longer than that for a missing unit |
| Sync hold | Only when the share carries audio | Whatever `neteq` settled on for that share's audio track |

The first two absorb reordering, not delay, and exist on every video track. The third is the only place video is deliberately delayed, it is bought for sync inside the content, and it disappears entirely the moment a share has no audio. A silent share is display-on-arrival exactly as described above.

**Why 50 ms rather than the one frame impatience suggests.** The costs on the two sides of this deadline are wildly asymmetric, and the expensive one is being impatient:

- **Waiting costs a bounded hitch.** Later units are already buffered, so the display holds for at most the deadline and then catches up. 33 ms, once, on a path that reordered.
- **Discarding costs a corrupted picture for far longer than that.** Losing one fragment discards the whole access unit — ~37 datagrams — and every later frame referencing it is now decoding against a frame that never arrived. With intra refresh there is no IDR to snap back to, so the error smears until the refresh sweep passes over the damaged region, which is hundreds of milliseconds of visible corruption on motion-heavy content. Trading a 50 ms hitch for that is a bad trade in every direction.

The one-frame rule came from a real principle — do not turn a reorder window into a latency buffer — but applied it to the wrong quantity. What must not grow without limit is the *steady-state* delay. A deadline is not steady-state delay: it is paid only when something is actually missing, and on a clean path it is never paid at all.

**A flat constant, not a multiple of the frame interval**, for two reasons. The receiver does not know the sender's framerate — it is a local setting and appears nowhere on the wire, so a frame-relative deadline would need either a new protocol field or an estimate inferred from arrival timing, which is the one signal a jitter deadline must not trust. And the scaling it would provide is already there: the clock only starts once a *later* unit is complete, so a 15 fps sender cannot trigger an early deadline no matter what the constant says. The trigger condition is framerate-relative even though the timeout is not.

**Then measure it rather than arguing about it.** Count two things per track: units abandoned on the deadline, and fragments that arrive *after* their unit was abandoned. The second is the whole answer — a nonzero late-arrival count is direct evidence the deadline is too tight, and a count of zero means it is already generous and the losses are real losses that no amount of waiting would recover. Both are free, since the reassembler already recognises stale fragments in order to drop them.

**If judder turns out to be objectionable, the fix is on the sender.** A 1440p frame is ~37 datagrams; send them back-to-back and they arrive as a burst whose spread *is* the judder. Pacing the fragments across the frame interval costs nothing and attacks the cause. Reach for that before adding receive-side delay — and if a fixed delay is ever added anyway, make it one frame and fixed, never adaptive.

Be precise about which clock is forbidden here, because the two cases look alike and are not. Holding a share's video against **its own** system audio is sync inside one source, on one capture clock, and it is what the stage table above already does. Holding it against **the room's voices** is sync across sources that share no clock, which buys nothing and is rejected under *No sync between sources*. The distinction is the source, not the act of waiting.

### The SFU

The entire media path: read datagram, parse 13 bytes, check the sender owns the claimed `media_id`, look it up in the forwarding table, write the datagram unmodified to each subscriber. It never touches bytes past offset 13. Keeping that literally true is what makes the payload format the client's business alone, and why encryption can be dropped in later without the server changing at all.

The ownership check is not optional — without it any authenticated user can inject into anyone else's stream. It is cheap: each connection knows its own current `Tracks`, so the check is a compare against that connection's own two or three ids, not a global reverse lookup.

Consequences worth accepting up front: no server-side transcoding, no simulcast layer selection, no bandwidth adaptation beyond what QUIC congestion control gives per connection. One encoding per track, sent to everyone. Right trade for a handful of participants; a room of forty would need more.

### Frame transform hooks

Fragmentation is the only place the payload is touched, so the transform hook lives there — after fragmenting outbound, before reassembling inbound — behind one trait:

```rust
pub trait FrameTransform: Send {
    /// after fragmenting, before the header is prepended and the datagram sent
    fn outbound(&mut self, hdr: &FrameHeader, buf: &mut Vec<u8>) -> Result<()>;
    /// after the header is stripped, before reassembly
    fn inbound(&mut self, hdr: &FrameHeader, buf: &mut Vec<u8>) -> Result<()>;
    /// bytes this transform may add, reserved in the MTU budget
    fn overhead(&self) -> usize;
}

pub struct Passthrough;
impl FrameTransform for Passthrough {
    fn outbound(&mut self, _: &FrameHeader, _: &mut Vec<u8>) -> Result<()> { Ok(()) }
    fn inbound (&mut self, _: &FrameHeader, _: &mut Vec<u8>) -> Result<()> { Ok(()) }
    fn overhead(&self) -> usize { 16 }
}
```

The hooks are called unconditionally and run `Passthrough` today. There are exactly two call sites — one in the fragmenter, one in the reassembler — which is the whole benefit of owning the payload path: no plugin to register, no element to write, nothing to install. The buffer is handed over as a growable `Vec` because any real transform will append bytes.

`Passthrough::overhead()` returns 16 rather than 0 on purpose: the fragment size already accounts for an AEAD tag, so enabling encryption later does not shift packet sizes, retune the encoder, or turn working packets into silently-dropped oversized ones.

### Loss repair

Datagrams do not retransmit, so a lost keyframe freezes a receiver until the encoder produces the next one. Three cheap measures:

- A receiver that sees a gap in `seq` and holds no valid reference frame sends `RequestKeyframe(media_id)`; the server routes it to that sender as `KeyframeWanted`, forcing an IDR. Rate limit to one per second per `media_id`. Detect the gap from the frame header `seq`, not from the jitter buffer — the header is read before the payload reaches a decoder at all, and reassembly already has to notice the gap to discard the access unit.
- **Intra refresh rather than periodic keyframes.** VA-API, NVENC and Media Foundation can all spread intra-coded blocks across a rolling band of frames instead of emitting whole IDRs. That turns the recovery cost from a periodic spike into a flat surcharge, which matters at 20 Mbps because a full 1440p keyframe is a burst large enough for congestion control to clip — meaning the packets most needed for recovery are the ones most likely to be dropped. Content being motion-heavy rather than static doesn't change this; it only makes the surrounding P-frames larger too.
- Audio FEC, because voice dropouts are far more noticeable than a stuttering picture.

**A new subscriber gets an IDR on request, not by waiting for one.** The obvious design — the server holds forwarding until it sees a frame with the keyframe bit set — breaks the moment intra refresh is enabled, because there is then no IDR to wait for and the gate never opens. Instead, when a peer enters a room the server sends `KeyframeWanted` to everyone sharing in it and forwards everything meanwhile. The newcomer sees a moment of nothing rather than a stall, the sharers each produce one real IDR, and the server needs no keyframe logic and no per-track-kind special-casing at all. Keep bit0 defined and set by senders — it costs nothing and a receiver-side use is plausible — but don't route on it.

### Presenting frames without a CPU round trip

The naive path is GPU → CPU → GPU. A hardware decoder produces a frame in video memory, `gldownload` or `videoconvert` drags it into system memory, it is copied into a `Vec`, and the UI framework uploads it back to the GPU. At 1440p60 that is two 14.7 MB transfers per frame in opposite directions — around 1.8 GB/s of traffic that accomplishes nothing — and with motion-heavy content there is no framerate cap to hide behind. This has to be solved rather than mitigated.

**A small patch on upstream gpui — not a divergent fork.** The renderer-side capability is already present: gpui's Linux platform renders through wgpu and its surface support is largely built. What is missing is the *exposure* — `Window::paint_surface()` is wired up on macOS only, so the Linux path has to be plumbed back out to that same public entry point. That patch has been written and works. Windows needs the identical treatment against its D3D11 renderer.

The distinction from a fork like `gpui-ce`/`gpui-wgpu` matters and is the whole reason this is acceptable. Those replace the platform layer with `winit` and the text stack with `cosmic-text` — a rewrite of the two subsystems that decide whether an app feels native, and a permanent divergence. This is a mechanical patch that connects existing internal capability to an existing public API. No new concepts, no architectural drift, and a diff small enough to read in one sitting.

| Platform | Renderer | `paint_surface` | Work |
| --- | --- | --- | --- |
| macOS | Metal | exposed upstream | none; VideoToolbox's `CVImageBuffer` is what it already takes |
| Linux | wgpu | renderer support present, not exposed | patched and working |
| Windows | D3D11 | not exposed | same patch, second time. First check whether the renderer has the underlying support the way wgpu did, since that decides whether this is plumbing or implementation |

**Try to upstream it.** This is a far better candidate than the things upstream has declined. Custom shaders and tray support were rejected as features Zed does not use; exposing `paint_surface` on the platforms whose renderers already support it is closer to completing an abstraction that is accidentally macOS-only — and Zed wants screen sharing on Linux and Windows itself, so the incentives line up. If it lands, the patch disappears. If it stalls, an open PR still means the diff is public, reviewed, and rebased by more than one person.

**Carrying it, until then.** Keep the fork branch as *only* the patch, rebased onto tagged upstream releases, consumed through a `[patch]` block so `gpui-component` resolves to the same build. Two consequences. Pin to a specific upstream tag and upgrade deliberately: a patch against the platform/renderer boundary is more fragile to refactoring than one against public API, so gpui bumps stop being routine. And if the diff ever grows past "expose what is already there", that is the signal that the approach has drifted into a fork and the cost calculation needs redoing.

Note that `throcc-client/video.rs` containment does **not** insulate you from this one — the patch lives below your code, not in it. What limits the exposure is the patch staying small and targeting a stable public signature.

**The receive path.**

1. **Hardware decode to a GPU surface.** On Linux, VA-API (`vah264dec`, `vah265dec`, `vaav1dec`) produces `video/x-raw(memory:DMABuf)` on Intel and AMD, which is the clean case; NVIDIA's `nvcodec` decoders produce CUDA memory instead, and reaching Vulkan from there means CUDA–Vulkan external memory or a detour through GL textures. On Windows it is D3D11VA or Media Foundation producing an `ID3D11Texture2D`. On macOS it is VideoToolbox producing a `CVImageBuffer`, which `paint_surface` consumes directly. A prototype that works on one GPU vendor is not evidence about the other — the NVIDIA path on Linux and the vendor spread on Windows both need their own confirmation.
2. **Convert and scale on the GPU.** Hardware decoders emit NV12 or P010, which are multi-planar, and wgpu cannot import a multi-planar image as an ordinary texture today. (macOS is the exception: `paint_surface` takes the NV12 buffer as-is and does the colour conversion in Metal, so no postproc pass is needed there.) ffmpeg's hardware scalers — `scale_vaapi`, `scale_cuda`, `scale_d3d11`, `scale_vt` — convert to single-plane RGBA *and* scale to roughly the tile size in one pass, all in video memory, with the same `AVHWFramesContext` the decoder already produced. The alternative — importing Y and UV as separate single-plane textures and doing the colour conversion in a shader — is faster and considerably more work; start with the postproc pass and only write the shader if profiling asks for it.
3. **Import the dmabuf as a `wgpu::Texture`** and hand it to the surface element.

**Two things will bite.**

*Buffer lifetime.* Do not release the `AVFrame` when the frame is handed over — release it when the GPU has finished sampling it, which is at least a frame later. Getting this wrong produces tearing and flicker that looks exactly like a decoder bug, and it also means the decoder's buffer pool needs enough surfaces for the frames in flight.

*Synchronisation.* Most Linux drivers honour dmabuf implicit sync, which is why this works at all without a fence path. Explicit fences — sync fds, drm syncobj — are the correct answer and worth adding if tearing survives a correct buffer-lifetime fix. Don't build them speculatively.

**The fallback ladder,** in order of preference, because not every machine reaches the top:

1. Hardware decode → dmabuf → GPU convert → import. No CPU copies.
2. **Software decode** (`dav1d` for AV1, ffmpeg for H.265 and H.264) → system memory → one upload. One transfer, and crucially *not* a round trip, because nothing is read back from the GPU. This is an acceptable fallback and it is what makes announce-don't-negotiate viable.
3. Hardware decode → download → upload. Strictly worse than software decode. If you end up here, take option 2 instead — it is the one case where using less hardware is faster.

**The send path is symmetric and deserves the same care.** The portal hands out a dmabuf on Linux, Windows Graphics Capture an `ID3D11Texture2D`, ScreenCaptureKit a `CVPixelBuffer`; each is wrapped into an `AVHWFramesContext` and handed to the hardware encoder without the frame touching the CPU. At 1440p60 that is the difference between a client that idles and one that pins a core.

**This is where the missing negotiation is paid for.** Nothing matches the memory type between source and encoder for you: you construct the frames context yourself and import a foreign handle into it, which is `av_hwframe_ctx_init` plus `av_hwframe_map` and is the one genuinely unsafe corner of the client. Confine it to `media/video/capture` behind a trait that yields an `AVFrame` already on the GPU, so the rest of the client never sees a raw handle. The self-view takes a second reference to the same frame — a refcount, not a copy — and goes to the surface element directly.

**The frame channel is still a `watch`, not an `mpsc`.** It now carries a reference-counted GPU buffer handle rather than pixels, which only strengthens the argument: an `mpsc` at capacity drops the *newest* item, which for a display path is backwards, while `watch` overwrites the pending value so a late wake-up sees the current frame. A frame dropped this way costs a buffer release rather than a wasted copy. The network-facing channels stay `mpsc` — there every packet is part of a frame and preferring the old one buys nothing.

**Don't reach for a video-player crate or example**, in gpui or anywhere else. Every one of them is built around a file: seekable source, duration, playback clock, preroll before PLAYING, and deliberate read-ahead to smooth delivery. The read-ahead is what disqualifies them — smoothing delivery is exactly the latency this design spends effort avoiding elsewhere. What is wanted is smaller than a player, not a subset of one: newest frame, now, everything older discarded. Owning the decode loop makes that the natural shape rather than something to configure — pull the frame out of the decoder, hand it to the renderer, drop any frame the renderer has not taken yet. Note in particular that nothing consults a presentation timestamp: a player holds each frame until its PTS, which is latency by design and a stall outright if the timestamps are wrong. This decoder's output is displayed on arrival and its PTS is ignored.

**No sync between sources, deliberately.** A voice from a microphone and a picture from someone's screen share no clock and never did, so nothing tries to align them: voice plays out through `cpal` with `neteq` setting its timing, and a share with no audio of its own is displayed the moment it arrives. Video then runs ahead of the room's voices by roughly the jitter buffer's depth, 50–100 ms — audio *lagging* video, the direction people tolerate and largely do not notice. Buying alignment there would mean buffering video to match: latency in exchange for a defect nobody reported.

Sync *within* a share is a different question with a different answer — see *System audio travels beside the share*.

**The local self-view never touches the network.** `tee` the capture branch before the encoder. Instant, and it removes the most confusing failure available — a preview that stutters because the uplink is saturated, which makes users believe their own capture is broken.

## Client architecture

```rust
// throcc-client-core/lib.rs
pub struct Client { commands: mpsc::Sender<Cmd>, events: broadcast::Sender<Event>,
                    runtime: Runtime }

impl Client {
    pub fn connect(address: SocketAddr, server: &str, keystore: Keystore) -> Result<Self>;
    pub fn cmd(&self, command: Cmd);
    pub fn events(&self) -> broadcast::Receiver<Event>;
}
```

`connect` blocks rather than being `async`, because an `async` one would need an executor above the boundary to drive it — and the whole point of the boundary is that there isn't one. It runs the QUIC handshake, the control stream and the auth exchange on core's own runtime and returns once `AuthResult` has arrived, so a `Client` that exists is a `Client` that is authenticated and placed. That also removes a race: there is no window in which events are emitted before the caller has had a chance to subscribe.

`Cmd` and `Event` are core's own types, not `throcc-proto`'s — core translates, so UI state does not churn every time the wire format moves. Internals are tasks: one owns the control stream, one owns datagram receive, and one per remote track owns its decoder. `mpsc` in, `broadcast` out.

**Two executors, one channel between them.** gpui runs its own executor on its own main thread; quinn needs tokio. So `throcc-client-core` starts a tokio runtime on a dedicated thread and owns everything network- and media-shaped behind it, and the UI never sees quinn or a `Runtime` handle. The channels cross the boundary unchanged, because `tokio::sync`'s channels are runtime-agnostic — they need no reactor to be awaited — so gpui's executor can await a `broadcast::Receiver` directly. Only quinn and `tokio::time` have to live inside the runtime thread.

The UI side is then one long-lived spawned task: await an event, update the root entity, `cx.notify()`. Which is why `Cmd`/`Event` being core's own types matters more with gpui than it would have with iced — the framework boundary is a channel, and nothing about gpui appears below it. Swapping the frontend again would touch `throcc-client` only.

Reconnect re-runs the full handshake with `want_room` set to the last requested room, so a dropped connection is a brief absence from the roster rather than a state to repair. Media ids change on reconnect by construction; peers learn the new ones from `UserEntered`.

## Profiles

Server authoritative: a SQLite row, no signing, no canonical serialization problem.

Avatars are content-hash-keyed blobs, re-encoded server side to a capped size, with only the hash in the profile. Clients cache by hash and never refetch an avatar they have seen.

Display names are not unique. The public key is identity, names are labels. The client shows a short key fingerprint alongside the name in the roster and admin views — cheap now, and the hook any future out-of-band verification would hang off.

## Server storage

```
users(id INTEGER PRIMARY KEY, pubkey UNIQUE, name, avatar_hash, role, created_at)
rooms(id, name, epoch)
invites(secret_hash, role, expires_at, redeemed_by, redeemed_at)
blobs(hash, bytes)
counters(name, value)          -- next media_id
```

`UserId` is the `users` rowid. The pubkey is the identity; the integer is a compact handle for it on the wire.

Alongside the database, the data directory holds `server_key` at mode `0600` — the server's long-lived identity, from which its certificate is regenerated on each start. No certificate is stored.

Everything except `blobs` is small. `counters` holding `media_id` in the database rather than in memory is what makes the never-reuse property survive a restart.

## Server packaging

The server ships as a container image, built from **one** Dockerfile that also produces the development environment. One file rather than two because a `Dockerfile.dev` that drifts from the release build is how "works on my machine" gets reintroduced: the dev stage and the release stage share the same base, the same Rust toolchain and the same dependency layer, and only differ in what they do with them. It lives at `crates/throcc-server/Dockerfile` because the server is the only thing it builds, and is built with the workspace root as context, since cargo-chef needs the whole dependency graph.

The server is the only thing containerised. The client is a desktop app with GPU, audio device and screen-capture-portal access; putting it in a container would mean forwarding all three and would prove nothing.

**Stage layout, in dependency order:**

| Stage | Base | Purpose |
| --- | --- | --- |
| `chef` | `lukemathwalker/cargo-chef:<pinned>-slim` | Shared toolchain base for everything below |
| `planner` | `chef` | `cargo chef prepare` → `recipe.json`, a dependency-only manifest |
| `build` | `chef` | `cargo chef cook --release` (cached layer), then build the workspace |
| `dev` | `chef` | `cargo chef cook` (debug), then `bacon`/`cargo watch` over a bind-mounted source tree |
| `runtime` | `distroless/cc:nonroot` | The release image: the `throcc-server` binary and nothing else |

**Why cargo-chef.** Docker caches by layer, and a naive Rust build invalidates the whole dependency compile whenever any source file changes, because `COPY . .` precedes `cargo build`. cargo-chef splits that: `prepare` reduces the workspace to a skeleton whose only real content is the dependency graph, and `cook` compiles exactly that. The result is a layer keyed on `Cargo.toml`/`Cargo.lock` alone, so touching `server/rooms.rs` recompiles four crates rather than four hundred. This matters more here than for a typical service — `quinn`, `rustls` and `ring`'s C and assembly are a couple of minutes of cold compile that would otherwise be paid on every single build.

**The dev stage watches, it does not bake.** Source is bind-mounted rather than copied, the target directory is a named volume so artifacts survive container restarts, and the entrypoint is a watcher that rebuilds and reruns `throcc-server` on change. The cooked dependency layer means the first build inside a fresh container is a workspace build, not a world build. This is only for the server: the two-CLI-clients-on-localhost loop from the implementation guide still runs on the host, against a containerised server, which is the same shape as production.

**The release image carries a binary and nothing else.** Multi-stage means the compiler, the source and the cargo registry cache all stay in `build`. Runtime needs no media libraries at all — the server never touches media — and no CA bundle either, since the server makes no outbound TLS connections and its own certificate is self-signed from `server_key`. The image is a distroless base and the binary — no shell, no package manager, and an unprivileged user already in place, so the data directory only has to be created with that user's ownership for the mounted volume to inherit it.

**The data directory is the entire persistent state,** and it is exactly the thing that must not live in the container's writable layer. `server_key` is the server's pinned identity: lose it on a `docker rm` and every client hits a pin mismatch warning. Mount it as a named volume or a host path, document that it is what gets backed up, and let the image itself be disposable — which it now genuinely is, since the certificate is derived from the key at every start rather than stored.

**Publish UDP, and mean it.** The listener is UDP/8476 with no TCP fallback, so the port mapping must be `-p 8476:8476/udp`; a default TCP mapping produces a server that starts cleanly, logs nothing wrong, and is unreachable. Container NAT is otherwise a non-issue because the server never advertises an address of its own — clients connect to whatever host and port the operator handed out.

## Room left for E2EE later

Nothing here needs undoing. What is already in place, and why each would have been unpleasant to retrofit:

- **The server never reads past byte 13.** Encrypting the payload changes nothing server-side. Had the SFU depayloaded or inspected frames, this would be a rewrite.
- **The header is already exactly one nonce wide.** `media_id || epoch || seq` is 96 bits with no padding and no rollover counter, the never-reuse rule on `media_id` is what makes it unique without further thought, and the header doubles as AAD so the routing fields are authenticated. Wire format changes are the expensive kind; this is the field this design was most careful about.
- **Media ids are per track, never per user** — see the nonce-reuse argument above.
- **Fragment boundaries are inside the encrypted region**, not in the header's flag bits, so enabling encryption does not leave frame boundaries and frame sizes visible to the server.
- **The MTU budget already reserves 16 bytes** for the AEAD tag, so enabling encryption does not change packet sizes.
- **The transform probes are already installed** and already take a growable buffer.
- **Identity is already an Ed25519 key on an allowlist**, mapping to X25519 for key wrapping with no new enrollment step.
- **The per-room epoch already bumps on membership change**, which is exactly when a room key needs rotating, and it is already in the header for the receiver to select a key by.

What gets added: a `RotateSet` type, three or four control messages to request and distribute wrapped keys, a per-epoch key cache in `throcc-client-core/keys.rs`, and a real `FrameTransform`. Additive, and confined to the client.

The hard part is not the crypto but the trust in the roster. The server is the source of who is in a room, so it can name a member who should not be there, and no wrapping scheme detects that on its own. The honest ceiling: the operator remains trusted for *membership* while the crypto handles *confidentiality against everyone else* — a real improvement over this design, but not the same thing as not trusting the operator.

## What to build first

See the implementation guide for the full milestone breakdown. In outline:

1. QUIC transport with certificate derivation and client-side pinning.
2. Control stream, framing, request correlation. A CLI client, before any GUI.
3. Handshake, invite redemption, roster.
4. `SetRoom`, media id allocation, roster events.
5. Media transport with synthetic payloads — frame header, fragment byte and transform hooks in final form, no codecs yet.
6. Audio end to end.
7. gpui client.
8. Screenshare, then robustness, then admin and deployment.

The frame header and media id granularity go in at step 5 in their final form. Everything else can move.
