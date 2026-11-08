# throcc-client

The desktop app, built with gpui and gpui-component on top of `throcc-client-core`. `--mock` runs it against sample data with no server, and `--mock --screen <name>` opens one screen as shown in the README's screenshots.

| Module | Holds |
| --- | --- |
| `main.rs` | Arguments, the saved settings, the audio thread and window setup. Without an audio thread the app doesn't start |
| `assets.rs` | The Lucide icons plus the mascot drawings embedded from `assets/mascot` |
| `app.rs` | `ThroccApp`: the current screen and dialog, form inputs, and every action. It owns the settings, kept whole to `settings.json` as it changes, and the audio thread, which runs from start to exit with or without a server and is handed to each session it connects. `show_sample` lists the mock screens |
| `views/` | Rendering, one file per area: `onboarding` (the first screen, until a name is kept: the name every server knows us by and an optional picture), `server_bar` (known servers down the left edge, under the traffic lights), `join` (no server connected, or the connection to it lost), `main` (sidebar, lobby, room and a fullscreen share), `person_menu` (right-click on a person: their volumes, profile and stream), `overlay` (the labels and controls over a watched share, one design and size for tiles, fullscreen and pop-outs), `dialogs` (among them a person's profile with your safety number for them, and your own), `settings` (profile, audio, appearance, and everyone verified on this computer), `server` (the server's own pages, among them Privacy and Keys: this server's key and the people here), `banner`, `mascot` (the sets of mascot moods, and the one on screen, picked at random each time its screen shows), `part` (the parts of the main window drawn as cached views that redraw when the app changes but not when only a share does: the server bar, the sidebar, the room's header and controls, people's cards and the labels over shares), `page` (the frame the settings and server pages share: sidebar, page links, fields, segmented choices, the picture editor and Save line; the onboarding screen uses its fields, picture editor and Save line, and the dialogs its fields), and shared pieces in `common` |
| `theme.rs` | The 8-colour palette, its presets and everything mixed from it. The only place colours live |
| `video.rs` | Share pictures handed to gpui as NV12 surfaces, or on macOS as the pixel buffers they were captured or decoded into. Each share on screen is a view of its own that redraws when its picture changes. The only place frames meet gpui |
| `popout.rs` | A watched share in a window of its own |
| `avatar.rs` | Profile pictures: scaled to send, decoded to draw (PNG and JPEG only, shrunk to 128 px) |
| `hotkey.rs` | The global push-to-talk key |
| `keep_display_awake.rs` | Keeps the display on while someone else's share is watched |

`tests/surface.rs` (wgpu on Linux, Metal on macOS) and `tests/surface_windows.rs` check that NV12 surfaces render through the vendored gpui in `third_party/`.
