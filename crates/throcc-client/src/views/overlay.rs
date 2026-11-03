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

/// Distance of each label from the picture's edges.
const INSET: Pixels = px(8.);
const LABEL_HEIGHT: Pixels = px(28.);
const AVATAR: Pixels = px(18.);
const NAME: theme::TextStyle = theme::CALLOUT;
const BUTTON: Size = Size::Medium;
const BADGE_ICON: Pixels = px(14.);
const BADGE_TEXT: theme::TextStyle = theme::CAPTION;
const DETAILS_MIN_WIDTH: Pixels = px(240.);
const WARNING_TEXT_MIN_WIDTH: Pixels = px(360.);

/// The labels and controls laid over a watched share, the same wherever it is drawn.
pub struct ShareOverlay {
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
        let mut labels = Vec::new();
        if self.controls || self.too_slow {
            labels.push(
                div()
                    .absolute()
                    .top(INSET)
                    .left(INSET + self.title_inset)
                    .right(INSET)
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.controls.then(|| self.title()))
                    .children(self.too_slow.then(|| self.warning())),
            );
        }
        if self.controls {
            labels.push(
                pill()
                    .absolute()
                    .bottom(INSET)
                    .left(INSET)
                    .px_2()
                    .h(LABEL_HEIGHT)
                    .typeset(BADGE_TEXT)
                    .child(audience(self.audience, BADGE_ICON)),
            );
        }
        if self.controls && !self.buttons.is_empty() {
            labels.push(
                pill()
                    .absolute()
                    .bottom(INSET)
                    .right(INSET)
                    .gap_1()
                    .p_1()
                    .children(
                        self.buttons
                            .into_iter()
                            .map(|button| button.with_size(BUTTON)),
                    ),
            );
        }
        labels
    }

    fn title(&self) -> Div {
        let details = (self.width >= DETAILS_MIN_WIDTH && !self.details.is_empty())
            .then(|| caption(self.details.clone()).min_w_0().truncate());
        pill()
            .min_w_0()
            .gap_2()
            .px_2()
            .h(LABEL_HEIGHT)
            .child(avatar(&self.name, self.seed, self.picture.clone(), AVATAR, false).flex_none())
            .child(
                div()
                    .min_w_0()
                    .typeset(NAME)
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(self.name.clone()),
            )
            .children(details)
    }

    fn warning(&self) -> Stateful<Div> {
        let label = (self.width >= WARNING_TEXT_MIN_WIDTH).then(|| {
            div()
                .typeset(BADGE_TEXT)
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
            .h(LABEL_HEIGHT)
            .child(
                icon("triangle-alert")
                    .size(BADGE_ICON)
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
