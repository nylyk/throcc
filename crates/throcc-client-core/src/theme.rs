use crate::settings::Appearance;

/// The preset themes by name, each as its eight colours in the order
/// [`Appearance::palette`] keeps them: background, panel, text, muted text, accent,
/// warning, good and bad. The first is the default.
pub const PRESETS: [(&str, [u32; 8]); 7] = [
    (
        "Ink",
        [
            0x0C0D11, 0x14161D, 0xECEDF3, 0x8E92A6, 0xD07A93, 0xFFB86B, 0x5BE3A4, 0xFF6B85,
        ],
    ),
    (
        "Moss",
        [
            0x0F1411, 0x161D18, 0xE8EFE9, 0x8C9A90, 0x7FD1A8, 0xE8C468, 0x8BE07D, 0xF0786E,
        ],
    ),
    (
        "Dusk",
        [
            0x15101A, 0x1E1724, 0xF1EAF4, 0x9C8FA6, 0xFF8FB8, 0xFFC285, 0x7FE0C4, 0xFF6F6F,
        ],
    ),
    (
        "Tide",
        [
            0x0B1218, 0x111B23, 0xE6EEF3, 0x8798A6, 0x5CC8E8, 0xF2B55E, 0x5FD9A0, 0xF2727F,
        ],
    ),
    (
        "Ember",
        [
            0x14100D, 0x1D1814, 0xF2ECE6, 0xA0958A, 0xFF9A5C, 0xF5D06B, 0x8FD88A, 0xFF6B6B,
        ],
    ),
    (
        "Paper",
        [
            0xF2F4F7, 0xFFFFFF, 0x1C1B22, 0x6B6878, 0x5B4BDB, 0xD9822B, 0x1F9D6A, 0xD9445E,
        ],
    ),
    (
        "Sand",
        [
            0xF3EEDF, 0xFCF9EE, 0x24201A, 0x756C5F, 0x1E7F86, 0xC07A1F, 0x4A8B3A, 0xC8414F,
        ],
    ),
];
/// The name [`Appearance::theme`] has while [`Appearance::palette`] holds the colours.
pub const CUSTOM: &str = "Custom";

/// The name and colours of the theme `appearance` picks, the first preset where it picks
/// none that exists.
pub fn picked(appearance: &Appearance) -> (&'static str, [u32; 8]) {
    let picked = match appearance.theme.as_deref() {
        Some(CUSTOM) => saved_custom(appearance).map(|colors| (CUSTOM, colors)),
        Some(name) => preset(name),
        None => None,
    };
    picked.unwrap_or(PRESETS[0])
}

/// The preset called `name`, ignoring case.
pub fn preset(name: &str) -> Option<(&'static str, [u32; 8])> {
    PRESETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .copied()
}

/// The Custom theme's colours in `appearance`, or the picked theme's until any are saved.
pub fn custom(appearance: &Appearance) -> [u32; 8] {
    saved_custom(appearance).unwrap_or_else(|| picked(appearance).1)
}

/// Picks the theme called `name`, a preset or [`CUSTOM`], keeping the Custom theme's colours
/// as they stand: picking a preset never loses them, and Custom starts from them.
pub fn pick(appearance: &mut Appearance, name: &str) {
    let colors = to_hex(custom(appearance));
    appearance.theme = Some(name.to_owned());
    appearance.palette = Some(colors);
}

/// Picks the Custom theme with `colors` as its colours.
pub fn pick_custom(appearance: &mut Appearance, colors: [u32; 8]) {
    appearance.theme = Some(CUSTOM.to_owned());
    appearance.palette = Some(to_hex(colors.map(|c| c & 0xFF_FFFF)));
}

fn saved_custom(appearance: &Appearance) -> Option<[u32; 8]> {
    from_hex(appearance.palette.as_ref()?)
}

/// The eight colours as `#RRGGBB`, as [`Appearance::palette`] keeps them.
pub fn to_hex(colors: [u32; 8]) -> Vec<String> {
    colors.iter().map(|c| format_hex(*c)).collect()
}

pub fn from_hex(colors: &[String]) -> Option<[u32; 8]> {
    let colors: Vec<u32> = colors.iter().map(|c| parse_hex(c)).collect::<Option<_>>()?;
    colors.try_into().ok()
}

/// `#rrggbb`, `rrggbb` or `#rgb` as a colour.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.trim().trim_start_matches('#');
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
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

/// Whether a theme on the background `bg`, as 0xRRGGBB, is a light one.
pub fn is_light(bg: u32) -> bool {
    oklab(bg)[0] > 0.6
}

/// `color`, as 0xRRGGBB, in Oklab.
pub fn oklab(color: u32) -> [f32; 3] {
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

/// `color` from Oklab, as 0xRRGGBB, clamped into sRGB.
pub fn from_oklab([l, a, b]: [f32; 3]) -> u32 {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let b = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    (linear_to_srgb(r) << 16) | (linear_to_srgb(g) << 8) | linear_to_srgb(b)
}

/// `share` of `top` over `base`, mixed in Oklab like CSS `color-mix(in oklab, …)`.
pub fn mix(top: u32, share: f32, base: u32) -> u32 {
    let (t, b) = (oklab(top), oklab(base));
    from_oklab(std::array::from_fn(|i| t[i] * share + b[i] * (1.0 - share)))
}

/// The WCAG contrast ratio of two colours, from 1 to 21.
pub fn contrast(one: u32, other: u32) -> f32 {
    let (a, b) = (luminance(one), luminance(other));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The contrast text needs to read on a surface, as WCAG AA asks of body text.
pub const READABLE: f32 = 4.5;

/// `color` mixed toward `text` in steps of a tenth until it reads on every one of
/// `surfaces`, or `text` itself if no step does.
pub fn readable(color: u32, text: u32, surfaces: &[u32]) -> u32 {
    (0..=10)
        .map(|step| mix(text, step as f32 / 10.0, color))
        .find(|mixed| surfaces.iter().all(|s| contrast(*mixed, *s) >= READABLE))
        .unwrap_or(text)
}

/// The colours every client mixes from a theme's eight, the same on each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub bg: u32,
    pub panel: u32,
    /// A row or control lifted off the panel.
    pub raised: u32,
    /// A control's fill, a step above `raised`.
    pub control: u32,
    /// Lines and borders.
    pub line: u32,
    /// What is picked.
    pub selected: u32,
    /// Behind a warning about something gone wrong.
    pub danger: u32,
    pub text: u32,
    /// These read as text on every surface above, at [`READABLE`] contrast.
    pub muted: u32,
    pub accent: u32,
    pub warning: u32,
    pub good: u32,
    pub bad: u32,
}

impl Colors {
    /// The colours of a theme given as its eight, in [`PRESETS`] order.
    pub fn of([bg, panel, text, muted, accent, warning, good, bad]: [u32; 8]) -> Self {
        let raised = mix(text, 0.06, panel);
        let control = mix(text, 0.10, panel);
        let selected = mix(accent, 0.18, panel);
        let surfaces = [bg, panel, raised, control, selected];
        let readable = |color| readable(color, text, &surfaces);
        Self {
            bg,
            panel,
            raised,
            control,
            line: mix(text, 0.16, panel),
            selected,
            danger: mix(bad, 0.18, panel),
            text,
            muted: readable(muted),
            accent: readable(accent),
            warning: readable(warning),
            good: readable(good),
            bad: readable(bad),
        }
    }
}

/// Relative luminance, as WCAG defines it.
fn luminance(color: u32) -> f32 {
    let [r, g, b] = [color >> 16, color >> 8, color].map(|c| srgb_to_linear(c & 0xFF));
    0.2126 * r + 0.7152 * g + 0.0722 * b
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

fn srgb_to_linear(channel: u32) -> f32 {
    let c = channel as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_survive_their_own_hex() {
        for (_, colors) in PRESETS {
            assert_eq!(from_hex(&to_hex(colors)), Some(colors));
        }
        assert_eq!(from_hex(&["#000000".into(), "#FFFFFF".into()]), None);
    }

    #[test]
    fn the_picked_theme_is_found_by_name() {
        let colors = PRESETS[2].1;
        let mut appearance = Appearance {
            theme: Some("Paper".into()),
            palette: Some(to_hex(colors)),
            ..Appearance::default()
        };
        let paper = PRESETS.iter().find(|(name, _)| *name == "Paper").copied();
        assert_eq!(Some(picked(&appearance)), paper);
        assert_eq!(custom(&appearance), colors);
        appearance.theme = Some(CUSTOM.into());
        assert_eq!(picked(&appearance), (CUSTOM, colors));
        appearance.theme = None;
        assert_eq!(picked(&appearance), PRESETS[0]);
        appearance.theme = Some("Gone".into());
        assert_eq!(picked(&appearance), PRESETS[0]);
        appearance.palette = None;
        assert_eq!(custom(&appearance), PRESETS[0].1);
        appearance.theme = Some(CUSTOM.into());
        assert_eq!(picked(&appearance), PRESETS[0]);
    }

    #[test]
    fn hex_accepts_short_and_bare_forms() {
        assert_eq!(parse_hex("#8C7BFF"), Some(0x8C7BFF));
        assert_eq!(parse_hex("8c7bff"), Some(0x8C7BFF));
        assert_eq!(parse_hex("#fa0"), Some(0xFFAA00));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
        assert_eq!(parse_hex("#+FFFFF"), None);
    }

    #[test]
    fn mixing_keeps_the_ends_and_meets_in_the_middle() {
        assert_eq!(mix(0xFF0000, 1.0, 0x0000FF), 0xFF0000);
        assert_eq!(mix(0xFF0000, 0.0, 0x0000FF), 0x0000FF);
        assert_eq!(mix(0x808080, 0.5, 0x808080), 0x808080);
        assert!((contrast(0x000000, 0xFFFFFF) - 21.0).abs() < 0.01);
    }

    #[test]
    fn every_text_colour_reads_on_every_surface() {
        let dim = [
            0x0C0D11, 0x14161D, 0xECEDF3, 0x2A2C35, 0x30333D, 0x2A2C35, 0x203020, 0x302020,
        ];
        for (name, colors) in PRESETS.into_iter().chain([("dim", dim)]) {
            let c = Colors::of(colors);
            for color in [c.muted, c.accent, c.warning, c.good, c.bad] {
                for surface in [c.bg, c.panel, c.raised, c.control, c.selected] {
                    assert!(
                        contrast(color, surface) >= READABLE,
                        "{name}: {color:06X} on {surface:06X}"
                    );
                }
            }
        }
        assert_ne!(Colors::of(dim).muted, dim[3]);
    }

    #[test]
    fn only_paper_and_sand_are_light() {
        let light: Vec<&str> = PRESETS
            .iter()
            .filter(|(_, colors)| is_light(colors[0]))
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(light, ["Paper", "Sand"]);
    }
}
