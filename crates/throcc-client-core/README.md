# throcc-client-core

Everything a client does apart from drawing: connecting, keys, voice, screen share and the state the UI shows. Both the desktop client and `throcc-cli` build on it.

## Flow

The UI talks to a `ClientApi` from `client.rs`. It sends `Command`s and reads a whole `State` after every change. `Client` is the real one: its tokio runtime runs the session task in `session.rs`, which signs in through `account.rs`, keeps `State` current, and reconnects into the room it was in. `MockClient` answers locally with sample people and rooms, for screenshots and UI work.

Once signed in, a `Call` (`call.rs`) ties the control `Link` (`control.rs`) to the `MediaEngine` (`media/engine.rs`). The engine seals, fragments and sends our tracks, and receives, opens and reassembles everyone else's. The `KeyManager` (`keys.rs`) gives it a key for each sender and epoch.

Trust follows identity keys, not servers. The key manager wraps our sender secret to every room peer whose encryption key is validly signed by the identity key it presents, for this server and its user id. The session's `Trust` (`trust.rs`) notes those identity keys and checks them against `VerifiedKeys` (`verified.rs`): the keys the user verified by safety number, kept once for the whole install, so someone verified on one server shows as verified on every server where their key appears. Only a key presented this connection counts: someone verified before shows as not verified, with no safety number, until they present that key in a room with us again, and then for the rest of the connection, even once they left the room. Everyone not verified is marked so but still hears us. Verifying names the key whose safety number the user compared, and is refused once that user presents another. Each verified key also remembers the server and user id pairs it was seen under; when a server shows a different, unverified key for one of them, `people.json` keeps a key-changed warning for that pair, across restarts and even once the old key is back, until the new key is verified, the old one unverified or the warning dismissed. Dismissing it while the verified key is back keeps watching the pair for the next change. While a warning stands, the newest key shown there is kept in memory and written with the next change to the file, so a server flipping keys can't make every room state rewrite it.

## Modules

| Module | Holds |
| --- | --- |
| `client.rs`, `session.rs`, `state.rs` | The UI's API, the session task, and the state it publishes |
| `trust.rs` | Who is verified on one server: the identity keys presented this connection, checked against the verified ones, and the key-changed warnings |
| `account.rs`, `connection.rs`, `control.rs` | Sign-in and enrollment, the pinned QUIC connection, the control stream. Each enrollment takes a new encryption key and drops the epoch floors kept for the user id it gets, which may have stood for an earlier account of ours |
| `keystore.rs`, `known_servers.rs`, `verified.rs`, `floors.rs`, `profile.rs`, `playback.rs`, `server_look.rs` | What stays on this machine: our keys, pinned servers, the identity keys we verified, the newest epoch we took part in per room, our name and picture, how loud we play each person, saved under the identity key they present in a room with us and held in memory until they do, each server's name and picture as last seen. The identity key lives in the OS keychain (a 0600 `identity_key` file where there is none), the encryption key in `enc_key` |
| `settings.rs`, `people.rs`, `servers.rs`, `json_file.rs`, `files.rs` | Where it stays: in the platform config directory, app-wide settings in `settings.json`, everything per person by identity key in `people.json` (the verified keys with the server and user id pairs each was seen under, and playback), everything per server (epoch floors included) in `servers.json`, and our picture in `profile_picture`. Every JSON change locks and re-reads the file first, so no writer undoes another. Files are written 0600 and replaced in one step. Each JSON file carries a `version`: an older one is migrated forward through its `json_file::Format::MIGRATIONS` as it loads, and a newer one is refused |
| `avatars.rs` | Everyone's pictures, users' and servers', in the cache directory by hash. Each use marks a picture used; when a client connects, ones unused for 30 days go, then the least recently used until the cache is under 64 MiB |
| `keys.rs` | Sender secrets per epoch, wrapped to every peer with a validly signed encryption key; a new secret whenever those peers or their keys change, so a peer's routine encryption-key renewal starts a new secret too. Once we come back to a room, nothing at or before the newest epoch we took part in there (`floors.rs`, saved on each placement, every few seconds after, on disconnect and at exit, and kept across restarts) is accepted; within one stay, only the epoch just before the current one is, for a short grace |
| `call.rs` | A signed-in connection with its media engine |
| `media/engine.rs`, `fragment.rs`, `reorder.rs` | Sending and receiving media datagrams: fragments, the replay window, the reorder window |
| `media/voice.rs`, `audio.rs`, `devices.rs`, `jitter.rs` | The audio thread: mic processing and Opus, the mix, device rings, jitter buffers. Devices open at 48 kHz where they offer it; on macOS, where opening at another rate would change the device's rate for every app, at their current rate when 10 ms of it is a whole number of frames. Otherwise they open at their current rate under the same condition, or not at all. The audio thread resamples the rest |
| `media/cues.rs` | Join, leave, mute, deafen, connection and screen share cues, synthesized once and mixed in by the audio thread |
| `media/share.rs`, `share_audio.rs` | Our running share: capture, encode, send. Its audio, and the audio-led sync of shares we watch |
| `media/capture/` | Screen and system-audio capture per OS: ScreenCaptureKit, Windows Graphics Capture with process loopback, the portal with PipeWire. The portal keeps nothing of what was picked, so every share asks again |
| `media/video/` | Encode and decode through ffmpeg, hardware first, and the NV12 frames between them |

Video needs the `ffmpeg` feature. Without it the client does voice only. Audio and capture callbacks never block, allocate, lock or panic; they reach tokio only through bounded rings that drop when full.

## Manual hardware tests

These tests are `#[ignore]`d because they need a display, a desktop session or audio hardware, and no CI job runs them. Run one by name on a machine that has what its `ignore` reason names:

```sh
cargo test -p throcc-client-core --features ffmpeg --lib -- --ignored <name>
```

| OS | Test | Needs |
| --- | --- | --- |
| Linux | `pipewire_source_delivers_converted_frames` | A PipeWire daemon and a session manager |
| Linux | `desktop_audio_has_other_apps_but_not_ours` | The same, an audio sink and `pw-play` |
| Linux | `portal_share_delivers_frames_and_asks_every_time` | A desktop session with xdg-desktop-portal, and someone to pick a screen |
| macOS | `main_display_delivers_frames`, `share_audio_arrives_in_order_with_sound` | A display, the screen recording permission, and speakers for the second |
| macOS | `sending_1440p60_takes_under_one_core` | The same, with moving content on the display |
| Windows | `primary_monitor_delivers_frames`, `share_audio_arrives_in_order_with_sound` | A display, a GPU, and speakers for the second |

The VA-API tests (`gpu_input`) and the VideoToolbox ones run in the normal suite and print `skipped:` where the hardware is missing.
