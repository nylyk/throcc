# throcc

Self-hosted voice and screen share in Rust, in the spirit of TeamSpeak. Persistent rooms with low-latency voice, screen share up to 1440p60 with its system audio, and end-to-end encryption for all media. The server relays everything; there is no peer-to-peer path. One server binary, one data directory; clients join with an address and an invite code.

Not in scope: web clients, rooms beyond 30 people, recording, transcoding, simulcast, a TCP fallback, text chat.

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
packaging/licenses/notices.sh client   # third-party notices the packages ship (also `client-macos`, `server`, `android`); needs cargo-about
packaging/macos/bundle.sh              # macOS: target/macos/throcc.dmg
packaging/flatpak/cargo-sources.sh     # the flatpak's offline crates, before flatpak-builder
mobile/ios/build-core.sh [device] [simulator]   # iOS: ThroccCore.xcframework and the Swift bindings, before Xcode builds; both slices by default
just                                   # lists the recipes that build and run the desktop app, the server and the phone apps
mobile/android/gradlew -p mobile/android assembleDebug   # Android: builds the core with cargo-ndk, then the APK
xcrun simctl launch booted cc.thro.throcc --mock --screen <name>   # iOS sample screens; names in `Sample.swift`
adb shell am start -S -n cc.thro.throcc/.MainActivity --ez mock true --es screen <name>   # Android sample screens; names in `AppModel.kt`
THROCC_SOFTWARE_DECODE=1 THROCC_CODEC=vp9 just desktop   # testing: software decoders, and one codec to share in (av1, hevc, h264, vp9)
just android --ez software_decode true                   # the same software switch on Android; `--software-decode` as an iOS launch argument
```

macOS, iOS and Android link dav1d and libvpx statically: `packaging/codecs/build.sh deps/codecs <rust-target>...` builds them, then `set -a; eval "$(packaging/codecs/env.sh deps/codecs)"; set +a` before cargo. macOS needs no ffmpeg; Linux and Windows video needs `--features ffmpeg`.

## Crates

| Crate | Holds |
| --- | --- |
| `throcc-proto` | Messages, ids, the media header, framing, fingerprints. No async runtime, no media, no I/O |
| `throcc-crypto` | PAKE, identity signatures, HPKE wrapping, media AEAD, safety numbers. No I/O. Feature `media` is off in the server |
| `throcc-server` | Sessions, auth, rooms, SFU, key relay, SQLite storage |
| `throcc-video` | Video encoders and decoders, a backend per platform, and the NV12 frames between them. No sessions, keys or wire |
| `throcc-client-core` | Connection, keystore, key manager, voice and share pipelines. `Client` and `MockClient` behind `ClientApi` |
| `throcc-client` | gpui UI and the video surface |
| `throcc-cli` | Headless client for scripts and integration tests |
| `throcc-mobile` | UniFFI layer over the core for the iOS and Android apps in `mobile/` |

Each crate's `README.md` explains its modules and how they fit together. Update it when a module is added, removed or changes its role.

`third_party/` holds vendored gpui crates with an NV12 surface patch; its README lists the changes and how to refresh them.

## Rules

- Only the main agent changes `throcc-proto`. From 0.1 on, any change to messages bumps `PROTOCOL` in `messages.rs`; new enum variants go last. 0.1 ships protocol 1.
- Limits that server and client share live in `throcc_proto::limits` and nowhere else; anything sized by a room or a server derives from them.
- The server never depends on codecs, capture, audio, media crypto or `throcc-video`. `check-server-deps.sh` enforces this.
- The server never reads media past the 13-byte header and holds no media key.
- **Security-critical:** the media header, the media AEAD and track-key derivation, HPKE sender-key wrapping and its info, the signed encryption key, and the safety-number construction. Fix real issues here with great care and without changing the protocol; a fix that needs a protocol change needs a human decision.
- Never weaken a security property to make a test pass. Stop and ask instead.
- Audio and capture callbacks never block, allocate, lock, await or panic. Cross into tokio only with a bounded channel, `try_send`, drop on full and a counter.
  The one exception is outside our code: screencapturekit's own dispatcher on macOS takes a lock and allocates before our handler runs. Our handler itself stays lock-free (`try_lock` only).
- SFU egress never drops audio by policy, and drops video only as whole access units.
- No GPL or AGPL dependencies. `cargo deny` enforces this.
- Crypto, protocol and SFU changes get reviewed by a subagent that didn't write them.
- Released formats only change through a migration: the server schema (`MIGRATIONS` in `database.rs`) and every client JSON file (`json_file::Format::MIGRATIONS`). Version 1 of each is what 0.1 shipped; never edit it in place.

## Releases

- `main` only holds releases. Work happens on `develop`, and PRs target `develop`. `main` takes changes only through a PR with green `ci`; neither branch accepts force pushes, and release tags can't be moved or deleted.
- `throcc-server` and `throcc-client` have their own semver `version` in their `Cargo.toml`; the other crates keep the workspace version. Patch for fixes, minor for features or a protocol bump while 0.x.
- `PROTOCOL` alone decides compatibility. A server accepts `MIN_PROTOCOL..=PROTOCOL`, and a client checks its `PROTOCOL` is in the server's range. Both hellos stay decodable by every version. Every server version supports the current protocol and the one before it, except protocol 2, which dropped protocol 1. A protocol bump is a breaking server release.
- Docker is the only supported server distribution. The app id is `cc.thro.throcc` everywhere.

A release, for each side that changed since its last tag (`git log <side>-vX.Y.Z..develop -- crates/`):

1. On `develop`, bump that side's `version` and run `cargo build` for `Cargo.lock`. A client release also adds a `<release>` at the top of `packaging/flatpak/cc.thro.throcc.metainfo.xml`.
2. Optionally test a signed build: `gh workflow run packaging.yml --ref develop`.
3. Merge `develop` into `main` through a PR, with a merge commit.
4. Tag the merge commit `server-vX.Y.Z` and/or `client-vX.Y.Z` (annotated) and push the tags. The server tag publishes the image as `:X.Y.Z`, `:X.Y` and `:latest`; the client tag drafts a GitHub release with the signed dmg, the Windows installer, the flatpak and `SHA256SUMS`.
5. Edit the draft's notes and publish it; running clients then show the update notice.

## UI

- Every desktop colour lives in `throcc-client/theme.rs`: an 8-colour palette the user picks, everything else mixed from it in Oklab. The preset palettes live in `throcc_client_core::theme`.
- Quiet and minimal: one accent, no gradients, no shadows beyond focus rings, no emoji.
- Geist and Geist Mono; mono only for keys, fingerprints, safety numbers and invite codes. Spacing on a 4 px grid.
- Match the existing screens. Add a `--mock --screen` sample for any new screen or dialog.
- The phone apps in `mobile/` use native components and the system font, coloured by the same themes. A new screen there gets a sample on both platforms.

## Style

- Commit after each completed step with a one-line conventional commit (`feat(sfu): drop whole frames per receiver`). No body.
- If signing fails or asks for a passphrase, commit unsigned (`git commit --no-gpg-sign`) and carry on. Don't retry signing or rewrite history; the human signs those commits later.
- Clean, small, readable code. Clear names over comments.
- Comment only what the code can't say: why, invariants, safety. No comments that restate the code.

## Review baseline

The whole workspace was simplified, deep-reviewed line by line and security-reviewed through commit `2f1a0d3d58cfe2666f561a161ca8f36e14e9ec71` (2026-11-12). Later deep reviews start from there: `git diff 2f1a0d3..HEAD`. Run them with the `deep-review` skill, which moves this baseline when it finishes.
