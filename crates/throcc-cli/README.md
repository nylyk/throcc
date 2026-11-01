# throcc-cli

A headless client for scripts, tests and network measurements. `throcc-cli --help` lists its commands: enroll, rooms, join, invite, users, a voice call through the real audio path, and `send-synthetic` and `recv`, which push fake media through the real encrypted path.

| File | Holds |
| --- | --- |
| `src/main.rs` | Commands and the interactive prompt |
| `src/synthetic.rs` | Fake access units of a chosen size and rate, sent and checked on arrival |
| `tests/common/` | A server in-process and helpers to sign clients in |
| `tests/*.rs` | The workspace's integration tests: transport, control, enrollment, rooms, media, share and client |

The network scenarios in `tests/netem/` drive this binary across Linux network namespaces.
