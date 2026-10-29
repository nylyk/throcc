# Manual tests

Tests that need real hardware: a display, a GPU, audio devices, or two machines. Each runs with one command.

## M7: two CLI clients hold a call

Needs two machines with a mic and speakers (or headphones), and a running server.
On each machine, once:

```sh
cargo run --release -p throcc-cli -- --profile ./a enroll <server> <invite code>
```

On the first machine, create a room: `cargo run --release -p throcc-cli -- --profile ./a create-room <server> General`.
Then on both:

```sh
cargo run --release -p throcc-cli -- --profile ./a call <server> General
```

Pass: each side prints `speaking: <other name>` while the other talks, and hears them with no echo of their own voice. List devices with `throcc-cli devices` and pick them with `--input` / `--output`. On macOS the terminal asks for microphone access the first time.
