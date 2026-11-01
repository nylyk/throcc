# throcc-client

The desktop app, built with gpui and gpui-component on top of `throcc-client-core`. `--mock` runs it against sample data with no server, and `--mock --screen <name>` opens one screen as shown in the README's screenshots.

| Module | Holds |
| --- | --- |
| `main.rs` | Arguments, window setup and the saved theme |
| `app.rs` | `ThroccApp`: the current screen and dialog, form inputs, and every action. `show_sample` lists the mock screens |
| `views/` | Rendering, one file per area: `server_bar` (known servers down the left edge, under the traffic lights), `join` (no server connected), `main` (lobby and room), `person_menu` (right-click on a person: their volumes and stream), `dialogs`, `settings` (profile, audio, sounds, appearance, servers), `server`, `banner`, and shared pieces in `common` |
| `theme.rs` | The 8-colour palette, its presets and everything mixed from it. The only place colours live |
| `video.rs` | Share pictures handed to gpui as NV12 surfaces or BGRA images. The only place frames meet gpui |
| `popout.rs` | A watched share in a window of its own |
| `avatar.rs` | Profile pictures: scaled to send, decoded to draw |
| `hotkey.rs` | The global push-to-talk key |

`tests/surface.rs` and `tests/surface_windows.rs` check that NV12 surfaces render through the vendored gpui in `third_party/`.
