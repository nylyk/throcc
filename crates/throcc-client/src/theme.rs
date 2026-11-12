use std::cell::Cell;
use std::sync::OnceLock;

use gpui::{App, FontWeight, Hsla, Pixels, SharedString, px, rgb};
use gpui_component::{Theme, ThemeMode};
use throcc_client_core::settings::Appearance;
use throcc_client_core::theme::{self as core, oklab};
pub use throcc_client_core::theme::{CUSTOM, format_hex, parse_hex};

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

    pub const fn from_colors(c: [u32; 8]) -> Self {
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

    /// The eight colours as `#rrggbb`, in [`Self::LABELS`] order.
    pub fn to_hex(self) -> Vec<String> {
        core::to_hex(self.colors())
    }

    fn is_light(self) -> bool {
        core::is_light(self.bg)
    }
}

pub const PRESETS: [(&str, Palette); 7] = {
    let mut presets = [("", Palette::from_colors([0; 8])); 7];
    let mut i = 0;
    while i < presets.len() {
        let (name, colors) = core::PRESETS[i];
        presets[i] = (name, Palette::from_colors(colors));
        i += 1;
    }
    presets
};

thread_local! {
    static PALETTE: Cell<Palette> = const { Cell::new(PRESETS[0].1) };
    static TRANSLUCENT_SIDEBAR: Cell<bool> = const { Cell::new(false) };
}

fn palette() -> Palette {
    PALETTE.with(Cell::get)
}

pub fn translucent_sidebar() -> bool {
    TRANSLUCENT_SIDEBAR.with(Cell::get)
}

/// The name and palette of the theme `appearance` picks, the first preset where it picks
/// none that exists.
pub fn picked(appearance: &Appearance) -> (&'static str, Palette) {
    let (name, colors) = core::picked(appearance);
    (name, Palette::from_colors(colors))
}

/// The preset called `name`, ignoring case.
pub fn preset(name: &str) -> Option<(&'static str, Palette)> {
    core::preset(name).map(|(name, colors)| (name, Palette::from_colors(colors)))
}

/// The Custom theme's colours in `appearance`, or the picked theme's until any are saved.
pub fn custom(appearance: &Appearance) -> Palette {
    Palette::from_colors(core::custom(appearance))
}

/// Makes every window show the theme and sidebar `appearance` picks.
pub fn apply(appearance: &Appearance, cx: &mut App) {
    PALETTE.with(|p| p.set(picked(appearance).1));
    let translucent = appearance
        .translucent_sidebar
        .unwrap_or(cfg!(target_os = "macos"));
    TRANSLUCENT_SIDEBAR.with(|t| t.set(translucent));
    install(cx);
    cx.refresh_windows();
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// One palette colour as shown in the theme settings.
pub fn swatch(color: u32) -> Hsla {
    hex(color)
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
/// The sidebar, see-through when picked, over the blurred desktop on macOS and Linux.
pub fn sidebar_glass() -> Hsla {
    if SEES_DESKTOP && translucent_sidebar() {
        surface().opacity(0.9)
    } else {
        surface()
    }
}
/// Whether the main window shows the desktop behind see-through views, blurred where the
/// compositor supports it.
pub const SEES_DESKTOP: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// What shows behind every view: nothing where the window sees the desktop.
pub fn window_bg() -> Hsla {
    if SEES_DESKTOP {
        gpui::transparent_black()
    } else {
        bg()
    }
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
pub fn transparent() -> Hsla {
    gpui::transparent_black()
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
pub fn danger_border() -> Hsla {
    let p = palette();
    mixed(p.bad, 0.35, p.panel)
}
pub fn danger_text() -> Hsla {
    hex(palette().bad)
}
fn danger_hover() -> Hsla {
    let p = palette();
    mixed(p.bad, 0.28, p.panel)
}
/// Behind a fullscreen stream, as in any video player, whatever the theme.
pub fn player_bg() -> Hsla {
    gpui::black()
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
pub const RADIUS_CALL_BUTTON: Pixels = px(16.);
pub const RADIUS_TILE: Pixels = px(12.);
pub const RADIUS_CARD: Pixels = px(14.);
pub const RADIUS_DIALOG: Pixels = px(18.);
pub const TOUCH: Pixels = px(44.);
pub const SIDEBAR_WIDTH: Pixels = px(264.);
pub const SIDEBAR_MIN_WIDTH: Pixels = px(248.);
pub const SIDEBAR_MAX_WIDTH: Pixels = px(420.);
pub const SERVER_BAR_WIDTH: Pixels = px(76.);
pub const ROOM_MIN_WIDTH: Pixels = px(360.);

/// One step of the type scale: a size with its line height and weight.
#[derive(Clone, Copy)]
pub struct TextStyle {
    pub size: Pixels,
    pub line_height: Pixels,
    pub weight: FontWeight,
}

const fn text_style(size: f32, line_height: f32, weight: FontWeight) -> TextStyle {
    TextStyle {
        size: px(size),
        line_height: px(line_height),
        weight,
    }
}

pub const LARGE_TITLE: TextStyle = text_style(26., 32., FontWeight::BOLD);
pub const TITLE_1: TextStyle = text_style(20., 26., FontWeight::SEMIBOLD);
pub const TITLE_2: TextStyle = text_style(18., 24., FontWeight::SEMIBOLD);
pub const TITLE_3: TextStyle = text_style(16., 22., FontWeight::SEMIBOLD);
pub const HEADLINE: TextStyle = text_style(14., 20., FontWeight::SEMIBOLD);
pub const BODY: TextStyle = text_style(14., 20., FontWeight::NORMAL);
pub const CALLOUT: TextStyle = text_style(13., 18., FontWeight::NORMAL);
pub const CAPTION: TextStyle = text_style(12., 16., FontWeight::NORMAL);

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

/// Points gpui-component's own widgets (buttons, inputs, switches, focus rings) at these tokens.
fn install(cx: &mut App) {
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
        theme.font_size = BODY.size;
        theme.radius = RADIUS_BUTTON;
        theme.shadow = false;
        let c = &mut theme.colors;
        c.background = window_bg();
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
}
