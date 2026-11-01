use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use gpui::{Styled, Svg, px, svg};

use crate::theme;

pub const HAPPY: &str = "mascot/happy.svg";

pub const GREETERS: &[&str] = &[
    "mascot/cool.svg",
    HAPPY,
    "mascot/love.svg",
    "mascot/sing.svg",
    "mascot/thinking.svg",
];

pub const IDLE: &[&str] = &[
    "mascot/asleep.svg",
    "mascot/cool.svg",
    HAPPY,
    "mascot/love.svg",
    "mascot/sing.svg",
    "mascot/thinking.svg",
];

pub const UPSET: &[&str] = &["mascot/angry.svg", "mascot/dizzy.svg", "mascot/glitch.svg"];

pub fn pick(mascots: &[&'static str]) -> &'static str {
    let random = RandomState::new().build_hasher().finish();
    mascots[random as usize % mascots.len()]
}

/// The mascot drawing at `path`, large and in the muted colour.
pub fn drawing(path: &'static str) -> Svg {
    svg().path(path).size(px(180.)).text_color(theme::text_3())
}
