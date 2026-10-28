# throcc

Self-hosted voice and screen share in Rust. `PLAN.md` is the spec and the milestone list. Read it before starting work, and tick its checkboxes as you go.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
./scripts/check-server-deps.sh
```

## Style

- Commit after each completed step with a one-line conventional commit (`feat(sfu): drop whole frames per receiver`). No body.
- If signing fails or asks for a passphrase, commit unsigned (`git commit --no-gpg-sign`) and carry on. Don't retry signing or rewrite history; the human signs those commits later.
- Clean, small, readable code. Clear names over comments.
- Comment only what the code can't say: why, invariants, safety. No comments that restate the code.

## Rules

- `throcc-proto` has no async runtime, no media and no I/O. Only the main agent changes it.
- The server never depends on codecs, capture, audio or media crypto (`throcc-crypto` without the `media` feature).
- The wire format and crypto are frozen after M5. Changing them needs a human decision.
- Audio and capture callbacks never block, allocate, lock, await or panic. Cross into tokio only with a bounded channel, `try_send`, drop on full and a counter.
- Colours live only in `throcc-client/theme.rs`. The UI follows `design/mockups/*.dc.html` and PLAN.md §5.13.
- No GPL or AGPL dependencies.
- Record non-obvious decisions in `docs/DECISIONS.md`. Record anything that needs real hardware in `docs/MANUAL_TESTS.md`.
- A milestone is done when its *Done when* list is shown by a test or a command, not before.
- Crypto, protocol and SFU changes get reviewed by a subagent that didn't write them.
