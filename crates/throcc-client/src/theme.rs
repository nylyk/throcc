use std::cell::Cell;
use std::sync::OnceLock;

use gpui::{App, Hsla, Pixels, SharedString, px, rgb};
use gpui_component::{Theme, ThemeMode};

/// The eight colours a theme is made of. Every other colour is mixed from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub warning: u32,
    pub good: u32,
    pub bad: u32,
}

impl Palette {
    pub const LABELS: [&str; 8] = [
        "Background",
        "Panel",
        "Text",
        "Muted text",
        "Accent",
        "Warning",
        "Good",
        "Bad",
    ];

    pub fn colors(self) -> [u32; 8] {
        [
            self.bg,
            self.panel,
            self.text,
            self.muted,
            self.accent,
            self.warning,
            self.good,
            self.bad,
        ]
    }

    pub fn from_colors(c: [u32; 8]) -> Self {
        Self {
            bg: c[0],
            panel: c[1],
            text: c[2],
            muted: c[3],
            accent: c[4],
            warning: c[5],
            good: c[6],
            bad: c[7],
        }
    }

    /// One line of eight `#rrggbb` colours, in [`Self::LABELS`] order.
    pub fn to_line(self) -> String {
        let colors: Vec<String> = self.colors().iter().map(|c| format_hex(*c)).collect();
        colors.join(" ")
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let colors: Vec<u32> = line
            .split_whitespace()
            .map(parse_hex)
            .collect::<Option<_>>()?;
        Some(Self::from_colors(colors.try_into().ok()?))
    }

    fn is_light(self) -> bool {
        oklab(self.bg)[0] > 0.6
    }
}

pub const PRESETS: [(&str, Palette); 4] = [
    (
        "Ink",
        Palette {
            bg: 0x0C0D11,
            panel: 0x14161D,
            text: 0xECEDF3,
            muted: 0x8E92A6,
            accent: 0x8C7BFF,
            warning: 0xFFB86B,
            good: 0x5BE3A4,
            bad: 0xFF6B85,
        },
    ),
    (
        "Paper",
        Palette {
            bg: 0xF6F4EF,
            panel: 0xFFFFFF,
            text: 0x1C1B22,
            muted: 0x6B6878,
            accent: 0x5B4BDB,
            warning: 0xD9822B,
            good: 0x1F9D6A,
            bad: 0xD9445E,
        },
    ),
    (
        "Moss",
        Palette {
            bg: 0x0F1411,
            panel: 0x161D18,
            text: 0xE8EFE9,
            muted: 0x8C9A90,
            accent: 0x7FD1A8,
            warning: 0xE8C468,
            good: 0x8BE07D,
            bad: 0xF0786E,
        },
    ),
    (
        "Dusk",
        Palette {
            bg: 0x15101A,
            panel: 0x1E1724,
            text: 0xF1EAF4,
            muted: 0x9C8FA6,
            accent: 0xFF8FB8,
            warning: 0xFFC285,
            good: 0x7FE0C4,
            bad: 0xFF6F6F,
        },
    ),
];

/// `#rrggbb`, `rrggbb` or `#rgb` as a colour.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.trim().trim_start_matches('#');
    let digits: String = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).collect(),
        6 => digits.to_owned(),
        _ => return None,
    };
    u32::from_str_radix(&digits, 16).ok()
}

pub fn format_hex(color: u32) -> String {
    format!("#{color:06X}")
}

thread_local! {
    static PALETTE: Cell<Palette> = const { Cell::new(PRESETS[0].1) };
}

pub fn palette() -> Palette {
    PALETTE.with(Cell::get)
}

const APPEARANCE_FILE: &str = "appearance";

/// The palette chosen last time, if one was saved.
pub fn saved() -> Option<Palette> {
    let dir = throcc_client_core::keystore::default_dir().ok()?;
    Palette::from_line(&std::fs::read_to_string(dir.join(APPEARANCE_FILE)).ok()?)
}

pub fn save(palette: Palette) {
    let saved = throcc_client_core::keystore::default_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(APPEARANCE_FILE), palette.to_line() + "\n")?;
        Ok(())
    });
    if let Err(e) = saved {
        tracing::warn!("could not save the theme: {e:#}");
    }
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// One palette colour as shown in the theme settings.
pub fn swatch(color: u32) -> Hsla {
    hex(color)
}

fn srgb_to_linear(channel: u32) -> f32 {
    let c = channel as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u32 {
    let c = c.clamp(0.0, 1.0);
    let c = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round() as u32
}

fn oklab(color: u32) -> [f32; 3] {
    let [r, g, b] = [color >> 16, color >> 8, color].map(|c| srgb_to_linear(c & 0xFF));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn from_oklab([l, a, b]: [f32; 3]) -> u32 {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let b = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    (linear_to_srgb(r) << 16) | (linear_to_srgb(g) << 8) | linear_to_srgb(b)
}

/// `share` of `top` over `base`, mixed in Oklab like CSS `color-mix(in oklab, …)`.
fn mix(top: u32, share: f32, base: u32) -> u32 {
    let (t, b) = (oklab(top), oklab(base));
    from_oklab(std::array::from_fn(|i| t[i] * share + b[i] * (1.0 - share)))
}

fn mixed(top: u32, share: f32, base: u32) -> Hsla {
    hex(mix(top, share, base))
}

pub fn bg() -> Hsla {
    hex(palette().bg)
}
pub fn sidebar() -> Hsla {
    hex(palette().panel)
}
pub fn surface() -> Hsla {
    hex(palette().panel)
}
pub fn raised() -> Hsla {
    let p = palette();
    mixed(p.text, 0.06, p.panel)
}
pub fn selected() -> Hsla {
    let p = palette();
    mixed(p.accent, 0.18, p.panel)
}
pub fn control() -> Hsla {
    let p = palette();
    mixed(p.text, 0.10, p.panel)
}
pub fn border_strong() -> Hsla {
    let p = palette();
    mixed(p.text, 0.16, p.panel)
}
pub fn text() -> Hsla {
    hex(palette().text)
}
pub fn text_2() -> Hsla {
    let p = palette();
    mixed(p.text, 0.7, p.muted)
}
pub fn text_2_muted() -> Hsla {
    let p = palette();
    mixed(p.text, 0.3, p.muted)
}
pub fn text_3() -> Hsla {
    hex(palette().muted)
}
pub fn accent() -> Hsla {
    hex(palette().accent)
}
pub fn on_accent() -> Hsla {
    hex(palette().bg)
}
pub fn link() -> Hsla {
    let p = palette();
    mixed(p.accent, 0.8, p.text)
}
pub fn speaking() -> Hsla {
    hex(palette().good)
}
pub fn ok_bg() -> Hsla {
    let p = palette();
    mixed(p.good, 0.15, p.panel)
}
pub fn ok_text() -> Hsla {
    hex(palette().good)
}
pub fn warn_bg() -> Hsla {
    let p = palette();
    mixed(p.warning, 0.14, p.panel)
}
pub fn warn_border() -> Hsla {
    let p = palette();
    mixed(p.warning, 0.35, p.panel)
}
pub fn warn_text() -> Hsla {
    hex(palette().warning)
}
pub fn danger_bg() -> Hsla {
    let p = palette();
    mixed(p.bad, 0.18, p.panel)
}
pub fn danger_text() -> Hsla {
    hex(palette().bad)
}
fn danger_hover() -> Hsla {
    let p = palette();
    mixed(p.bad, 0.28, p.panel)
}
/// Behind a stream, where its letterbox bars show.
pub fn stage() -> Hsla {
    let p = palette();
    mixed(p.accent, 0.06, p.bg)
}
/// Behind a fullscreen stream, as in any video player, whatever the theme.
pub fn player_bg() -> Hsla {
    gpui::black()
}
/// Text and icons over a fullscreen stream.
pub fn player_text() -> Hsla {
    gpui::white()
}
pub fn player_hover() -> Hsla {
    gpui::white().opacity(0.16)
}
/// The shade behind a fullscreen stream's controls, darkest at the edge `toward_top` says.
pub fn player_shade(toward_top: bool) -> gpui::Background {
    let angle = if toward_top { 0. } else { 180. };
    gpui::linear_gradient(
        angle,
        gpui::linear_color_stop(gpui::black().opacity(0.0), 0.0),
        gpui::linear_color_stop(gpui::black().opacity(0.75), 1.0),
    )
}
/// Dims the window behind a dialog.
pub fn scrim() -> Hsla {
    gpui::black().opacity(0.6)
}
/// Labels laid over a stream.
pub fn overlay() -> Hsla {
    surface().opacity(0.85)
}

/// Avatar hues mixed from the palette, picked by user id.
pub fn avatar(seed: u64) -> Hsla {
    let p = palette();
    let hues = [
        p.accent,
        p.warning,
        mix(p.accent, 0.5, p.good),
        mix(p.warning, 0.5, p.bad),
        p.good,
        mix(p.accent, 0.5, p.bad),
    ];
    hex(hues[(seed % hues.len() as u64) as usize])
}

pub fn on_avatar() -> Hsla {
    bg()
}

pub const RADIUS_ROW: Pixels = px(8.);
pub const RADIUS_BUTTON: Pixels = px(10.);
pub const RADIUS_TILE: Pixels = px(12.);
pub const RADIUS_CARD: Pixels = px(14.);
pub const RADIUS_DIALOG: Pixels = px(18.);
pub const TOUCH: Pixels = px(44.);
pub const SIDEBAR_WIDTH: Pixels = px(264.);

const FONTS: &[&str] = &[
    "Geist",
    ".SystemUIFont",
    "Inter",
    "Segoe UI",
    "Cantarell",
    "Noto Sans",
    "DejaVu Sans",
];
const MONO_FONTS: &[&str] = &[
    "Geist Mono",
    "SF Mono",
    "Menlo",
    "Cascadia Mono",
    "Consolas",
    "DejaVu Sans Mono",
];
static FAMILIES: OnceLock<(SharedString, SharedString)> = OnceLock::new();

/// The UI font: Geist when installed, else the platform's own.
pub fn font() -> SharedString {
    FAMILIES
        .get()
        .map_or_else(|| FONTS[0].into(), |f| f.0.clone())
}

pub fn mono_font() -> SharedString {
    FAMILIES
        .get()
        .map_or_else(|| MONO_FONTS[0].into(), |f| f.1.clone())
}

fn pick_families(cx: &App) -> (SharedString, SharedString) {
    let installed = cx.text_system().all_font_names();
    (
        first_installed(FONTS, &installed).into(),
        first_installed(MONO_FONTS, &installed).into(),
    )
}

/// The first candidate that is installed. Names starting with a dot are macOS system aliases.
fn first_installed(candidates: &[&'static str], installed: &[String]) -> &'static str {
    candidates
        .iter()
        .find(|c| {
            (c.starts_with('.') && cfg!(target_os = "macos")) || installed.iter().any(|i| i == *c)
        })
        .copied()
        .unwrap_or(candidates[0])
}

/// Height shared by the sidebar header and the room header, so their borders line up.
pub const HEADER_HEIGHT: Pixels = px(56.);
/// Height shared by the profile bar and the control bar.
pub const FOOTER_HEIGHT: Pixels = px(68.);

/// Makes `palette` the theme of every window.
pub fn set(palette: Palette, cx: &mut App) {
    PALETTE.with(|p| p.set(palette));
    install(cx);
    cx.refresh_windows();
}

/// Points gpui-component's own widgets (buttons, inputs, switches, focus rings) at these tokens.
pub fn install(cx: &mut App) {
    let families = FAMILIES.get_or_init(|| pick_families(cx)).clone();
    let mode = if palette().is_light() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = families.0;
        theme.mono_font_family = families.1;
        theme.font_size = px(14.);
        theme.radius = RADIUS_BUTTON;
        theme.shadow = false;
        let c = &mut theme.colors;
        c.background = bg();
        c.foreground = text();
        c.muted = raised();
        c.muted_foreground = text_3();
        c.border = control();
        c.input = border_strong();
        c.ring = accent();
        c.accent = raised();
        c.accent_foreground = text();
        c.popover = surface();
        c.popover_foreground = text();
        c.selection = accent().opacity(0.3);
        c.switch = control();
        c.link = link();
        c.caret = accent();
        c.primary = accent();
        c.primary_foreground = on_accent();
        c.primary_hover = link();
        c.primary_active = link();
        c.button = control();
        c.button_foreground = text();
        c.button_hover = border_strong();
        c.button_active = border_strong();
        c.button_primary = accent();
        c.button_primary_foreground = on_accent();
        c.button_primary_hover = link();
        c.button_primary_active = link();
        c.button_secondary = control();
        c.button_secondary_foreground = text();
        c.button_secondary_hover = border_strong();
        c.button_secondary_active = border_strong();
        c.button_danger = danger_bg();
        c.button_danger_foreground = danger_text();
        c.button_danger_hover = danger_hover();
        c.button_danger_active = danger_hover();
        c.secondary = control();
        c.secondary_foreground = text();
        c.secondary_hover = border_strong();
        c.secondary_active = border_strong();
        c.danger = danger_text();
        c.warning = warn_text();
        c.success = ok_text();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_matches_css_color_mix_in_oklab() {
        assert_eq!(mix(0xFFFFFF, 0.0, 0x123456), 0x123456);
        assert_eq!(mix(0xFFFFFF, 1.0, 0x123456), 0xFFFFFF);
        let grey = mix(0xFFFFFF, 0.5, 0x000000);
        assert!((0x60..=0x66).contains(&(grey & 0xFF)), "{grey:06X}");
    }

    #[test]
    fn a_palette_survives_its_own_line() {
        for (_, palette) in PRESETS {
            assert_eq!(Palette::from_line(&palette.to_line()), Some(palette));
        }
        assert_eq!(Palette::from_line("#000000 #FFFFFF"), None);
    }

    #[test]
    fn hex_accepts_short_and_bare_forms() {
        assert_eq!(parse_hex("#8C7BFF"), Some(0x8C7BFF));
        assert_eq!(parse_hex("8c7bff"), Some(0x8C7BFF));
        assert_eq!(parse_hex("#fa0"), Some(0xFFAA00));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
    }

    #[test]
    fn only_paper_is_light() {
        let light: Vec<&str> = PRESETS
            .iter()
            .filter(|(_, p)| p.is_light())
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(light, ["Paper"]);
    }
}
