# throcc-cli

A headless client for scripts, tests and network measurements. `throcc-cli --help` lists its commands. Among them are a voice call through the real audio path, and `send-synthetic` and `recv`, which push fake media through the real encrypted path. `start-over` starts a new identity after the OS keychain lost yours; it asks you to type `start over` first, or takes `--yes` in a script.

| File | Holds |
| --- | --- |
| `src/main.rs` | Commands and the interactive prompt |
| `src/synthetic.rs` | Sends fake access units at a chosen size and rate, and receives and checks them |
| `src/unit.rs` | The fake access units themselves; the integration tests include this file too |
| `tests/common/` | A server in-process or on its own thread, UDP relays, and helpers to sign clients in |
| `tests/*.rs` | The workspace's integration tests: transport, control, enrollment, rooms, media, share, slow share and client |

The network scenarios in the repository root's `tests/netem/` drive this binary across Linux network namespaces.

## Manual tests

These integration tests are `#[ignore]`d because they need a hardware video encoder or a desktop session, and no CI job runs them. Virtual machines queue frames for seconds, so run them on real hardware:

```sh
cargo test -p throcc-cli --features ffmpeg --test <file> -- --ignored <name>
```

| File | Test | Checks |
| --- | --- | --- |
| `share` | `a_shared_picture_arrives_within_a_few_frames` | A test pattern's frames reach a viewer within a few frames |
| `share` | `share_audio_and_video_arrive_on_one_clock` | A share's audio and video keep one clock end to end |
| `slow_share` | `a_share_loading_on_a_good_link_never_shows_as_too_slow` | No false "too slow" while a share starts on a good link |
| `slow_share` | `a_share_shows_as_too_slow_while_our_link_cant_carry_it` | The label shows on a capped link and clears once it lifts |
| `slow_share` | `loss_on_the_sharers_upload_never_shows_the_share_as_too_slow` | The sharer's own upload loss isn't blamed on the viewer |
| `share` | `a_screen_share_runs_for_profiling` | Not a check: shares a screen picked through the portal for `THROCC_PROFILE_SECS` seconds (40 by default), and draws the preview too with `THROCC_PREVIEW` set, so a profiler can attach to the printed pid |
