# throcc-client

The desktop app, built with gpui and gpui-component on top of `throcc-client-core`. `--mock` runs it against sample data with no server, and `--mock --screen <name>` opens one screen as shown in the README's screenshots.

| Module | Holds |
| --- | --- |
| `main.rs` | Arguments, the saved settings, the audio thread and window setup. Without an audio thread the app doesn't start |
| `assets.rs` | The Lucide icons plus the mascot drawings embedded from `assets/mascot` |
| `app.rs` | `ThroccApp`: the current screen and dialog, form inputs, and every action. It owns the settings, kept whole to `settings.json` as it changes, and the audio thread, which runs from start to exit with or without a server and is handed to each session it connects. `show_sample` lists the mock screens |
| `views/` | Rendering, one file per area: `onboarding` (the first screen, until a name is kept: the name every server knows us by and an optional picture), `server_bar` (known servers down the left edge, under the traffic lights), `join` (no server connected, or the connection to it lost: reconnecting, waiting for a Reconnect click after a long gap, or signed in on another device), `main` (sidebar, lobby, room and a fullscreen share), `person_menu` (right-click on a person: their volumes, profile and stream, and Block; a blocked person's row opens it on a click too, with only Unblock), `overlay` (the labels and controls over a watched share, one design and size for tiles, fullscreen and pop-outs), `dialogs` (among them a person's profile with your safety number for them, and your own), `settings` (profile, audio, appearance, everyone verified on this computer, everyone blocked on it, and About: the version and the update check), `server` (the server's own pages, among them Privacy and Keys: this server's key and the people here), `banner`, `mascot` (the sets of mascot moods, and the one on screen, picked at random each time its screen shows), `part` (the parts of the main window drawn as cached views that redraw when the app changes but not when only a share does: the server bar, the sidebar, the room's header and controls, people's cards and the labels over shares), `page` (the frame the settings and server pages share: sidebar, page links, fields, segmented choices, the picture editor and Save line; the onboarding screen uses its fields, picture editor and Save line, and the dialogs its fields), and shared pieces in `common` |
| `theme.rs` | The 8-colour palette and everything mixed from it. The presets and the picked theme come from the core's `theme.rs`. The only place the desktop's colours live |
| `video.rs` | Share pictures handed to gpui as NV12 surfaces, or on macOS as the pixel buffers they were captured or decoded into. Each share on screen is a view of its own that redraws when its picture changes. The only place frames meet gpui |
| `popout.rs` | A watched share in a window of its own |
| `avatar.rs` | Profile pictures: scaled to send, decoded to draw (PNG and JPEG only, shrunk to 128 px) |
| `hotkey.rs` | The global push-to-talk key |
| `updates.rs` | The update check: the newest `client-v` release on GitHub, if newer than this build. It is off in a flatpak, which flatpak updates, until the user turns it on |
| `keep_display_awake.rs` | Keeps the display on while someone else's share is watched: the power APIs on macOS and Windows; on Linux the desktop portal, plus `idle_inhibit.rs` on each window showing a share |
| `idle_inhibit.rs` | Wayland's idle-inhibit protocol on a window's surface, bound through a queue of our own on gpui's display. Compositors honour it while the surface is visible, and idle daemons such as hypridle and swayidle follow the compositor, so it works where the portal has no Inhibit backend, as on Hyprland |

`tests/surface.rs` (wgpu on Linux, Metal on macOS) and `tests/surface_windows.rs` check that NV12 surfaces render through the vendored gpui in `third_party/`.
