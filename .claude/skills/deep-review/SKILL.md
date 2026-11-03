---
name: deep-review
description: Simplify, review, security-review, fix and clean up everything changed since the review baseline in CLAUDE.md, then move the baseline. Use when asked for a deep review.
---

# Deep review

`BASE` is the commit in the `Review baseline` section of CLAUDE.md. Record `START=$(git rev-parse HEAD)`. The scope is `BASE..START` plus whatever this review changes. If `BASE..START` is empty, stop.

A unit is one crate with changes in `BASE..START`. Changes outside `crates/` form one extra unit. Unchanged crates are skipped.

## Rules

- All CLAUDE.md rules apply to every agent.
- Fix real issues in security-critical code too, carefully. A fix that would change the protocol goes in the report instead.
- Only the main agent edits `throcc-proto`.
- Crypto, protocol and SFU fixes are reviewed by a subagent that didn't write them.
- Subagents get `BASE` and their unit's paths, edit only those paths and never commit. The main agent commits after each step, one commit per crate.
- Review subagents don't edit. Each finding has `file:line`, what goes wrong and a concrete trigger.
- Before every commit, run the gate and fix what fails. Never skip or weaken a test:
  ```sh
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

## Steps

1. **Simplify.** One subagent per unit runs the `simplify` skill on its part of the diff. No behaviour changes.
2. **Deep review.** One subagent per unit reads every changed line and the code around it: callers, invariants, error paths, concurrency, cancellation, panics, limits, and the CLAUDE.md rules.
3. **Security review.** One subagent each for `throcc-crypto`, `throcc-proto`, `throcc-server` and client-core's connection, keystore and key manager, if they changed. Auth, key handling, nonce reuse, untrusted input, denial of service from one client, leaks to the server or other members, downgrades.
4. **Triage and fix.** The main agent checks every finding against the code. Drop theoretical ones, duplicates and nits. Fix the rest itself or with one subagent per crate, with a regression test where possible.
5. **Cleanup.** One subagent per crate changed in `BASE..HEAD`. Dead code, unused imports, stale comments, naming, formatting, crate README updates. No refactors, no behaviour changes.
6. **Final review.** One fresh subagent per crate reviews `START..HEAD` for regressions and rule violations. Triage and fix as in step 4. Run `cargo deny check` and `./scripts/check-server-deps.sh`. Commit.
7. **Move the baseline.** Set the `Review baseline` section in CLAUDE.md to the full hash of the last commit and today's date. Commit on its own.

Steps 1 to 3 and 5 run their subagents in parallel.

## Report

- Fixed findings, one line each.
- Decisions needed: findings that need a product decision or a protocol change, with `file:line` and why.
