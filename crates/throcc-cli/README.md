# throcc-cli

A headless client for scripts, tests and network measurements. `throcc-cli --help` lists its commands. Among them are a voice call through the real audio path, and `send-synthetic` and `recv`, which push fake media through the real encrypted path.

| File | Holds |
| --- | --- |
| `src/main.rs` | Commands and the interactive prompt |
| `src/synthetic.rs` | Sends fake access units at a chosen size and rate, and receives and checks them |
| `src/unit.rs` | The fake access units themselves; the integration tests include this file too |
| `tests/common/` | A server in-process or on its own thread, UDP relays, and helpers to sign clients in |
| `tests/*.rs` | The workspace's integration tests: transport, control, enrollment, rooms, media, share and client |

The network scenarios in `tests/netem/` drive this binary across Linux network namespaces.
