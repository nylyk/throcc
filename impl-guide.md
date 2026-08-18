# Implementation Guide

Build order for the voice + screenshare platform. Each substep says **what** you're building and **how** to actually do it. Where the design doc already argues a decision, this guide points at it rather than repeating it — the **why** entries here are only the ones specific to build order and tooling.

## Ground rules

**Build a CLI client before the GUI.** Rendering a room list early means debugging your protocol and your GUI framework simultaneously, and when something breaks you won't know which one is lying. A `throcc-cli` binary that connects, prints events, and reads commands from stdin takes an afternoon and stays useful forever — it's how you'll test the server for the rest of the project. gpui arrives at M7, after the protocol is boring.

**Two clients on localhost is the test rig.** Almost every bug here is an interaction bug. Get to `throcc-server` plus two `throcc-cli` in three terminals as early as possible; that's the loop you'll spend the most hours in.

**`throcc-proto` stays pure.** Types, `postcard` encoding, the frame header codec, the fingerprint function. No tokio, no media, no I/O. The moment it grows a runtime dependency it stops being cheap to share and you'll start putting logic in it.

**Everything in `throcc-proto` gets a round-trip test from day one.** Encode, decode, assert equality. Cheapest tests you'll ever write, and they catch the whole class of "I reordered a field" bugs, which are otherwise silent and awful.

**Write the tests at each milestone yourself, even where you generate the code.** 1.6, 5.3, 5.5 and 5.6 especially. Writing an adversarial dedup-window test forces you to hold the invariant in your head, which is the cheap version of understanding the system; reading a generated implementation of it is not.

**Re-read the design doc's wire-format sections at the start of M5.** The frame header and media id granularity are the only things here that are genuinely expensive to change after clients ship. Everything else is refactorable.

---

## M0 — Workspace skeleton — done

**Goal:** `cargo build` succeeds across a workspace with the right crate boundaries, and nothing else.

### 0.1 Create the workspace

- **What:** `throcc/` with `crates/throcc-proto`, `crates/throcc-server`, `crates/throcc-client-core`, `crates/throcc-cli`, and a stub `crates/throcc-client`. Every package is prefixed and every directory is named after its package, so `cargo run -p throcc-server`, the Dockerfile's `-p throcc-server` and `ls crates/` all name the same thing. The prefix is not decoration: `throcc-proto` and `throcc-client-core` are meant to be publishable, so anyone can build a client against them, and crates.io has one flat namespace.
- **Why:** Boundaries are nearly free now; extracting `throcc-proto` out of a monolith later is a day of pain.
- **How:** Root `Cargo.toml` with `[workspace]`, `members`, `[workspace.dependencies]`, and `resolver = "3"` — every crate here is edition 2024, and a virtual manifest defaults to resolver 1 and warns until you say otherwise. Pin every version in the root; members use `dep.workspace = true`, and inherit `version`/`edition`/`rust-version`/`license` from `[workspace.package]`.

### 0.2 Pin the dependency set

- **What:** One version per crate, in `[workspace.dependencies]`, added the milestone it is first used and not before.
- **Why pin centrally:** Version drift inside one workspace produces type mismatches that read like nonsense — two incompatible `rustls` versions in the tree is the classic, and the error won't tell you that's what happened. Members carry `dep.workspace = true` and no versions of their own.
- **Why not all at once:** an unused pin is a pin nobody has checked. It compiles, so nothing tells you the version is wrong, the features are wrong, or that the crate is no longer the right answer by the time you reach the milestone that needs it — and it costs cold-compile time and audit surface in every build until then. Add the crate in the commit that first calls it, and delete it from both the member and the workspace the moment the last call goes.
- **What M0 and M1 actually need:** `tokio` (rt-multi-thread, macros, sync, time), `quinn`, `rustls` (0.23.x, no default features, one crypto provider chosen explicitly), `rcgen`, `ed25519-dalek`, `rand`, `sha2`, `serde` (derive), `serde_json` (the keystore — see 1.5), `postcard` (alloc), `directories`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `clap` (derive); `tempfile` as a dev-dependency.
- **What waits, and for what:** `rusqlite` (bundled) until M3, when there is a database. `bytes` until M5, when there are datagram buffers to hand around. `data-encoding` only if the invite alphabet in M3 wants it — the fingerprint hex in 1.2 is six lines by hand. `rand_core` never as a direct dependency: it is a version-alignment concern between `ed25519-dalek` and `rand`, not a crate this code calls.
  **Media crates arrive at M6, not now** — same reason — but decide the set here so the shape is known:
  - **Audio, all pure Rust with no build tooling:** `cpal` (devices), `ropus` (Opus as a Rust port rather than a binding — no libopus, no `pkg-config`, nothing to cross-compile), `neteq` (adaptive jitter buffer; already depends on `ropus`, so one codec in the tree), and the `sonora` family for everything between the device and the codec — `sonora` for echo cancellation, noise suppression, gain control and the high-pass filter, `sonora-common-audio` for WebRTC's `PushResampler`.
  - **No `rubato`.** Take the resampler from `sonora` too: a resampler feeding an echo canceller is part of that canceller's signal path, and two vendors' idea of group delay is the kind of mismatch that degrades cancellation with nothing pointing at the cause.
  - **Video:** `rsmpeg` (ffmpeg) for encode and decode **only**, plus the three capture backends — `ashpd` + `pipewire`, `windows-capture`, `screencapturekit`.
  **No `rtp` crate.** Fragmentation is ours (5.6), so there are no payloaders to inherit; see the design doc's *Fragmentation, and why there is no RTP*.
  **ffmpeg is the one build-environment cost, and it lands on the client only.** `rsmpeg` links system ffmpeg libraries, so client builds need them present and client packages need them shipped — a handful of dylibs per platform, no plugin registry and no runtime discovery. Sort it out before M8 rather than during it. Build LGPL-only: hardware encoders, ffmpeg's native decoders and `libdav1d` are all LGPL or BSD, so nothing here requires `--enable-gpl`, and dynamic linking keeps the obligations to shipping the notices and not blocking relinking. The **server** is untouched by all of this — no codec, no ffmpeg, no capture — which is why its Docker image stays a slim base and a binary.
  Three traps, all confirmed against the pinned set (`ed25519-dalek` 3, `rand` 0.10, `rcgen` 0.14, `rustls` 0.23):
  - **`rand_core` majors must agree.** `ed25519-dalek` 3 and `rand` 0.10 both sit on `rand_core` 0.10. Bump one alone and the RNG stops satisfying the bound on `generate`, with an error that never names the two crates that disagree.
  - **The OS RNG is fallible now, and that makes it the wrong one.** Under `rand_core` 0.10 the system source (`rand::rngs::SysRng`, re-exported from `getrandom`) implements `TryCryptoRng` with error type `SysError`, not the infallible `CryptoRng` that `SigningKey::generate` asks for — `expected Infallible, found SysError`. Use `rand::rng()`: infallible, OS-seeded, reseeding. Note the name change too — there is no `rand::rngs::OsRng` any more.
  - **Pick the rustls crypto provider by name, not by default.** With `default-features = false` there is no process-wide provider installed, so `ServerConfig::builder()` panics at runtime rather than failing to compile. Use `builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))` on both sides. `ring` over `aws-lc-rs` because aws-lc-rs wants cmake and nasm on Windows.

### 0.3 Logging and error types

- **What:** `tracing-subscriber` in every binary honouring `RUST_LOG`; `thiserror` enums for library errors (`throcc_proto::Error`, `throcc_client_core::Error`), `anyhow` at binary boundaries.
- **Why:** `println!` will not survive two clients and a server interleaving output, and you'll want per-connection spans before you know you want them.
- **How:** `tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init()`. Wrap each connection task in `tracing::info_span!("conn", peer = %addr)`.

### 0.4 One Dockerfile for the server

- **What:** `crates/throcc-server/Dockerfile` with a `dev` stage that watches and reruns, and `build`/`runtime` stages that produce the release image. cargo-chef underneath both. (Design: *Server packaging*.)
- **Why now rather than at M10:** the dependency-caching layer is the point, and it pays from the first build — `quinn`, `rustls` and `ring` are minutes of cold compile that you would otherwise pay on every image build for the rest of the project. Retrofitting cargo-chef later also means rewriting the `COPY` order, which is the part that is easy to get subtly wrong. One file rather than a separate `Dockerfile.dev`, so the dev environment cannot drift from what ships.
- **How:**

```dockerfile
FROM lukemathwalker/cargo-chef:latest-rust-1.97-slim-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS dev
RUN cargo install cargo-watch --locked
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json -p throcc-server
CMD ["cargo", "watch", "-w", "crates", "-x", "run -p throcc-server"]

FROM chef AS build
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p throcc-server
COPY . .
RUN cargo build --release -p throcc-server
RUN mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build --chown=nonroot:nonroot /data /data
COPY --from=build /app/target/release/throcc-server /usr/local/bin/throcc-server
CMD ["/usr/local/bin/throcc-server"]
```

- **The Dockerfile sits in the server crate, the build context is the workspace root.** cargo-chef needs `Cargo.lock` and every member manifest to compute the dependency graph, so `docker build -f crates/throcc-server/Dockerfile .` — which is what `docker-compose.yml` declares.
- **The `COPY` order is the whole trick.** `recipe.json` comes from `planner` and contains only the dependency graph, so the `cook` layer's cache key is `Cargo.toml`/`Cargo.lock` and nothing else. `COPY . .` must come *after* `cook`, never before — reverse those two and you have rebuilt the naive Dockerfile with extra steps and no cache hits. The `dev` stage has no `COPY . .` at all, because the source arrives as a bind mount.
- **The cargo-chef base already carries the C toolchain.** `ring` compiles C and assembly from BoringSSL, and `rusqlite` will compile SQLite from C once it arrives at M3; `gcc` plus the libc headers ship in the `slim` image — so there is no `apt-get` layer, and adding one is how you find out you did not need it.
- **The runtime user is the base image's, not one you create.** distroless `:nonroot` runs as uid 65532 and has no shell to `useradd` with. Creating `/data` in the `build` stage and copying it across with `--chown=nonroot:nonroot` is what gives the mounted volume an owner the server can write `server_key` into.
- **Run it with the dev stage bind-mounted** and a *named volume* on `/app/target`. The named volume is not optional: without it every container start recompiles from scratch, and on macOS and Windows a bind-mounted target directory is also punishingly slow.
- **Publish UDP explicitly:** `-p 8476:8476/udp`. Docker defaults to TCP, and a TCP-only mapping yields a server that starts, logs nothing wrong, and is silently unreachable — which is indistinguishable from every other UDP-blocked failure in this project. The Dockerfile carries no `EXPOSE` and no `VOLUME`; both are documentation-only directives and `docker-compose.yml` is where the ports and the volume are actually declared.
- **Keep the toolchain tag pinned** and bump it deliberately, since a toolchain change invalidates every layer below `chef`. Keep the runtime's Debian generation matched to the builder's, too — a binary built on bookworm wants `cc-debian12`, and a trixie build wants `cc-debian13`.

**Done when:** `cargo build --workspace` is clean, `cargo test --workspace` runs zero tests successfully, `docker build -f crates/throcc-server/Dockerfile --target runtime .` produces an image that runs the server binary, and editing a source file with the dev container running triggers a rebuild and restart without recompiling dependencies.

---

## M1 — QUIC transport with pinned identity — done

**Goal:** A client connects over QUIC, both sides log "connected", and the connection is refused if the server's key changes. Deliberately auth-free — TLS and QUIC setup has enough sharp edges alone.

### 1.1 Server keypair and derived certificate

- **What:** On startup load `server_key` from the data directory or create it at `0600`. Derive a self-signed cert from it in memory every start. (Design: *Server identity*.)
- **How:** `rcgen` can build params around an existing keypair. Write the key with permissions set at creation (`OpenOptions::new().mode(0o600)`) rather than chmod-ing after, so there's no world-readable window. Log the SPKI fingerprint at startup.

### 1.2 Fingerprint computation, shared

- **What:** One function in `throcc-proto/fingerprint.rs`: certificate DER → SPKI hash.
- **Why:** Client and server must agree byte-for-byte on what "the fingerprint" means. Two implementations will diverge and the symptom is an unexplainable pin mismatch.
- **How:** Extract the SubjectPublicKeyInfo from the cert DER, `sha2::Sha256`, render lowercase hex for display. This is the one place `throcc-proto` needs a DER reader — either a hand-rolled walk to the SPKI element or a small parser crate; don't pull in anything with an I/O or runtime dependency. Unit-test against a fixed cert so a refactor can't silently change the value.

### 1.3 Server endpoint

- **What:** A `quinn::Endpoint` in server mode on UDP.
- **How:** `rustls::ServerConfig` with the derived cert chain and key, `alpn_protocols = vec![b"throcc/1".to_vec()]`, wrapped via quinn's `QuicServerConfig`, then `Endpoint::server`. QUIC requires ALPN — omit it and the handshake fails with an error that doesn't mention ALPN. Accept in a loop, spawn a task per connection. Datagrams need no enabling — quinn's default `TransportConfig` already sizes both datagram buffers — so leave the transport config alone until M5 picks those sizes deliberately.

### 1.4 Pinning verifier on the client

- **What:** A `rustls` `ServerCertVerifier` comparing the SPKI hash and checking nothing else — no chain, no hostname, **no expiry**. (Design: *Server identity* for why expiry checking is off.)
- **How:** Implement in `throcc-client-core/connection.rs`, installed via `ClientConfig::dangerous().with_custom_certificate_verifier`. You must also implement `verify_tls12_signature`, `verify_tls13_signature`, and `supported_verify_schemes` — delegate the signature ones to the crypto provider's default helpers rather than writing them. `server_name` on `connect` is a fixed placeholder.

### 1.5 Keystore with `known_servers`

- **What:** A client-side file holding the identity keypair (generated here, used in M3), a map from server address to pinned SPKI hash, and later the device/mute/bitrate settings from 7.4.
- **Why:** Pin-on-first-use needs somewhere durable, and generating the identity key now means M3 is purely about using it.
- **How:** `directories::ProjectDirs` for the path, JSON (this is the file you end up reading by hand when a pin goes wrong, and it is far too small for the encoding to cost anything), mode `0600`, written to a temp file and renamed so an interrupted write cannot truncate away the identity key. On connect: no entry → accept and store; entry matching → proceed; mismatch → hard error carrying both fingerprints so the caller can offer re-accept.
- **Key the pin on a stable label, not on the socket address.** Store it under the `host:port` the user was given, and pass that label to `connect` alongside the resolved `SocketAddr`. A server that changes port or resolves to a different address is still the same server, and — the practical version — the pinning test cannot rebind on the same ephemeral port.
- **`THROCC_KEYSTORE` overrides the path.** That one environment variable is what lets two `throcc-cli` processes with separate identities run on one machine, which is the test rig from the ground rules.

### 1.6 Prove the pin actually works

- **What:** An integration test that connects twice, deleting `server_key` in between, asserting the second connect fails with a mismatch.
- **Why:** Pinning code that has never seen a mismatch is pinning code that doesn't work. It's the one security property of this milestone and it's trivially testable.
- **How:** `crates/throcc-client-core/tests/pinning.rs`, both endpoints in-process on `127.0.0.1:0`, `tempfile::TempDir` for both data directories. This is why the server is a library with a thin `main.rs` over it, and why `throcc-client-core` takes `throcc-server` as a **dev**-dependency: the test needs to stand a real server up next to the client, and a binary-only crate cannot be linked into one.
- **Assert that a *refused* connection leaves the stored pin untouched.** Silently overwriting on mismatch would defeat the whole mechanism while still passing a naive test. Cover the move case too — same identity, new port — because the pin is keyed on the label the user typed, not on the socket address.

**Done when:** two terminals show a completed handshake, and the mismatch test passes.

---

## M2 — Control stream and framing — done

**Goal:** Typed request/response/event messages flow over one bidirectional stream in both directions.

### 2.1 Length-delimited framing

- **What:** `throcc-proto/framing.rs`: write a `u32` big-endian length then the postcard bytes; read the reverse.
- **Why:** QUIC streams are byte streams, not message streams. Without explicit framing you get partial reads that decode into garbage under load and never in testing.
- **How:** Async helpers over quinn's `SendStream`/`RecvStream`. **Check the length against the 1 MiB cap before allocating** — one line, and the difference between a protocol and a remote memory-exhaustion vector.

### 2.2 Message types

- **What:** `ServerHello`, `Auth`, `AuthResult`, `Req`, `Resp`, `Placed`, `Event` in `throcc-proto/msg.rs`; `UserId`, `RoomId`, `MediaId`, `Epoch` newtypes in `throcc-proto/ids.rs`.
- **Why:** Newtypes because you will otherwise pass a `RoomId` where a `MediaId` belongs, and both are integers so the compiler won't save you.
- **How:** `#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]`; newtypes also get `Copy`, `Eq`, `Hash`, `Ord`. Round-trip test every variant. Note `Placed` is shared by `AuthResult::Ok` and `Resp::Placed` — one type, one construction path.

### 2.3 Request/response correlation

- **What:** `ReqEnvelope { id, req }` / `RespEnvelope { id, resp }`, client-allocated monotonic ids, server echoes. `Event` has no id. (Design: *Request correlation*.)
- **How:** Client keeps `HashMap<u32, oneshot::Sender<Resp>>` and completes by id. An id with no pending entry is a protocol error: log and drop the connection, don't ignore it. Server side the id is opaque — read it off the envelope, hand `req` to the handler, put it back on the reply; handlers never see it. Wraparound is fatal, not a reuse.

### 2.4 Connection tasks and the runtime boundary

- **What:** Server: a per-connection task reading requests in a loop. Client: a tokio runtime on its own dedicated thread, one task owning the control stream, `mpsc<Cmd>` in, `broadcast<Event>` out.
- **Why:** gpui has its own executor and owns the main thread, so `throcc-client-core` cannot assume it's being driven by tokio from above — it has to bring its own runtime. Doing that now, while the only consumer is the CLI, means M7 is purely UI work. And `tokio::sync` channels are runtime-agnostic (awaiting one needs no reactor), so the same two channels work unchanged from the CLI, from gpui's executor, and from tests.
- **How:** `tokio::select!` over the command channel and the stream reader. Public surface is `Client::cmd()` and `Client::events()`; `Cmd` and `Event` are core's own types, translated from `throcc-proto`, so UI state doesn't churn when the wire format moves. Nothing above `throcc-client-core` ever sees a `quinn` type or a `Runtime` handle — if it does, the boundary has leaked.

**Done when:** `throcc-cli` sends a hardcoded no-op request and prints the response.

---

## M3 — Authentication and enrollment

**Goal:** Only allowlisted keys connect. New users enroll with a one-time invite code.

### 3.1 Client identity key

- **What:** Ed25519 keypair generated on first launch into the keystore.
- **How:** `ed25519_dalek::SigningKey::generate(&mut OsRng)`. Store the 32-byte seed, not the expanded form. Never log it. `UserId` is the `users` rowid assigned at enrollment, not derived from the key — the pubkey is the identity, the integer is a compact handle for it on the wire and in foreign keys.

### 3.2 Handshake with exporter binding

- **What:** Server sends `ServerHello { server_nonce, proto }` on accept. Client replies with `Auth { pubkey, client_nonce, invite_code, want_room, sig }`, signing `b"throcc-auth-v1" || server_nonce || client_nonce || tls_exporter`. (Design: *Auth* for what each element does.)
- **How:** `Connection::export_keying_material(&mut buf, label, context)` on both sides with identical label and context. Verify the signature *before* touching the database. `want_room` is `None` on a first connect and set on reconnect (9.2).

### 3.3 Allowlist and roles

- **What:** `users(id INTEGER PRIMARY KEY, pubkey UNIQUE, name, avatar_hash, role, created_at)` and rank checks in one module.
- **Why:** One `perms.rs` states "you can only act on strictly lower ranks" once, which gets "admins can't remove other admins" with no fourth role and no special cases.
- **How:** `rusqlite` with `bundled`. Roles as integer ranks: User 0, Manager 1, Admin 2. Every mutating handler calls one `require_rank_above(actor, target)`. Resist inlining the check "just this once".

### 3.4 Invite codes

- **What:** Six characters from `OsRng` over the 36-symbol alphanumeric alphabet (`K7QM2X`), hash stored, single use, default 24h TTL. (Design: *Invites and roles*.)
- **How:** Draw one byte per character and reject values at or above 252 before taking it modulo 36 — 36 does not divide 256, so masking or a bare modulo skews the first four symbols and quietly costs you entropy you cannot spare at this length. Uppercase on input, trim whitespace, and accept lowercase. `invites(secret_hash, role, expires_at, redeemed_by, redeemed_at)`. Redemption is one SQL transaction: check unexpired and unredeemed, insert the user, mark redeemed — mark, don't delete, so the enrollment trail survives; prune expired and redeemed rows on a timer.
- **The rate limit ships with the feature, not after it.** Six characters is ~31 bits, so a global failure budget is load-bearing rather than hygiene: per-address limits alone are defeated by rotating IPs. Refuse all redemption once the server-wide budget is exceeded, until an operator clears it, and log every failure with its source. (Design: *Invites and roles* for the arithmetic.)

### 3.5 Bootstrap invite

- **What:** On an empty users table, mint an Admin invite and write it to a `0600` file in the data directory.
- **Why:** Otherwise there's no way in. A file rather than stdout because stdout on a systemd service lands in the journal, which is readable by more people than you think.
- **How:** Log the *path*, never the code.

### 3.6 `AuthResult::Ok` carries the world

- **What:** `me`, `role`, full user list, room list, `placed`.
- **Why:** One message and the UI can render. A separate bootstrap fetch is a second round trip and a second code path that can disagree with the first.
- **How:** Assemble it inside the same transaction that resolved the auth, so the roster can't shift between checking and sending.

**Done when:** an unknown key is rejected, the bootstrap code enrolls one admin, the code fails on second use, and a reconnect with the stored key succeeds with no code.

---

## M4 — Rooms and membership

**Goal:** Clients move between rooms, everyone sees an accurate roster, and media ids are allocated.

### 4.1 Room CRUD

- **What:** `CreateRoom`, `RenameRoom`, `DeleteRoom`, gated by Manager rank. `rooms(id, name, epoch)`.
- **Why:** Rooms are persistent and may sit empty, so they cost a row rather than being implied by presence, which makes the room list stable and bookmarkable.
- **How:** No `is_default` column and no protected room — nothing auto-joins, so there is nothing for a default to be the target of. Deleting an occupied room moves its occupants to `None` and fans out the event; `None` is an ordinary state, so this needs no special handling.

### 4.2 `SetRoom` as one transaction

- **What:** `SetRoom(Option<RoomId>)`, the only membership operation. (Design: *Rooms and membership*.)
- **How:** One in-memory lock over the room registry for the whole transition: remove from old, add to new, bump both epochs, allocate tracks, build the reply. Fan out `UserExited` to the old room and `UserEntered` to the new one after committing, never during.

### 4.3 Media id allocation

- **What:** A fresh `Tracks { mic, shares }` on every membership change, from a monotonic `u32` in `counters`, **never reused**. (Design: *Media id allocation* for why never-reuse is stricter than routing needs.)
- **How:** The counter lives in SQLite, not memory, so the property survives a restart. Allocate inside the `SetRoom` transaction. Treat exhaustion as a fatal error rather than wrapping.
- **Shape it as `{ mic: MediaId, shares: Vec<Share> }`, where `Share { video: MediaId, audio: Option<MediaId> }`, with one share.** The nesting is what tells a receiver which sound belongs to which screen, and it is why system audio (8.4) needs no protocol change beyond a boolean. Camera getting dropped from the design already demonstrated that a fixed field set isn't stable, and the second monitor is the obvious next request. `Tracks` is what the wire format, the routing table and the ownership check are all built on, so it's the piece that has to be able to grow; get the vector in *here* and adding `OpenShare -> MediaId` later is a new enum variant instead of a wire break.

### 4.4 Epoch

- **What:** Per-room counter, bumped on every membership change, in `Placed` and membership events. Senders stamp the latest value seen; receivers ignore it. (Design: *Epoch*.)
- **How:** Persist it. Never decrement it. Overflow is fatal.

### 4.5 Placement on auth

- **What:** Place the client in `want_room` if it asked for one, otherwise in no room. (Design: *Rooms and membership*.)
- **Why:** Connecting is not entering. A client connects on launch, on wake and on every reconnect, and none of those are a decision to walk into a room with people in it. `want_room` is set only by a reconnect restoring where you already were (9.2), so a first connect always lands in `None`.
- **How:** Reuse the `SetRoom` path rather than duplicating it, including for the `None` case, so there is exactly one construction site for `Placed`. If `want_room` names a room that no longer exists, place in `None` rather than substituting another room — silently putting someone somewhere they didn't ask for is the behaviour this removes. **Mic and screen both start off**, and with no room on connect no capture device is opened at all.
- **Check the whole path handles `None` from the start.** It's now the common case, not an edge: no room in `Placed`, no `Tracks` allocated, no subscriber list entry, no `UserEntered` fanout, and the CLI must print something sensible rather than assuming a room. Getting this right in M4 is what stops `Option<RoomId>` being unwrapped in six places by M8.

**Done when:** a client authenticates, is in no room, sees the full roster and room list, then enters a room explicitly; two CLI clients move between rooms and `None` and each sees the other's transitions, with no roster divergence after fifty random moves.

---

## M5 — Media transport, without codecs

**Goal:** Synthetic packets flow client → SFU → client with correct routing, sequencing, dedup and fragmentation. This milestone exists to separate transport bugs from media bugs; skipping it means debugging both at once, through a codec that hides your packets.

### 5.1 Frame header codec

- **What:** `throcc-proto/frame.rs`: `media_id: u32`, `epoch: u32`, `seq: u32`, `flags: u8` — **13 bytes**, fixed width, big endian. (Design: *Protocol* for why 13 and not 17, and why not postcard.)
- **How:** Manual `to_be_bytes` into `[u8; 13]`. Reject short buffers. Clients reject unknown flag bits; the server reads bit0 and ignores the rest, so a bit can be introduced without a server upgrade. Round-trip test, plus a test asserting the encoded length is exactly 13 so nobody widens a field without noticing.
- **Why the width is load-bearing:** `media_id || epoch || seq` is exactly 96 bits, which is a ChaCha20-Poly1305 nonce with no padding and no rollover counter. Widen `seq` to a `u64` and you've paid four bytes per packet forever for nothing; narrow anything and the nonce needs contrivance. This is the one field layout in the project you should treat as frozen once it ships.

### 5.2 SFU forwarding

- **What:** Read datagram, parse 13 bytes, **verify the sender owns the claimed `media_id`**, look it up, write the datagram unmodified to each subscriber.
- **Why:** The server never touching bytes past offset 13 is what makes the payload format the client's business alone. The ownership check is what stops any authenticated user injecting into anyone's stream.
- **How:** `Connection::read_datagram()` in a per-connection task. Ownership is a compare against that connection's current `Tracks` — one field and a short slice — held on the connection state — no global reverse map. Fanout from per-room subscriber lists rebuilt on membership change, so the hot path doesn't lock the world. Drop unknown ids silently.

### 5.3 Sequence numbers and the dedup window

- **What:** `seq` monotonic per `media_id`, never resetting. Receivers keep a 64-entry sliding window per `media_id` and drop anything already seen or below it. (Design: *Sequence numbers*.)
- **How:** A `u64` bitmask plus a highest-seen value. Unit-test directly with an adversarial sequence: duplicates, far-past, far-future, window wraparound. Expose a "gaps that never filled" count off the same structure — that's the receiver-side loss metric in 9.3, and it's free here.

### 5.4 Fixed datagram budget

- **What:** One compile-time budget. **Not 1200 minus the header — measure first.** QUIC's 1200 is the *UDP datagram*; a DATAGRAM frame's payload is what is left after QUIC's own packet overhead, and quinn reports `max_datagram_size() == 1162` at default settings (confirmed on loopback at M1). So the budget is `1162 - 13 - overhead()` = **1133 bytes**, where `overhead()` is 16 even though nothing uses it yet. Assert at handshake that the peer's `max_datagram_size()` is at least `1162`.
- **Why 1162 rather than whatever the connection reports:** 1200 is QUIC's floor for the UDP datagram, so 1162 is a floor too, and a floor is the only safe basis for a fixed budget — MTU discovery may raise the real figure later in a connection, and a sender that took the raised value would emit packets some receivers cannot accept. Take the floor once, in a `const`, and ignore the rest.
- **Why:** Do **not** size per-connection from `max_datagram_size()`. The SFU forwards bytes unmodified and cannot re-fragment, so if one sender negotiates a larger size than some receiver accepts, its packets are undeliverable to that receiver only — silently, with no error at either end. A fixed conservative frame removes the whole failure mode. Reserving the 16 bytes now means enabling encryption later doesn't shift packet sizes or turn working packets into silently-dropped oversized ones.
- **How:** Read `max_datagram_size()` after the handshake purely to validate it. Treat `None` (no datagram support) and an under-budget value as fatal, clearly-worded errors. Put the budget in one `const` in `throcc-proto` so the fragmenter in 5.6 derives from it rather than repeating the arithmetic. One of those 1133 bytes is the fragment byte, leaving **1132** for codec output.

### 5.5 Synthetic loopback test

- **What:** Two clients, one sending counted dummy payloads on three media ids, the other asserting in-order receipt with no duplicates. Plus a test that a sender claiming someone else's `media_id` is dropped.
- **Why:** Last moment when you can be certain the transport works before real codecs enter the picture and start hiding your packets.
- **How:** Integration test plus a `--synthetic` flag on `throcc-cli` so you can watch it by hand.

### 5.6 Fragmentation

- **What:** `throcc-client-core/media/fragment.rs`: split an access unit into 1132-byte payloads each preceded by a fragment byte, and reassemble the reverse. Bit0 = first fragment, bit1 = last, **bit2 = a 4-byte 90 kHz capture timestamp follows** (first fragment only, and only on tracks that need sync), rest reserved. (Design: *Fragmentation, and why there is no RTP*.)
- **Why it's in M5 and not M6:** it is transport, not media — it has no codec in it, it is pure and trivially testable, and having it before any encoder exists means the synthetic loopback in 5.5 exercises the real payload path rather than a stand-in that gets deleted.
- **Why the fragment byte is in the payload, not the header's spare flag bits:** the header is plaintext forever, and first/last markers there would hand the server exact frame boundaries and sizes — traffic analysis against the party the future encryption is meant to exclude. One byte inside the encrypted region costs 0.09%.
- **How:**
  - No fragment index. `seq` is monotonic per `media_id` and never resets, so one access unit is a contiguous `seq` run from a first-marked to a last-marked packet. Reassembly is: buffer per `media_id`, emit when the run is unbroken, discard the whole unit on a gap.
  - **Emit in `seq` order, never as units happen to complete.** Datagrams reorder, so unit N+1 can finish before N. A decoder fed out of order produces garbage or wedges. Audio does not care — one packet is one unit and NetEq sorts them — but the same reassembler serves video, so make ordered emission the reassembler's contract rather than something the video path adds later. (Design: *Video gets a reorder buffer, not a jitter buffer*.)
  - Cap the reassembly buffer and evict by age, or a peer that never sends a last-marked fragment is a memory leak. A 1440p60 access unit is tens of packets; anything holding hundreds is broken, not slow.
  - Fragment into `bytes::Bytes` slices of the single encoder output buffer. `Bytes::slice` is refcount arithmetic; building a `Vec` per fragment is a copy, and `send_datagram` would then copy again.
  - Test it against the adversarial cases directly, and write these yourself: reordered fragments, a missing middle fragment, a missing last fragment, duplicate fragments, two access units interleaved, and a unit that is exactly one fragment (both bits set — the audio case, which is 99% of packets by count).

**Done when:** the loopback test passes, a third client in a different room receives nothing, a spoofed `media_id` is rejected, and the fragmenter round-trips a 400 KB synthetic access unit through reassembly with reordering applied.

---

## M6 — Audio end to end

**Goal:** Two people can talk. No ffmpeg in this milestone at all — audio is four Rust crates and no C.

### 6.1 Audio devices

- **What:** `cpal` for enumeration, capture and playback, exposed from `throcc-client-core` as plain data so the UI in 7.4 renders a list without knowing what `cpal` is.
- **Why first:** a wrong default device is the single most common "it's broken" report, and device handling is where the platform differences actually live — not in the codec.
- **How:** Enumerate at startup and on device-change notifications; don't cache a device handle across a disconnect, because unplugging a headset invalidates it. Ask for 48 kHz mono; if the device won't give it, resample with `sonora-common-audio`'s `PushResampler` rather than running the codec at another rate — 441 in, 480 out per 10 ms block, verified energy-preserving. Log the negotiated config at startup — sample rate, buffer size, channel count — because every later audio mystery starts there.

### 6.2 Encode and send

- **What:** input callback → `ropus` → one datagram per 20 ms frame, through 5.6's fragmenter.
- **How:** `Encoder::builder(48_000, Channels::Mono, Application::Voip)`, `set_bitrate(24_000)`, `set_inband_fec(InbandFec::Enabled)`, `set_packet_loss_perc(10)`. VoIP mode, not Audio mode. One frame is one datagram and always fits — 20 ms at 24 kbps is ~60 bytes against a 1132-byte budget — so audio never actually fragments and both fragment bits are always set.
- **Mute stops sending rather than sending silence.** Saves bandwidth and makes mute a property rather than an inference. Local mute takes effect immediately, before the server round trip.

### 6.3 The realtime boundary

- **What:** the rule at every callback edge: bounded channel, `try_send`, drop on full, count the drop. **This is the substep to get exactly right**; it's the one that eats a day. (Design: *The media/tokio boundary*.)
- **Why:** a `cpal` callback is a realtime audio callback. Any `.await`, `blocking_send`, `block_on`, allocation, or lock held across a syscall risks missing the device deadline, and the symptom is a click or a dropout that sounds like a network problem.
- **How:**
  - `tokio::sync::mpsc::channel(64)`, `Sender` cloned into the callback when the stream is built. Never look up a runtime handle from inside a callback.
  - Input callback: copy samples out, run the echo canceller, encode, `try_send`, return. `Err(Full)` → drop and bump the counter that feeds 9.3. Dropping is correct: the packet is worthless in 20 ms.
  - Output callback: `neteq.get_audio()`, copy into the device buffer, return. Nothing else — no decoding decisions, no locks the network path can hold.
  - No `unwrap`, no indexing, no allocation in either callback. A panic across the FFI boundary is UB.
  - Send `bytes::Bytes`, not `Vec<u8>` — `send_datagram` takes `Bytes` and a `Vec` costs a second copy at the send site.
- **Throughput is not the concern; the deadline is.** Audio is 50 packets a second per peer and a tokio `mpsc` does millions of sends a second. Do not optimise the channel; protect the callback.

### 6.4 The transform hook

- **What:** `FrameTransform` with `outbound`/`inbound`/`overhead`, implemented as `Passthrough` returning 16, called from the two sites in 5.6's fragmenter. (Design: *Frame transform hooks*.)
- **How:** Hand the buffer over as a growable `Vec<u8>`. Wiring it now, while it does nothing, means adding encryption later is one impl block rather than surgery on a working path.

### 6.5 Receive: one NetEq per remote track

- **What:** datagram → dedup window → `inbound` → `neteq.insert_packet()`; output callback → `neteq.get_audio()` → mix.
- **Why NetEq rather than a buffer you write:** a fixed-depth buffer is either too shallow for a bad network or permanent added latency on a good one. NetEq adapts the target depth to observed jitter, time-stretches to reach it without pitch artefacts, and conceals gaps. It's what every browser call uses, and a weekend version of it is the kind of thing that seems fine until someone is on hotel wifi.
- **How:**
  - Construct `AudioPacket` yourself — NetEq takes packets, not RTP off the wire. `sequence_number` is `seq` truncated to `u16`; `timestamp` is `seq * 960` at 48 kHz, which is exact because one datagram is one 20 ms frame and `seq` never resets.
  - One NetEq instance per `media_id`, owned by that track's task. The shared datagram loop only `try_send`s to it — one stalled peer must never stall the room.
  - Enable FEC decode: on a gap, `ropus` can recover the previous frame from the next packet's in-band copy. Verified working — a dropped frame's audio comes back rather than coming back as silence.
  - Create on `UserEntered`/`PeerMedia`, tear down on `UserExited`. Get teardown right now; leaked instances present as slowly worsening audio.

### 6.6 Capture cleanup: echo, noise, gain

- **What:** `sonora` between capture and encode, with the mixed playout as its reference signal. Four stages, one config. (Design: *Audio*.)
- **Why it's not optional:** a meaningful share of desktop users are on speakers, and without AEC everyone else hears themselves. It is also the one piece of plumbing that spans the send and receive paths, so retrofitting it means rethreading both.
- **Why all four rather than only echo cancellation:** they are fields on one `Config`, and the module orders them internally. Noise suppression running before echo cancellation degrades the canceller's estimate, and the resulting echo comes and goes with the speaker's voice — a bug that is miserable to attribute. Take the ordering from the module rather than assembling a chain yourself.
- **How:**
  - `Config { echo_canceller: Some(..), noise_suppression: Some(NoiseSuppression { level: Moderate, .. }), gain_controller: Some(..), high_pass_filter: Some(..) }`. Start at `Moderate`: `High` and `VeryHigh` audibly chew consonants, and the complaint that produces ("they sound underwater") is harder to diagnose than the noise it removed. Noise suppression force-enables the high-pass filter regardless.
  - **The chain is fixed at 10 ms frames** and panics on anything else, while Opus encodes 20 ms — so capture buffers two APM frames per encoded frame. Get this right first; a mismatch that silently processes every second frame sounds like the module "not doing much", not like a bug.
  - Feed `process_render_frame` the same mixed buffer the output callback wrote, before it reaches the device. The reference has to be what was actually played, or the canceller is estimating against the wrong signal.
  - Expose the levels in 7.4's settings, at least as on/off. Noise suppression is the one users argue about — musicians and anyone on a good microphone want it off.
- **Write the acceptance test, and write it here rather than at M9.** `sonora` is a young crate implementing a subtle algorithm, and its failure mode is "people say it sounds weird", which no functional test catches. Thirty lines does it: synthesise a far-end signal, run it through a fake room (35 ms delay, a few decaying reflections, and a `tanh` for loudspeaker nonlinearity — the nonlinearity is the part that separates a real canceller from an NLMS toy), feed render and capture, and assert two things. Echo return loss above a floor once converged, and near-end speech still present when you mix it into the capture. Measured on the current version: 46 dB ERLE in the first second, 73 dB converged, near-end retained at −2.9 dB through double-talk. Those numbers are the regression baseline.
- **The fallback is `webrtc-audio-processing`**, a binding to the C++ original, whose `Config` `sonora` mirrors closely enough that switching is mechanical. It costs meson and ninja in the build on all three platforms, so take it only if the test above starts failing.

### 6.7 Spike: get a decoded frame onto the screen, per platform

- **What:** A throwaway binary, not part of the workspace, that opens a gpui window and displays frames produced by an ffmpeg hardware decoder as a GPU texture through gpui's surface path, with nothing in the path touching system memory. **The renderer half on Linux is done** — the wgpu platform's surface support was substantially already there and only needed exposing through `paint_surface`. The producer half is what this spike proves.
- **Why it's here and not in M8:** it is the highest-risk unknown in the project and the only one that can invalidate a *framework* choice rather than a module. Answer it before building the UI on top.
- **What the producer half involves:** nothing negotiates the memory type for you, so you build the `AVHWFramesContext` yourself and export the decoder's surface with `av_hwframe_map`. That is the whole of it, and it is also the only unsafe code in `throcc-client-core` — `#![forbid(unsafe_code)]` becomes a scoped `#![allow]` in `media/video` and nowhere else.
- **Confirm, in order of leverage:**
  - **Linux/VAAPI.** `av_hwframe_map` with `AV_HWFRAME_MAP_DIRECT` to DRM_PRIME, import as a `wgpu::Texture`. Your renderer patch already works; this proves the producer.
  - **Windows — the same `paint_surface` exposure, second time.** First read whether the D3D11 renderer already has the underlying surface support the way the wgpu platform did; that decides plumbing versus implementation. ffmpeg's `d3d11va` decoder gives an `ID3D11Texture2D`, but note it hands you a *texture array* plus an index rather than a standalone texture — that is the detail that surprises people.
  - **macOS.** `paint_surface` takes a `CVImageBuffer`, which is exactly what ffmpeg's `videotoolbox` decoder produces, so this is likely the least work of the three — confirm it, and confirm it composites where you expect rather than only in Zed's specific usage.
  - **NVIDIA on Linux.** `cuvid`/`nvdec` gives CUDA memory rather than dmabuf, so it is a different path and needs its own test.
- **Harden before trusting it.** Things that pass a demo and fail a call:
  - **Frame lifetime.** Hold the `AVFrame` reference until the GPU has finished sampling, not until you hand it over. Run at 60 fps for an hour with a small decoder pool and watch for intermittent tearing — the symptom appears under load and looks exactly like a decoder bug. Concretely: `av_frame_ref` before handing over, unref on the renderer's completion.
  - **Format modifiers.** Some drivers hand back tiled or compression-modified buffers that import rejects. Works on one driver, fails on the next.
  - **Resize, and moving the window between monitors with different scale factors.**
- **If a platform's surface path is closed:** native child surface first — `AVSampleBufferDisplayLayer` on macOS, a DirectComposition visual on Windows, a synchronized `wl_subsurface` on Wayland. Below that, software decode with one CPU→GPU upload, which beats hardware-decode-then-download because it is one transfer instead of two.

**Done when:** two CLI clients on one machine carry a conversation, mute works both ways, joining and leaving repeatedly leaks no NetEq instances or streams, a 5% loss injection is intelligible rather than choppy, and the spike shows a moving test pattern with no CPU copy anywhere in the path.

---

## M7 — gpui client

**Goal:** A usable GUI for everything built so far.

### 7.0 Add gpui and pin it hard

- **What:** upstream `gpui` plus `gpui-component`, pinned to exact versions.
- **A small patch on upstream, carried deliberately.** gpui's Linux platform is wgpu and its surface support is largely present in the renderer, but `Window::paint_surface()` is only exposed on macOS — so the Linux path has to be plumbed back out to it. That patch is written and works (6.7). Windows needs the same thing against its D3D11 renderer.
- **This is not the `gpui-ce`/`gpui-wgpu` situation.** Those forks swap the platform layer for `winit` and the text stack for `cosmic-text`, which costs you native macOS text rendering, menus and vibrancy — the things you're paying gpui for. Don't use them. Yours is a mechanical patch wiring existing internals to an existing public API.
- **How to carry it:** a fork branch containing *only* the patch, rebased onto tagged upstream releases, consumed via `[patch]` so `gpui-component` resolves to the same build. If the diff grows past "expose what's already there", stop and reassess — that's drift into a real fork.
- **Try to upstream it.** Much better odds than the features upstream has declined: this completes an abstraction that's accidentally macOS-only, and Zed wants Linux and Windows screen sharing itself. If it lands the patch disappears; if it stalls, an open PR still gets the diff reviewed and rebased by more than just you.
- **`gpui-component` is a plain dependency,** so use it. The earlier argument against it was about fork-patching friction, and a `[patch]` block handles that fine.
- **Why pin hard:** gpui is pre-1.0 and breaks between versions — the `Model`/`Entity` rename and the `Window` parameter threading were both mid-2025 churn — and a patch against the platform/renderer boundary is more fragile to refactoring than one against public API. Pin to a specific upstream tag, pin `gpui-component` with it, and make gpui bumps a deliberate task with a video smoke test rather than a routine `cargo update`.

### 7.1 App shell

- **What:** A root entity implementing `Render`, holding the mirrored state from `throcc-client-core/state.rs`, plus one long-lived task bridging the event stream.
- **How:** `cx.spawn` a task that loops on the `broadcast::Receiver`, calls `this.update(cx, |state, cx| { apply(ev); cx.notify() })`, and exits when the channel closes. That's the whole bridge — gpui's executor can await `tokio::sync` channels directly, no adapter, because those primitives don't need a reactor. All networking stays in `throcc-client-core`; if `throcc-client` gains a `quinn` dependency, something has gone wrong.
- **Why this shape:** it's the same two channels the CLI uses, so the CLI keeps working and stays your debugging tool for everything below the UI.

### 7.2 First-run and connect flow

- **What:** Enter an address and an invite code, generate the identity key, connect, pin.
- **Why:** This is the entire onboarding experience, and a self-hosted tool lives or dies on it.
- **How:** Two fields, an address and an invite code. The address is a domain or IP with an optional `:port`, defaulting to 8476; the code is six characters, uppercased on entry. First contact pins whatever answers. On pin mismatch, show both fingerprints and an explicit re-accept action — that action has to exist, because rebuilding a server is a normal event and the recovery path must not be hand-editing a config file.

### 7.3 Room list and roster

- **What:** Rooms with occupants, click to enter, an explicit leave action, per-peer mic and sharing state, a short key fingerprint next to each name.
- **Why:** It's the main screen, and visible membership plus a visible fingerprint is the only thing that would ever surface an unexpected participant. It is also the screen a connected user sits on while in no room, which is now where every session starts — so "connected, in no room" has to look deliberate and calm rather than like a failed connect. Show connection state and the roster distinctly from room membership; a user in the lobby is fully connected and should be able to tell.
- **How:** `gpui-component`'s dock/panel layout for the sidebar-plus-main split, and its list and button components for the roster; the video grid in 8.4 is raw gpui flex. Render from the mirrored state — the UI holds no authoritative state of its own.

### 7.4 Settings

- **What:** Audio device selection, default mute state, display name, avatar, and **share bitrate and framerate**. Show which codec was selected read-only — it's diagnostic, not a choice. No camera settings; there is no camera.
- **Why:** A wrong default audio device is the single most common "it's broken" report. Bitrate and framerate are settings rather than an adaptation loop by design (see *Bitrate is set by the sender*), which means the UI is the only place they can be changed, which means they have to be discoverable and they have to be where the 9.3 warning points.
- **How:** Enumerate devices via `cpal` in `throcc-client-core` (6.1), exposed as plain data. Persist choices in the keystore file. `SetProfile` replaces both fields at once, so the UI sends the current contents of both.

**Done when:** a fresh user installs, enters an address and a code, and talks to someone without touching a terminal.

---

## M8 — Screenshare

**Goal:** One person shares a screen at 1440p60 and everyone else sees it, with frames staying on the GPU end to end. Content is games and video, so this is motion-heavy and there is no static-content shortcut. There is no camera anywhere in this project.

### 8.1 Capture: one trait, three implementations

- **What:** A `Capture` trait yielding frames already on the GPU, and three separate backends behind it. This is where cross-platform costs you most, and there is no crate that removes the cost — see the note below.
- **Why three hand-written backends rather than one crate:** the crates that cover all three platforms (`scap`, `xcap`) hand you `Vec<u8>` pixel data, which is capture → system memory → back to the GPU for the encoder, i.e. exactly the round trip this milestone exists to avoid. The one crate that yields GPU handles (`crabgrab`) covers macOS and Windows only and pins `wgpu 0.20` against a current 30. Linux is hand-rolled in every scenario, so the choice is really two implementations plus a stale dependency, or three implementations and no dependency. Take the three: each is a few hundred lines of platform ceremony, and the ceremony is what you would be maintaining inside the crate anyway.
- **How, per platform** — the picker is the fiddliest part of each:
  - **Linux:** `ashpd` for the xdg-desktop-portal ScreenCast flow, then `pipewire` for the stream. The portal is mandatory on Wayland and gives you the picker for free — it is the compositor's own dialog, which is also the only way to get one that users trust. Negotiate `memory:DMABuf`; the fd you get back is what goes into the `AVHWFramesContext`. It's a D-Bus flow independent of gpui, so build and test it from `throcc-cli` first. **No X11 path** — Wayland or the portal's X11 shim.
  - **Windows:** `windows-capture` (Windows Graphics Capture underneath). Gives an `ID3D11Texture2D` directly, so capture-to-encoder stays on the GPU with no extra work. Use its picker, or `GraphicsCapturePicker` directly.
  - **macOS: budget the most time here.** `screencapturekit`, plus the TCC screen-recording permission prompt, which means a first-run flow that explains itself and survives the user saying no. Frames arrive as `CVPixelBuffer`. This is the weakest link in M8 — validate it before you commit to the milestone estimate. The consolation is that ScreenCaptureKit itself is the best source available on any of the three platforms; the risk is the permission flow around it, not the capture.
- **Verify the format, don't assume it.** Nothing checks that capture and encoder agree; a mismatch is a runtime error at the first frame, or worse, a silent fallback to a CPU copy. Log the actual pixel format and memory type once at stream start, on every platform.
- **Framerate is 30–60 fps, not the 10–15 that suits static desktops.** Games and video need it, which removes the cheap mitigation for every downstream cost.

### 8.2 Encode

- **What:** captured GPU frame → `AVHWFramesContext` → hardware encoder → access unit → 5.6's fragmenter → the same channel audio uses.
- **How:**
  - **Import, don't copy.** Wrap the platform handle in an `AVHWFramesContext` matching the capture device (`AV_HWDEVICE_TYPE_VAAPI` / `D3D11VA` / `VIDEOTOOLBOX`) and hand the encoder an `AVFrame` that points at it. This is the unsafe corner from 6.7; keep it in `media/video/capture` behind the trait.
  - **Probe the ladder in order AV1, H.265, H.264; take the first that works; announce it in `SetMedia`.** No negotiation — the sender picks, receivers cope. Candidates: Linux `av1_vaapi`/`hevc_vaapi`/`h264_vaapi` and `av1_nvenc`/`hevc_nvenc`/`h264_nvenc`; Windows the NVENC set plus `av1_qsv`/`hevc_qsv`/`h264_qsv` and `hevc_mf`/`h264_mf`; macOS `hevc_videotoolbox`/`h264_videotoolbox`. **VideoToolbox has no AV1 encoder**, so the ladder starts at H.265 on every Mac — which is what VideoToolbox does best anyway.
  - **Encoder presence is not capability.** `avcodec_find_encoder_by_name` succeeding says nothing about whether this device supports that profile at this resolution. Probe by opening the codec with your real parameters and falling through on failure — the ffmpeg equivalent of setting an element to PLAYING.
  - Rate control matters more than the codec: CBR, low-delay or ultra-low-latency mode where offered, **no B-frames and no lookahead** (`bf=0`, and the per-encoder low-latency preset). Each adds frames of latency for nothing here. Leave it at 4:2:0 — game and video content is camera-like and most hardware encoders only do 4:2:0.
  - **Intra refresh instead of periodic keyframes** (Design: *Loss repair*). VAAPI, NVENC and Media Foundation all expose it; VideoToolbox exposes less control, so check what you can actually set. This changes 9.1: with intra refresh there is no IDR to wait for, so the server must not gate a new subscriber on a keyframe bit.
  - CBR at the bitrate from settings, and **never change it at runtime**. Report the configured value in `SetMedia`.
  - Cap resolution in the encoder's input parameters rather than trusting the source — someone will share a 5K display.
  - **The self-view is a second reference to the captured frame**, taken before the encoder — `av_frame_ref`, not a copy, straight to the surface element. It must never come back off the network, or a saturated uplink makes users think their own capture is broken.

### 8.3 Decode and present

- **What:** reassembled access unit → hardware decoder → GPU handle → gpui surface. Newest frame, now, everything older discarded. (Design: *Presenting frames without a CPU round trip*.)
- **Why this is a whole substep:** there is no ready-made gpui video widget, and you should not port one from a video player. Players are built around a file — seekable source, duration, playback clock, preroll, read-ahead — and the read-ahead is the latency you are trying not to add. What you need is smaller than a player, not a subset of one.
- **How:**
  - Feed the decoder whole access units, in order. This is why 5.6 discards an incomplete unit rather than passing along what arrived: a partial access unit is not decodable and handing one to `avcodec_send_packet` produces error spew and, on some decoders, a wedged state.
  - **Three buffering stages, and only the third is conditional** (Design: *Video gets a reorder buffer, not a jitter buffer*). Build them in this order and keep them distinct — conflating them is how "the video buffer" ends up meaning something different in each function that touches it.
    1. **Reassembly — always.** One access unit's fragments, from 5.6.
    2. **Reorder window — always.** Emit units in `seq` order, and if one is incomplete while a later one is ready, wait **50 ms**, then abandon it, emit the next, and fire a rate-limited `RequestKeyframe`. **This stage exists whether or not the share has audio** — it is about datagram reordering, not about sync.
       **50 ms, not one frame, because the costs are asymmetric.** Waiting costs a bounded hitch: later units are already buffered, so the display holds for at most the deadline and then catches up. Discarding costs the whole access unit — ~37 datagrams — plus every later frame that references it, and with intra refresh there is no IDR to snap back to, so the damage smears until the refresh sweep passes. Hundreds of milliseconds of corruption to save 50 ms is the wrong way round. A deadline is not steady-state latency: it is paid only when something is missing, and never at all on a clean path.
       **A flat 50 ms, not a multiple of the frame interval.** You do not know the sender's framerate — it is a local setting (7.4) and is not on the wire — so a frame-relative deadline needs a protocol field or an estimate from arrival timing, and arrival timing is exactly what a jitter deadline must not trust. It would also be redundant: the clock only starts once a later unit is complete, so a 15 fps sender cannot trigger an early deadline whatever the constant is. Frame-relative behaviour comes from the trigger, not the timeout.
       **Instrument it instead of guessing.** Count units abandoned on the deadline, and — the number that actually answers the question — fragments arriving *after* their unit was abandoned. Nonzero means the deadline is too tight; zero means these are real losses no wait would have recovered. Both are free: the reassembler already identifies stale fragments in order to drop them. Feed both to 9.3.
    3. **Sync hold — only when the share carries audio (8.4).** Hold each frame until its timestamp matches the share audio's playout position; audio is the clocked side, so it leads. This is the one place video is deliberately delayed, it applies only within a share, and a silent share never enters this stage at all.
  - Because B-frames and lookahead are off (8.2), decode order is capture order, so the window only sorts — it never has to reorder around frame dependencies. If anyone ever turns B-frames on, this is the code that breaks.
  - Drop fragments for a unit older than the last one emitted. Stale by definition.
  - **Tune the deadline from the counters, not from taste.** The constant above is a starting point chosen from the cost asymmetry, not from measurement — the late-arrival counter is what turns it into a number.
  - **If judder shows up, fix it on the sender before touching this.** A 1440p frame is ~37 datagrams; sent back-to-back they arrive as a burst whose spread *is* the judder. Pace fragments across the frame interval — it costs nothing and attacks the cause. Receive-side delay is the last resort, and if it is ever added it stays fixed at one frame and never adaptive. Note this is not the stage-3 sync hold: holding a share's video against its *own* audio is sync within one source and is intended; holding it against the room's voices is sync across sources and is not.
  - **Ignore the PTS.** Display on arrival. Nothing in this client consults a presentation timestamp, which is the inverse of what a player does.
  - **Convert and scale on the GPU in one pass** — `scale_vaapi`, `scale_cuda`, `scale_d3d11`, `scale_vt` — NV12/P010 to single-plane RGBA *and* down to roughly the tile size. macOS is the exception: `paint_surface` takes the NV12 `CVImageBuffer` as-is and Metal does the conversion, so no filter pass is needed there. The alternative — importing Y and UV as separate textures and converting in a shader — is faster and considerably more work; only write it if profiling asks.
  - **One trait, three backends, one file.** `video.rs` exposes a single "here is a frame, display it" interface; behind it sit DRM_PRIME→`wgpu::Texture` on Linux, `CVImageBuffer`→`paint_surface` on macOS, and `ID3D11Texture2D` on Windows. This is 6.7 productionised.
  - **Build the decoder from the codec in `PeerMedia`**, not by probing the bitstream. Rebuild it if a peer reconnects with a different codec.
  - **Hold each `AVFrame` reference until the GPU is done with it.** Same rule as 6.7 and the same misleading symptom if you get it wrong.
  - **Fall back to software decode, not to download-then-upload.** `libdav1d` for AV1, ffmpeg's native decoders for H.265/H.264, then one CPU→GPU upload. This is also the answer for a Mac receiving AV1 from a Linux or Windows sharer without hardware AV1 decode. One transfer beats two. Pick per peer at decoder construction based on what actually opened.
  - **`tokio::sync::watch` for the frame handoff, not `mpsc`.** `mpsc` at capacity drops the newest item, which is backwards for a display path; `watch` overwrites, so a late wake-up sees the current frame. Keep `mpsc` for the network paths.
  - On the gpui side, render through the surface path and **release the previous frame when you replace it.** This is the leak everyone hits; it presents as memory climbing through a call, not as anything image-shaped.
  - `cx.notify()` on frame arrival. No frame queue, no pacing.
  - **Don't sync audio to video.** Audio times itself off NetEq, video paints on arrival. Video ends up 50–100 ms ahead of its audio, which is audio-lag rather than audio-lead and below the threshold anyone notices. Aligning would mean buffering video: trading latency for a defect nobody reported.
  - `throcc-client/video.rs` stays the only file in the frontend touching ffmpeg or GPU interop. It is the most delicate file in the project, so keep everything else out of it.

### 8.4 System audio

- **What:** the share's own sound, captured as a second track of the same `Share`, encoded as stereo Opus, and timestamped from the same clock as the video. (Design: *System audio travels beside the share, not inside a container*.)
- **Not a container.** No MPEG-TS, no fMP4. Sync comes from the capture timestamp in the fragment byte's bit2, which is the useful half of what a container would give you without coupling audio loss to video loss, without forcing the SFU to demux, and without putting audio packets behind a 37-datagram video burst.
- **How, per platform:**
  - **macOS — the easy one.** `screencapturekit` takes `.with_captures_audio(true).with_sample_rate(48000).with_channel_count(2)`, and system audio arrives from the same `SCStream` as the video, already on one clock. Nothing to map.
  - **Windows — the awkward one.** `cpal` already does loopback: open an *input* stream on a **render** device and it sets `AUDCLNT_STREAMFLAGS_LOOPBACK` for you, so no extra crate. But Windows Graphics Capture and WASAPI are separate subsystems with separate clocks, so map both onto QPC before either timestamp means anything relative to the other. **Budget for the sync bug living here.**
  - **Linux.** Not from the portal — the ScreenCast portal does not reliably hand you audio. Capture the sink's monitor node through the `pipewire` crate you already have for video, which puts both in one PipeWire clock domain.
- **Encode it differently from the microphone.** This is content, not speech: stereo, `Application::Audio` rather than `Voip`, 96–128 kbps, and **no `sonora` processing at all**. Running an echo canceller, noise suppressor or AGC over a clean digital tap of a game's soundtrack is destructive — noise suppression in particular will treat music as noise.
- **Feed the echo canceller the full playout mix, including system audio.** If the sharer is on speakers, their microphone picks up the game as well as the room. The AEC reference has to be what the device actually played or the mic track carries an echo of the very audio you are also sending cleanly, and receivers hear it twice.
- **Mute is per track.** Muting the microphone must not mute the share's sound, and vice versa. Two toggles, two booleans in `SetMedia`.
- **Watch NetEq on this track.** It is tuned for speech, and its concealment on music is more audible than on voice. The mitigating fact is that a digital tap has far less arrival jitter than a microphone, so it should settle on a shallow target — but if music artefacts show up, this is the cause.

### 8.5 Layout

- **What:** The shared screen large, the roster as a list beside or below it.
- **Why it's simple:** with no cameras there is no tile grid to solve. The common case is exactly one active share and a list of names with mic indicators, which should be built as the easy thing rather than as a degenerate grid.
- **How:** Raw gpui flex, not a component. One share fills the area; two or more split it. Tear down the decoder for a share that isn't on screen rather than merely skipping the paint — decoding a 1440p stream nobody is looking at is the most expensive no-op available.

**Done when:** three participants on three different operating systems watch one 1440p60 game share without stalling, no frame in the path is ever copied to system memory on a machine with hardware decode, `perf` shows the client's CPU use roughly flat as resolution rises, and the sharer's own preview is instant regardless of network conditions.

---

## M9 — Robustness

**Goal:** It survives bad networks and restarts.

### 9.1 Keyframe repair

- **What:** `RequestKeyframe(MediaId)` routed to that sender as `KeyframeWanted`, forcing an IDR.
- **Why:** Datagrams don't retransmit, so a lost keyframe freezes a receiver until the encoder's next one — potentially seconds of a frozen face.
- **How:** Detect the gap from the frame header `seq`, not from a decoder — the header is read before the payload reaches one at all, 5.3's window already has the number, and 5.6's reassembler has to notice the gap anyway to discard the access unit. Rate-limit to one per second per `media_id`. The server routes it without parsing anything. **Do not gate a new subscriber on the keyframe bit** — with intra refresh (8.2) there is no IDR to wait for and the gate never opens. Instead the server sends `KeyframeWanted` to every sharer in a room when someone enters it, and forwards everything meanwhile: the newcomer sees a moment of nothing instead of a permanent stall, and the server keeps no keyframe logic and no per-track-kind special cases at all.

### 9.2 Reconnect

- **What:** Re-run the full handshake with `want_room` set to the last requested room — which is `None` if the user was in the lobby, and must stay `None` rather than being treated as "unset, pick something".
- **Why:** A dropped connection should be a brief absence from the roster, not a state to repair by hand.
- **How:** Exponential backoff with jitter. Media ids change by construction and peers learn the new ones from `UserEntered`, so tear down every decoder, NetEq instance and reassembly buffer on disconnect rather than trying to resume them.

### 9.3 Link diagnostics

- **What:** `throcc-client-core/health.rs`: a periodically-sampled struct the UI renders, and the replacement for bitrate adaptation. (Design: *Link diagnostics*.)
- **Why:** Fixed sender-side bitrate is deliberate, and its failure mode is silence — congestion control drops datagrams locally with no error at either end, so an overcommitted uplink is indistinguishable from a bug. Without this substep, every network problem in the product's life becomes an unfalsifiable report.
- **How:**
  - Sender: sample `Connection::stats()` on a 1s timer for rtt, congestion window and lost packets, and read 6.3's send-channel drop counter (audio) and 8.2's (video). `send_datagram` returning `Ok` is **not** evidence of transmission — quinn's datagram queue is bounded and drops rather than erroring, so the counter and the stats are the only truth.
  - Receiver: per-`media_id` loss from 5.3's window — gaps that never filled over a rolling interval. No protocol addition.
  - Receiver, video only: units abandoned on the reorder deadline, and fragments that arrived after their unit was abandoned (8.3). The second is what says whether the deadline is mistuned rather than the network being lossy — worth surfacing in diagnostics even though it is not a user-facing number.
  - Combined: compare a peer's advertised `screen_kbps` against bytes actually arriving on their share ids. Materially below → their uplink; roughly matching while your own stats look bad → your downlink. These are indistinguishable without both numbers.
  - UI: one unobtrusive indicator, and one plain-language sentence when it's bad — "your upload can't carry 4 Mbps, lower the bitrate in settings" — pointing at 7.4. No graphs, no jargon.

### 9.4 Failure-mode pass

- **What:** Deliberately break things and watch: kill the server mid-call, 5% loss and 200ms jitter, block UDP entirely, saturate the uplink, fill the disk, revoke a user mid-call.
- **Why:** Every one of these will happen. Finding out now is cheaper than finding out from a user with no logs.
- **How:** `tc qdisc add dev lo root netem loss 5% delay 100ms 50ms`, and `tc ... rate 500kbit` to squeeze the uplink below the configured bitrate — that last one is the test for 9.3, and the pass condition is that the client *says so*. Blocked UDP must produce a clear error naming UDP — there's no TCP fallback, so that error message *is* the user's diagnosis.

### 9.5 Avatar upload off the control stream

- **What:** A fresh unidirectional stream per upload: a `{ req_id, len }` postcard header then the bytes, with the hash returned as a normal `RespEnvelope` on the control stream.
- **Why:** Blocking the control stream behind a multi-megabyte image is exactly the head-of-line problem datagrams exist to avoid. Reusing `req_id` from the same counter means the reply completes through the same pending-request map as everything else, instead of needing a second matching mechanism.
- **How:** Server-side size cap, re-encode to fixed dimensions, store by content hash. Clients cache by hash and never refetch.

**Done when:** a call survives 5% loss and 200ms jitter intelligibly, a throttled uplink produces a correct on-screen explanation rather than mystery stutter, and a server restart mid-call reconnects everyone into their previous rooms without intervention.

---

## M10 — Admin and deployment

### 10.1 Admin UI

- **What:** Invite generation with role and TTL, user list with roles, removal, room management.
- **Why:** These all exist server-side already; without UI the operator needs a database client.
- **How:** Hide by rank. `gpui-component`'s table and modal components cover this whole screen. Show generated codes as copyable text with their expiry, and warn that they're single-use.

### 10.2 Revocation semantics

- **What:** Removing a user deletes the allowlist row and closes any live connection holding that key.
- **Why:** A removal that leaves the current session running is not a removal.
- **How:** Keep a `HashMap<UserId, ConnHandle>`; `close()` with a reason so the client can show `Kicked`.

### 10.3 Deployment

- **What:** The container image from 0.4 as the supported deployment, plus a compose file and a documented install. A systemd unit for operators who would rather not run Docker.
- **Why the container is primary:** the release stage already exists and has been building since M0, so the deployed artifact is the one you have been testing against all along rather than a packaging effort invented at the end.
- **How, container:** publish the `runtime` stage. A minimal compose file with `ports: ["8476:8476/udp"]` — **the `/udp` suffix is load-bearing** — a named volume on `/data`, and `restart: unless-stopped`. Ship it in the repo so the documented install is "edit nothing, `docker compose up -d`, read the bootstrap code out of the volume". 8476 is unprivileged, so the container's own user binds it with no capability at all — which is the main reason to prefer it over 443, whose unprivileged binding depends on the runtime granting `CAP_NET_BIND_SERVICE`.
- **How, systemd:** `DynamicUser=yes` with `StateDirectory=throcc` gets `/var/lib/throcc` with correct ownership and no manual user creation; point the server at it with `Environment=THROCC_DATA_DIR=%S/throcc`, since the compiled-in default is the container's `/data`. Add `ProtectSystem=strict`, `NoNewPrivileges=yes`, `PrivateTmp=yes`. Binding UDP/8476 needs no `AmbientCapabilities` at all; a port below 1024 would need `CAP_NET_BIND_SERVICE`.
- **Either way:** UDP/8476 is the target port — unprivileged and unassigned. It is more likely to be blocked than 443 would be, and an operator who needs the reachability of a well-known port should publish 443 on the host and map it to 8476. Document `ufw allow 8476/udp`, since a silently dropped UDP port presents as "it just hangs", which is the same symptom as a TCP-only port mapping and worth naming in the docs next to it.

### 10.4 Operator docs

- **What:** A README covering install, bootstrap code location, backup, upgrade.
- **How:** State plainly: back up the data directory volume, `server_key` above all — upgrading is `docker compose pull && up -d`, which replaces the image and keeps the volume, and that only stays true if the operator never put state anywhere else. Losing it isn't data loss but it is an identity change and every client hits a pin warning. Restoring to different hardware or a new IP is fine, because the pin follows the key rather than the certificate or the address.

**Done when:** a clean Debian box with Docker goes from `docker compose up` to a working call in under ten minutes using only the README.

---

## Later: end-to-end encryption

Not part of this build. The design doc's *Room left for E2EE later* lists the hooks already in place — the point of repeating it here is only so nobody deletes them as dead weight during M5–M9. Specifically: the server never reads past byte 13, `media_id || epoch || seq` is already a 96-bit AEAD nonce with a never-reused `media_id` and no rollover counter, the header doubles as AAD, the fragment byte is inside the payload rather than in the header's flag bits so enabling encryption doesn't leave frame boundaries and sizes visible to the server, the datagram budget reserves 16 bytes for the tag, the transform hooks are called and take a growable buffer, and the per-room epoch already bumps on exactly the events that would trigger a rekey and is already in the header for key selection.
