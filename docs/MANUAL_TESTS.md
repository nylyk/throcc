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

## M9: macOS screen capture and sender load

Needs a Mac with the screen recording permission granted to the terminal, and `brew install ffmpeg@8`.

```sh
export FFMPEG_PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig
cargo test -p throcc-client-core --features ffmpeg --lib main_display -- --ignored
```

Pass: the main display delivers at least 10 frames in 3 s. For the sender load, play moving content full screen first, then run the 1440p60 test in release:

```sh
ffplay -fs -autoexit -t 14 -f lavfi -i testsrc2=size=2560x1440:rate=60 &
cargo test --release -p throcc-client-core --features ffmpeg --lib sending_1440p60 -- --ignored --nocapture
```

Pass: over 50 fps and under one core.

## M9: share latency and loss recovery

Needs a machine with a hardware video encoder (any Apple silicon Mac; CI's virtual machines queue frames for seconds).

```sh
cargo test -p throcc-cli --features ffmpeg --test share -- --ignored --nocapture
```

Pass: both tests pass. The first prints the encode-to-decoded latency (16 ms median at 1080p60 on an M-series MacBook); the second shows every keyframe request answered within a second at 2 % loss.

## M9: share between two desktop clients

Needs two machines with the client built with `--features ffmpeg` and a running server. Join one room on both, press Share on one and pick a window in the system picker. Pass: the other side sees the window within a second of pressing Watch, and the sharer sees their own self-view.

## M9: Windows screen capture and sender load

Needs a Windows 10 2004 or newer machine with a GPU. First, the capture alone, with something moving on the primary monitor:

```sh
cargo test -p throcc-client-core --lib primary_monitor -- --ignored
```

Pass: at least 10 frames in 3 s, timestamps rising, picture not uniform.

Then the client, built with `--features ffmpeg`, in a room with a second client. Press Share.

- The system picker opens over the throcc window. Pick a window. Pressing Cancel in the picker leaves the call as it was, with a notice that nothing was picked.
- On Windows 11 no yellow border appears around the shared window or screen. On Windows 10 a border is expected.
- The cursor shows in the shared picture.
- Resize the shared window: the viewer's picture follows, letterboxed, without stretching.
- Share the High preset (2560×1440 at 60 fps) of a screen playing moving video full screen. In Task Manager's Details tab, add the CPU time column or use `Get-Process throcc-client | Select CPU` twice 10 s apart. Pass: the sender uses under one core, and the viewer sees about 60 fps.
- On a 120 Hz or faster display, the viewer still gets about 60 fps, not the display rate.

## M9: Linux screen share through the portal

Needs a Wayland or X11 desktop with `xdg-desktop-portal` and its desktop backend (GNOME, KDE or wlr), PipeWire, and `libpipewire-0.3-dev`. Keep something moving on screen, such as a video, since compositors send frames only on change.

```sh
cargo test -p throcc-client-core --lib portal_share -- --ignored --nocapture
```

Pass: the portal's dialog appears once; pick a monitor or a window. Both shares print a size and get at least 10 frames, and the second share starts without a dialog. To ask again, delete `~/.local/share/throcc/screencast-restore-token`.
