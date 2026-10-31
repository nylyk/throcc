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

## Zero-copy screen share on Linux

Needs a Linux desktop whose portal shares DMA-BUFs (Hyprland, GNOME, KDE), a GPU with VA-API video processing and encoding (Mesa `radeonsi`, or Intel's `iHD`), and the LGPL ffmpeg from `packaging/ffmpeg/build.sh` with its bindings. Keep a monitor animating, for example `mpv --loop=inf --fs 'av://lavfi:testsrc2=size=2560x1440:rate=60'`. In one terminal:

```sh
export FFMPEG_DLL_PATH=<ffmpeg>/lib FFMPEG_BINDING_PATH=<ffmpeg>/binding.rs LD_LIBRARY_PATH=<ffmpeg>/lib
cargo test --release -p throcc-client-core --features ffmpeg --lib gpu_input -- --nocapture
THROCC_PROFILE_SECS=45 cargo test --release -p throcc-cli --features ffmpeg --test share a_screen_share_runs_for_profiling -- --ignored --nocapture
```

While the second test shares, in another terminal:

```sh
pw-dump | grep -A3 '"modifier"'
pidstat -t -u -p <pid it prints> 30 1
```

Pass: the `gpu_input` tests pass, which import an exported VA surface, check red and blue against full-range BT.709 and check the black bars. `pw-dump` shows the stream's `Format` with a `modifier`. `throcc-share` uses a few percent of a core, and well under the 65 % the CPU path took for 2560×1440 at 60 fps. Run the share test again with `THROCC_PREVIEW=1`: still a few percent. Then share from the client and open and close the self-view: it appears within a second and looks right. For the fallback, run the share with an encoder that takes no GPU frames (on a machine with NVENC, or after renaming the VA driver): the log says `falling back to frames in shared memory`, the share keeps going, and `pw-dump` shows a `Format` without a `modifier`.

## Shares drawn as NV12 surfaces on Linux and Windows

Needs a Linux desktop with an AMD GPU and ffmpeg with VA-API (Mesa's `radeonsi` VA driver), a Windows machine with a GPU, and a third machine sharing 2560×1440 at 60 fps with moving content (a video full screen). Run the client built with `--features ffmpeg` and watch the share:

```sh
RUST_LOG=throcc_client_core=info,gpui_wgpu=info cargo run --release -p throcc-client --features ffmpeg
```

- **Linux:** the log says `decoding <codec> in hardware` and names the AMD GPU as the wgpu adapter. Pass: the picture is sharp and moves smoothly, colours match the sharer's screen (a red, green and blue test image shows no hue shift, black is black and white is white, not grey), and the tile's rounded corners show the tile behind them, in the grid, the focus view, fullscreen and a pop-out. `top -H -p $(pgrep throcc-client)` shows the main thread's CPU; note it, and compare it with the same share watched in a build from before the NV12 surface (which converted each frame to BGRA on that thread): it should be several times lower.
- **Windows:** the same checks, with Task Manager's Details tab for the main thread's CPU. Then resize the window quickly and change the share's resolution on the sender: the picture follows without flicker or stale frames. Unplug a monitor or switch GPUs (a device-lost) while watching: the picture comes back.

## M10: macOS share audio

Needs a Mac with the screen recording permission and speakers.

```sh
cargo test -p throcc-client-core --lib share_audio_arrives -- --ignored
```

Pass: it plays a system sound and captures it. Then, in a call between two machines, share a screen with audio while the other side talks. Pass: the other side never hears their own voice come back, and a clap video shared with audio looks and sounds in sync (offset under 40 ms, judged by eye or with a phone's slow-motion camera).

## M10: Windows share audio

Needs a Windows 10 2004 or newer machine with a GPU and speakers. First, the capture alone:

```sh
cargo test -p throcc-client-core --lib share_audio_arrives -- --ignored
```

Pass: it plays `C:\Windows\Media\Alarm01.wav` from PowerShell and captures it. Over 100 audio frames arrive in 4 s, timestamps rise, at least 90 % of steps are exactly 960, and the loudest sample is above 0.01.

Then, in a call between two machines, share the screen with audio from Windows while the other side talks. Pass: the other side never hears their own voice come back. Play a video with sound on the Windows machine: the other side hears it. Share a clap video with audio: it looks and sounds in sync (offset under 40 ms). Share a single window: other apps' sound is shared too (expected until include mode is possible).

## M10: Linux share audio

Needs a Linux desktop with PipeWire, a session manager, the portal (as for M9), speakers, and a second machine in the call.

```sh
cargo test -p throcc-client-core --lib desktop_audio_has_other_apps -- --ignored --nocapture
```

Pass: it prints the other app's tone near 0.3 and ours near 0. Then, in a call between two machines, share a screen with audio on the Linux machine while a video plays in a browser and the other side talks. Pass: the other side hears the video, and never hears their own voice come back. Check both with throcc playing through `pipewire-pulse` and through the PipeWire ALSA plugin if the desktop has both. While sharing, `pw-link -l` shows `throcc-share-audio-<pid>` fed by the browser's stream and not by throcc's own playback stream. A clap video shared with audio looks and sounds in sync (offset under 40 ms).

## M11: packaged clients enroll and talk

Needs a server installed from `packaging/throcc-server.service` (or the container image), a Mac and an Arch machine. Download `throcc-macos` (the `.dmg`) and `throcc-windows` from the latest `packaging` workflow run, and build the Arch package with `makepkg -si` in `packaging/arch`.

Pass: on each machine the packaged client opens (on macOS with right-click, Open the first time), joins with an invite code, shows the server fingerprint from the server's log, and holds a call with the others; sharing a screen shows the picture on the other side.

## Share audio stays clean across gaps and rejoins

Needs two machines in a call, one sharing with audio (test each of macOS, Windows and Linux as the sharer).

Pass: play music on the sharer, pause it for a few seconds, play again; the watcher hears clean audio after each pause, with no crackle or hiss. Stop watching and watch again three times; each time the audio is clean within a second.

## Audio settings

Needs a client connected to a server, a mic and speakers.

Pass: on the audio page the meter moves with your voice and turns green above the threshold mark; dragging the threshold above your speaking level stops the speaking ring, and below it brings it back. Input volume at 0 % silences you; output volume changes how loud others and shared audio play. Picking a device in either dropdown switches to it at once. Switching the output device while watching a share with audio, with one person turned down and the mic test on, keeps the share audio in sync, that person quiet and the mic test running.

## macOS window chrome

Needs a Mac.

Pass: the main window has no system titlebar; the traffic lights sit in the sidebar's top bar, left of the server name, and don't overlap it. Dragging any top bar moves the window, a double-click zooms it, and the buttons in the bars still work. The sidebar shows the desktop blurred behind it. Dragging the sidebar's right edge resizes it between about 250 and 420 px, on the main and settings screens. A pop-out shows the traffic lights over the picture with the title right of them; dragging the picture moves it.
