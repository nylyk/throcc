use std::sync::Arc;

use gpui::{
    Div, FontWeight, InteractiveElement, ParentElement, Pixels, RenderImage, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, Size};

use super::common::{Typeset, audience, avatar, caption, icon};
use crate::theme;

const TOO_SLOW: &str = "Connection too slow";
const TOO_SLOW_HINT: &str = "Frames are dropping. Ask the sharer for a lower bitrate.";

/// Where a share is drawn, which sizes its overlay.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A tile in the room's grid, on its stage or in the strip under it.
    Tile,
    /// Fullscreen, or a window of its own.
    Player,
}

/// Sizes of one [`Place`]'s overlay.
struct Metrics {
    /// Distance of each label from the picture's edges.
    inset: Pixels,
    label_height: Pixels,
    avatar: Pixels,
    name: theme::TextStyle,
    button: Size,
    badge_icon: Pixels,
    badge_text: theme::TextStyle,
    details_min_width: Pixels,
    warning_text_min_width: Pixels,
}

impl Place {
    fn metrics(self) -> Metrics {
        match self {
            Place::Tile => Metrics {
                inset: px(8.),
                label_height: px(28.),
                avatar: px(18.),
                name: theme::CALLOUT,
                button: Size::Medium,
                badge_icon: px(14.),
                badge_text: theme::CAPTION,
                details_min_width: px(240.),
                warning_text_min_width: px(360.),
            },
            Place::Player => Metrics {
                inset: px(16.),
                label_height: px(32.),
                avatar: px(24.),
                name: theme::BODY,
                button: Size::Large,
                badge_icon: px(16.),
                badge_text: theme::CALLOUT,
                details_min_width: px(320.),
                warning_text_min_width: px(520.),
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
    /// How wide the share is drawn, which decides how much of each label fits.
    pub width: Pixels,
    /// Room kept clear left of the name, for the macOS traffic lights.
    pub title_inset: Pixels,
    pub buttons: Vec<Button>,
    /// How many people watch the share.
    pub audience: usize,
    /// Whether the name, audience and buttons show. The slow-connection warning shows regardless.
    pub controls: bool,
    pub too_slow: bool,
}

impl ShareOverlay {
    /// The overlay's labels, each placed absolutely in the element that holds the share.
    pub fn render(self) -> Vec<Div> {
        let m = self.place.metrics();
        let mut labels = Vec::new();
        if self.controls || self.too_slow {
            labels.push(
                div()
                    .absolute()
                    .top(m.inset)
                    .left(m.inset + self.title_inset)
                    .right(m.inset)
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.controls.then(|| self.title(&m)))
                    .children(self.too_slow.then(|| self.warning(&m))),
            );
        }
        if self.controls {
            labels.push(
                pill()
                    .absolute()
                    .bottom(m.inset)
                    .left(m.inset)
                    .px_2()
                    .h(m.label_height)
                    .typeset(m.badge_text)
                    .child(audience(self.audience, m.badge_icon)),
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
        let details = (self.width >= m.details_min_width && !self.details.is_empty())
            .then(|| caption(self.details.clone()).min_w_0().truncate());
        pill()
            .min_w_0()
            .gap_2()
            .px_2()
            .h(m.label_height)
            .child(avatar(&self.name, self.seed, self.picture.clone(), m.avatar, false).flex_none())
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
        let label = (self.width >= m.warning_text_min_width).then(|| {
            div()
                .typeset(m.badge_text)
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(TOO_SLOW)
        });
        pill()
            .id("too-slow")
            .ml_auto()
            .flex_none()
            .gap_1p5()
            .px(if label.is_some() { px(8.) } else { px(4.) })
            .h(m.label_height)
            .child(
                icon("triangle-alert")
                    .size(m.badge_icon)
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
