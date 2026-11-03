# throcc-client

The desktop app, built with gpui and gpui-component on top of `throcc-client-core`. `--mock` runs it against sample data with no server, and `--mock --screen <name>` opens one screen as shown in the README's screenshots.

| Module | Holds |
| --- | --- |
| `main.rs` | Arguments, window setup and the saved theme |
| `assets.rs` | The Lucide icons plus the mascot drawings embedded from `assets/mascot` |
| `app.rs` | `ThroccApp`: the current screen and dialog, form inputs, and every action. `show_sample` lists the mock screens |
| `views/` | Rendering, one file per area: `server_bar` (known servers down the left edge, under the traffic lights), `join` (no server connected, or the connection to it lost), `main` (sidebar, lobby, room and a fullscreen share), `person_menu` (right-click on a person: their volumes, profile and stream), `overlay` (the labels and controls over a watched share, one design and size for tiles, fullscreen and pop-outs), `dialogs` (among them a person's profile with your safety number for them, and your own), `settings` (profile, audio, appearance, and everyone verified on this computer), `server` (the server's own pages, among them Privacy and Keys: this server's key and the people here), `banner`, `mascot` (the sets of mascot moods, and the one on screen, picked at random each time its screen shows), `page` (the frame the settings and server pages share: sidebar, page links, fields, segmented choices, the picture editor and Save line; the dialogs use its fields and segmented choices too), and shared pieces in `common` |
| `theme.rs` | The 8-colour palette, its presets and everything mixed from it. The only place colours live |
| `video.rs` | Share pictures handed to gpui as NV12 surfaces or BGRA images. The only place frames meet gpui |
| `popout.rs` | A watched share in a window of its own |
| `avatar.rs` | Profile pictures: scaled to send, decoded to draw (PNG and JPEG only, shrunk to 128 px) |
| `hotkey.rs` | The global push-to-talk key |

`tests/surface.rs` and `tests/surface_windows.rs` check that NV12 surfaces render through the vendored gpui in `third_party/`.
