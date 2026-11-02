# throcc

Self-hosted voice and screen share in Rust, in the spirit of TeamSpeak. Persistent rooms with low-latency voice, screen share up to 1440p60 with its system audio, and end-to-end encryption for all media. The server relays everything; there is no peer-to-peer path. One server binary, one data directory; clients join with an address and an invite code.

Not in scope: web or mobile clients, rooms beyond about 20 people, recording, transcoding, simulcast, a TCP fallback, text chat.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
./scripts/check-server-deps.sh
throcc-server admin-invite --data <dir>   # single-use admin invite, works while the server runs
throcc-client --mock --screen <name>   # sample data, no server; names in `ThroccApp::show_sample`
./scripts/screens.sh                   # macOS: README screenshots into docs/screens/
```

On macOS, building with `--features ffmpeg` needs `FFMPEG_PKG_CONFIG_PATH=$PWD/deps/ffmpeg-lgpl/lib/pkgconfig` (built by `packaging/ffmpeg/build.sh deps/ffmpeg-lgpl`).

## Crates

| Crate | Holds |
| --- | --- |
| `throcc-proto` | Messages, ids, the media header, framing, fingerprints. No async runtime, no media, no I/O |
| `throcc-crypto` | PAKE, identity signatures, HPKE wrapping, media AEAD, safety numbers. No I/O. Feature `media` is off in the server |
| `throcc-server` | Sessions, auth, rooms, SFU, key relay, SQLite storage |
| `throcc-client-core` | Connection, keystore, key manager, voice and share pipelines. `Client` and `MockClient` behind `ClientApi` |
| `throcc-client` | gpui UI and the video surface |
| `throcc-cli` | Headless client for scripts and integration tests |

Each crate's `README.md` explains its modules and how they fit together. Update it when a module is added, removed or changes its role.

`third_party/` holds vendored gpui crates with an NV12 surface patch; its README lists the changes and how to refresh them.

## Rules

- Only the main agent changes `throcc-proto`. Any change to messages bumps `PROTOCOL` in `messages.rs`; new enum variants go last.
- The server never depends on codecs, capture, audio or media crypto. `check-server-deps.sh` enforces this.
- The server never reads media past the 13-byte header and holds no media key.
- **Frozen; changes need a human decision:** the media header, the media AEAD and track-key derivation, HPKE sender-key wrapping and its info, the signed encryption key, and the safety-number construction.
- Never weaken a security property to make a test pass. Stop and ask instead.
- Audio and capture callbacks never block, allocate, lock, await or panic. Cross into tokio only with a bounded channel, `try_send`, drop on full and a counter.
  The one exception is outside our code: screencapturekit's own dispatcher on macOS takes a lock and allocates before our handler runs. Our handler itself stays lock-free (`try_lock` only).
- SFU egress never drops audio by policy, and drops video only as whole access units.
- No GPL or AGPL dependencies. `cargo deny` enforces this.
- Crypto, protocol and SFU changes get reviewed by a subagent that didn't write them.
- Released formats only change through a migration: the server schema (`MIGRATIONS` in `database.rs`) and every client JSON file (`json_file::Format::MIGRATIONS`). Version 1 of each is what 0.1 shipped; never edit it in place.

## UI

- Every colour lives in `throcc-client/theme.rs`: an 8-colour palette the user picks, everything else mixed from it in Oklab.
- Quiet and minimal: one accent, no gradients, no shadows beyond focus rings, no emoji. The one gradient is the shade behind controls over video, which keeps them readable on any picture.
- Geist and Geist Mono; mono for addresses, codes, fingerprints, numbers and codecs. Spacing on a 4 px grid.
- Match the existing screens. Add a `--mock --screen` sample for any new screen or dialog.

## Style

- Commit after each completed step with a one-line conventional commit (`feat(sfu): drop whole frames per receiver`). No body.
- If signing fails or asks for a passphrase, commit unsigned (`git commit --no-gpg-sign`) and carry on. Don't retry signing or rewrite history; the human signs those commits later.
- Clean, small, readable code. Clear names over comments.
- Comment only what the code can't say: why, invariants, safety. No comments that restate the code.

## Review baseline

The whole workspace was simplified, deep-reviewed line by line and security-reviewed through commit `64dcb54d10dc94b4837950066f18a36c23adb602` (2026-11-02), including the owner's follow-up decisions. Later deep reviews start from there: `git diff 64dcb54..HEAD`.
