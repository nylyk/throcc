use std::sync::OnceLock;

use gpui::{App, Hsla, Pixels, SharedString, px, rgb};
use gpui_component::{Theme, ThemeMode};

/// A colour from a hex literal.
fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

pub fn bg() -> Hsla {
    hex(0x0E0F11)
}
pub fn sidebar() -> Hsla {
    hex(0x121417)
}
pub fn surface() -> Hsla {
    hex(0x16181B)
}
pub fn raised() -> Hsla {
    hex(0x1D2024)
}
pub fn control() -> Hsla {
    hex(0x24272C)
}
pub fn border_strong() -> Hsla {
    hex(0x2A2E33)
}
pub fn text() -> Hsla {
    hex(0xE8EAED)
}
pub fn text_2() -> Hsla {
    hex(0xC9CDD2)
}
pub fn text_2_muted() -> Hsla {
    hex(0x9AA0A6)
}
pub fn text_3() -> Hsla {
    hex(0x8A9097)
}
pub fn accent() -> Hsla {
    hex(0x7C9CFF)
}
pub fn on_accent() -> Hsla {
    hex(0x0E0F11)
}
pub fn link() -> Hsla {
    hex(0x9DB4FF)
}
pub fn speaking() -> Hsla {
    hex(0x3DDC97)
}
pub fn ok_bg() -> Hsla {
    hex(0x14261D)
}
pub fn ok_text() -> Hsla {
    hex(0x6FE3AE)
}
pub fn warn_bg() -> Hsla {
    hex(0x2A2114)
}
pub fn warn_border() -> Hsla {
    hex(0x5A4520)
}
pub fn warn_text() -> Hsla {
    hex(0xF2C27A)
}
pub fn danger_bg() -> Hsla {
    hex(0x3A1F24)
}
pub fn danger_text() -> Hsla {
    hex(0xFF8A8A)
}

/// Muted avatar hues, picked by user id.
pub fn avatar(seed: u64) -> Hsla {
    const HUES: [u32; 8] = [
        0x3B4A6B, 0x4A3B5E, 0x2F5249, 0x5E4A32, 0x5A3640, 0x34505E, 0x4B4F2E, 0x463A5C,
    ];
    hex(HUES[(seed % HUES.len() as u64) as usize])
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

/// Points gpui-component's own widgets (buttons, inputs, switches, focus rings) at these tokens.
pub fn install(cx: &mut App) {
    let families = FAMILIES.get_or_init(|| pick_families(cx)).clone();
    Theme::change(ThemeMode::Dark, None, cx);
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
        c.button_danger_hover = hex(0x4A262C);
        c.button_danger_active = hex(0x4A262C);
        c.secondary = control();
        c.secondary_foreground = text();
        c.secondary_hover = border_strong();
        c.secondary_active = border_strong();
        c.danger = danger_text();
        c.warning = warn_text();
        c.success = ok_text();
    });
}
