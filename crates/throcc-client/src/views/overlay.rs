use std::sync::Arc;

use gpui::{
    Div, FontWeight, InteractiveElement, ParentElement, Pixels, RenderImage, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, Size};

use super::common::{Typeset, avatar, caption, icon};
use crate::theme;

const TOO_SLOW: &str = "Connection too slow";
const TOO_SLOW_HINT: &str = "Frames are dropping. Ask the sharer for a lower bitrate.";

/// Where a share is drawn, which sizes its overlay.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A small tile in the strip under a focused share.
    Strip,
    /// A tile in the room's grid, or the focused share on its stage.
    Tile,
    /// Fullscreen, or a window of its own.
    Player,
}

/// Sizes of one [`Place`]'s overlay.
struct Metrics {
    /// Distance of each label from the picture's edges.
    inset: Pixels,
    /// Height of the name and warning labels.
    label: Pixels,
    avatar: Pixels,
    name: theme::TextStyle,
    button: Size,
    warning_icon: Pixels,
}

impl Place {
    fn metrics(self) -> Metrics {
        match self {
            Place::Strip | Place::Tile => Metrics {
                inset: px(8.),
                label: px(28.),
                avatar: px(18.),
                name: theme::CALLOUT,
                button: Size::Medium,
                warning_icon: px(14.),
            },
            Place::Player => Metrics {
                inset: px(16.),
                label: px(32.),
                avatar: px(24.),
                name: theme::BODY,
                button: Size::Large,
                warning_icon: px(16.),
            },
        }
    }
}

/// The labels and controls laid over a watched share, the same wherever it is drawn.
pub struct ShareOverlay {
    pub place: Place,
    pub name: SharedString,
    pub seed: u64,
    pub picture: Option<Arc<RenderImage>>,
    pub details: SharedString,
    /// Room kept clear left of the name, for the macOS traffic lights.
    pub title_inset: Pixels,
    pub buttons: Vec<Button>,
    /// Whether the name and buttons show. The slow-connection warning shows regardless.
    pub controls: bool,
    pub too_slow: bool,
}

impl ShareOverlay {
    /// The overlay's labels, each placed absolutely in the element that holds the share.
    pub fn render(self) -> Vec<Div> {
        let m = self.place.metrics();
        let mut labels = Vec::new();
        if self.controls {
            labels.push(self.title(&m));
        }
        if self.too_slow {
            labels.push(
                div()
                    .absolute()
                    .top(m.inset)
                    .right(m.inset)
                    .child(self.warning(&m)),
            );
        }
        if self.controls && !self.buttons.is_empty() {
            labels.push(
                pill()
                    .absolute()
                    .bottom(m.inset)
                    .right(m.inset)
                    .gap_1()
                    .p_1()
                    .children(
                        self.buttons
                            .into_iter()
                            .map(|button| button.with_size(m.button)),
                    ),
            );
        }
        labels
    }

    fn title(&self, m: &Metrics) -> Div {
        let details = (self.place != Place::Strip && !self.details.is_empty())
            .then(|| caption(self.details.clone()).flex_none());
        pill()
            .absolute()
            .top(m.inset)
            .left(m.inset + self.title_inset)
            .max_w(relative(if self.too_slow { 0.6 } else { 0.9 }))
            .gap_2()
            .px_2()
            .h(m.label)
            .child(avatar(
                &self.name,
                self.seed,
                self.picture.clone(),
                m.avatar,
                false,
            ))
            .child(
                div()
                    .min_w_0()
                    .typeset(m.name)
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(self.name.clone()),
            )
            .children(details)
    }

    fn warning(&self, m: &Metrics) -> Stateful<Div> {
        let label = (self.place != Place::Strip).then(|| {
            div()
                .typeset(if self.place == Place::Player {
                    theme::CALLOUT
                } else {
                    theme::CAPTION
                })
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(TOO_SLOW)
        });
        pill()
            .id("too-slow")
            .gap_1p5()
            .px(if label.is_some() { px(8.) } else { px(4.) })
            .h(m.label)
            .child(
                icon("triangle-alert")
                    .size(m.warning_icon)
                    .text_color(theme::warn_text()),
            )
            .children(label)
            .tooltip(|window, cx| Tooltip::new(TOO_SLOW_HINT).build(window, cx))
    }
}

/// A translucent rounded box laid over a stream.
fn pill() -> Div {
    div()
        .flex()
        .items_center()
        .rounded(theme::RADIUS_ROW)
        .bg(theme::overlay())
        .text_color(theme::text())
}
