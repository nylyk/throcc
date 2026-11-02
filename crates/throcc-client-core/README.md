# throcc-client-core

Everything a client does apart from drawing: connecting, keys, voice, screen share and the state the UI shows. Both the desktop client and `throcc-cli` build on it.

## Flow

The UI talks to a `ClientApi` from `client.rs`. It sends `Command`s and reads a whole `State` after every change. `Client` is the real one: its tokio runtime runs the session task in `session.rs`, which signs in through `account.rs`, keeps `State` current, and reconnects into the room it was in. `MockClient` answers locally with sample people and rooms, for screenshots and UI work.

Once signed in, a `Call` (`call.rs`) ties the control `Link` (`control.rs`) to the `MediaEngine` (`media/engine.rs`). The engine seals, fragments and sends our tracks, and receives, opens and reassembles everyone else's. The `KeyManager` (`keys.rs`) gives it a key for each sender and epoch.

## Modules

| Module | Holds |
| --- | --- |
| `client.rs`, `session.rs`, `state.rs` | The UI's API, the session task, and the state it publishes |
| `account.rs`, `connection.rs`, `control.rs` | Sign-in and enrollment, the pinned QUIC connection, the control stream |
| `keystore.rs`, `known_servers.rs`, `contacts.rs`, `profile.rs`, `playback.rs`, `server_look.rs` | What stays on this machine: our keys, pinned servers, pinned peers, our name and picture, how loud we play each person, each server's name and picture as last seen. The identity key lives in the OS keychain (a 0600 `identity_key` file where there is none), the encryption key in `enc_key` |
| `settings.rs`, `servers.rs`, `json_file.rs`, `files.rs` | Where it stays: in the platform config directory, app-wide settings in `settings.json`, everything per server in `servers.json`, and our picture in `profile_picture`. Every JSON change locks and re-reads the file first, so no writer undoes another. Files are written 0600 and replaced in one step |
| `avatars.rs` | Everyone's pictures, users' and servers', in the cache directory by hash. Each use marks a picture used; when a client connects, ones unused for 30 days go, then the least recently used until the cache is under 64 MiB |
| `keys.rs` | Sender secrets per epoch, wrapped to every trusted peer, and the newest epoch this process took part in per room. Once we come back to a room, nothing at or before that epoch is accepted; within one stay, only the epoch just before the current one is, for a short grace |
| `call.rs` | A signed-in connection with its media engine |
| `media/engine.rs`, `fragment.rs`, `reorder.rs` | Sending and receiving media datagrams: fragments, the replay window, the reorder window |
| `media/voice.rs`, `audio.rs`, `devices.rs`, `jitter.rs` | The audio thread: mic processing and Opus, the mix, device rings, jitter buffers |
| `media/cues.rs` | Join, leave, mute, deafen, connection and screen share cues, synthesized once and mixed in by the audio thread |
| `media/share.rs`, `share_audio.rs` | Our running share: capture, encode, send. Its audio, and the audio-led sync of shares we watch |
| `media/capture/` | Screen and system-audio capture per OS: ScreenCaptureKit, Windows Graphics Capture with process loopback, the portal with PipeWire |
| `media/video/` | Encode and decode through ffmpeg, hardware first, and the NV12 frames between them |

Video needs the `ffmpeg` feature. Without it the client does voice only. Audio and capture callbacks never block, allocate, lock or panic; they reach tokio only through bounded rings that drop when full.
