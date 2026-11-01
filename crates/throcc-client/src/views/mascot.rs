use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use gpui::{Styled, Svg, px, svg};

use crate::theme;

/// On the join screen before any server is known.
pub const WELCOMERS: &[&str] = &["mascot/cool.svg", "mascot/happy.svg", "mascot/love.svg"];

/// On the join screen once a server is known.
pub const GREETERS: &[&str] = &[
    "mascot/cool.svg",
    "mascot/happy.svg",
    "mascot/love.svg",
    "mascot/sing.svg",
    "mascot/thinking.svg",
];

/// In a lobby with no rooms.
pub const IDLE: &[&str] = &[
    "mascot/asleep.svg",
    "mascot/cool.svg",
    "mascot/happy.svg",
    "mascot/love.svg",
    "mascot/sing.svg",
    "mascot/thinking.svg",
];

/// In a room with no one else in it and nothing shared.
pub const LONELY: &[&str] = &[
    "mascot/asleep.svg",
    "mascot/sad.svg",
    "mascot/side-eye.svg",
    "mascot/sobbing.svg",
    "mascot/thinking.svg",
];

/// Beside a failed join or a lost connection.
pub const UPSET: &[&str] = &["mascot/angry.svg", "mascot/dizzy.svg", "mascot/glitch.svg"];

/// The mascot on screen, kept while its screen stays up and picked afresh when it shows again.
#[derive(Default)]
pub struct Shown {
    last_render: Option<(&'static [&'static str], &'static str)>,
    this_render: Option<(&'static [&'static str], &'static str)>,
}

impl Shown {
    pub fn start_render(&mut self) {
        self.last_render = self.this_render.take();
    }

    /// A mascot from `mascots`: the one from the last render if it was picked from them too.
    pub fn pick(&mut self, mascots: &'static [&'static str]) -> &'static str {
        let path = match self.last_render {
            Some((list, path)) if list == mascots => path,
            _ => random(mascots),
        };
        self.this_render = Some((mascots, path));
        path
    }
}

fn random(mascots: &[&'static str]) -> &'static str {
    let random = RandomState::new().build_hasher().finish();
    mascots[random as usize % mascots.len()]
}

/// The mascot drawing at `path`, large and in the muted colour.
pub fn drawing(path: &'static str) -> Svg {
    svg().path(path).size(px(180.)).text_color(theme::text_3())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mascot_stays_while_its_screen_stays_up() {
        let mut shown = Shown::default();
        shown.start_render();
        let lonely = shown.pick(LONELY);
        for _ in 0..20 {
            shown.start_render();
            assert_eq!(shown.pick(LONELY), lonely);
        }
        shown.start_render();
        assert!(UPSET.contains(&shown.pick(UPSET)));
    }

    #[test]
    fn a_mascot_is_picked_afresh_after_a_render_without_it() {
        let mut shown = Shown::default();
        shown.start_render();
        let first = shown.pick(LONELY);
        let changed = (0..50).any(|_| {
            shown.start_render();
            shown.start_render();
            shown.pick(LONELY) != first
        });
        assert!(changed);
    }
}
