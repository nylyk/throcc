# throcc

Self-hosted voice calling and screen sharing, in Rust. One server, one QUIC
connection per client, rooms you move between without a second handshake. The
server is a dumb SFU: it routes datagrams and never parses media. Identity is an
Ed25519 keypair per client, allowlisted server-side; the server is authenticated
by a pinned self-signed certificate rather than a CA.

`design.md` is the full design and `impl-guide.md` the milestone plan. Both are
authoritative — read the relevant section before changing anything it covers.

## Layout

- `crates/throcc-proto` — wire types and pure codecs. No tokio, no I/O, no media.
- `crates/throcc-server` — QUIC listener, per-connection tasks, SFU. Depends on `throcc-proto` only.
- `crates/throcc-client-core` — connection, keystore, state, media. Owns the runtime.
- `crates/throcc-cli` — headless client; the test rig for everything below the UI.
- `crates/throcc-client` — desktop client. Stub until M7.

Nothing above `throcc-client-core` may see a `quinn` type or a tokio handle.

## Style

- KISS. Simplest thing that works; no speculative abstraction or config.
- Clean code: small functions, one responsibility each, errors handled where
  they can be acted on.
- Small functions, but never a function whose body is one self-evident
  expression. Extracting `data_dir.join(KEY_FILE)` for the sake of a name reads
  as though there is logic behind it, and costs a jump to find out there isn't.
  Inline it; a name is not worth a call site that lies about its own depth.
- Names carry the meaning. Every function, struct, field, variable and constant
  must be understandable from its name alone, without reading its body or a
  comment beside it. Rename before reaching for either. No abbreviations
  (`config` not `cfg`, `fingerprint` not `fp`) and no placeholders (`e` for an
  error in a match arm is fine; `c`, `ks`, `opts` are not).
- No comments unless absolutely necessary. Say it in the code. A comment is
  warranted only for a non-obvious *why* — a protocol constraint, a deliberate
  security trade-off, a workaround for someone else's bug. Never restate what
  the code says, and never narrate the milestone plan: `impl-guide.md` owns
  that, and a comment about M5 is wrong the moment M5 lands.
- Write for whoever reads the code next, never for the person running the
  server. "Back this up: losing it is an identity change" is operator
  documentation, and operator documentation belongs in the README, where someone
  deploying throcc will actually find it.
- That reader never saw the diff, and cannot see what is absent. Never comment
  on something the code does *not* do — a removal, a field left out, a road not
  taken. Such a comment has no subject on the page: it documents a change rather
  than the code, and the only person it ever spoke to was the reviewer of the
  commit that added it.
- Doc comments on public items only where the name cannot carry it, and then
  at most two lines. A fragment is fine, obscurity is not: say plainly what the
  item is or does, so it reads without the reader supplying a missing subject.
- No module docs. A rule binding a whole file or crate belongs here, in this
  file, where it governs every crate at once and cannot drift out of sync with
  the code it describes.
- Match the surrounding code's idiom.
- Always run `cargo fmt` after editing Rust. Never hand-format.

## Commands

```bash
cargo fmt
```

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets
```
