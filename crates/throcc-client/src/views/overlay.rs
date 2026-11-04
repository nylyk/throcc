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

const TOO_SLOW: &str = "Slow connection";
const TOO_SLOW_HINT: &str = "Frames are dropping. Ask the sharer for a lower bitrate.";

/// How much larger than its base sizes the overlay draws over a fullscreen share.
const FULLSCREEN_SCALE: f32 = 1.2;

/// Distance of each label from the picture's edges.
pub const INSET: Pixels = px(8.);
pub const LABEL_HEIGHT: Pixels = px(30.);
const AVATAR: Pixels = px(20.);
const NAME: theme::TextStyle = theme::BODY;
const BUTTON_PADDING: Pixels = px(3.);
const BUTTON_ICON: Pixels = px(15.);
const BADGE_ICON: Pixels = px(15.);
const BADGE_TEXT: theme::TextStyle = theme::CALLOUT;
const DETAILS_MIN_WIDTH: Pixels = px(260.);
const WARNING_TEXT_MIN_WIDTH: Pixels = px(390.);

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
    pub fullscreen: bool,
}

impl ShareOverlay {
    /// The overlay's labels, each placed absolutely in the element that holds the share.
    pub fn render(self) -> Vec<Div> {
        let inset = self.scaled(INSET);
        let mut labels = Vec::new();
        if self.controls || self.too_slow {
            labels.push(
                div()
                    .absolute()
                    .top(inset)
                    .left(inset + self.title_inset)
                    .right(inset)
                    .flex()
                    .items_center()
                    .gap(self.scaled(px(8.)))
                    .children(self.controls.then(|| self.title()))
                    .children(self.too_slow.then(|| self.warning())),
            );
        }
        if self.controls {
            labels.push(
                self.pill()
                    .absolute()
                    .bottom(inset)
                    .left(inset)
                    .px(self.scaled(px(8.)))
                    .h(self.scaled(LABEL_HEIGHT))
                    .typeset(self.text(BADGE_TEXT))
                    .child(audience(self.audience, self.scaled(BADGE_ICON))),
            );
        }
        if self.controls && !self.buttons.is_empty() {
            // gpui-component draws a custom-sized button's icon at 0.75 of that size.
            let icon_size = Size::Size(self.scaled(BUTTON_ICON) / 0.75);
            let button_size = self.scaled(LABEL_HEIGHT - BUTTON_PADDING * 2.);
            labels.push(
                self.pill()
                    .absolute()
                    .bottom(inset)
                    .right(inset)
                    .gap(self.scaled(px(4.)))
                    .p(self.scaled(BUTTON_PADDING))
                    .children(
                        self.buttons
                            .into_iter()
                            .map(|button| button.with_size(icon_size).size(button_size)),
                    ),
            );
        }
        labels
    }

    fn scaled(&self, length: Pixels) -> Pixels {
        if self.fullscreen {
            length * FULLSCREEN_SCALE
        } else {
            length
        }
    }

    fn text(&self, style: theme::TextStyle) -> theme::TextStyle {
        theme::TextStyle {
            size: self.scaled(style.size),
            line_height: self.scaled(style.line_height),
            ..style
        }
    }

    /// A translucent rounded box laid over the stream.
    fn pill(&self) -> Div {
        div()
            .flex()
            .items_center()
            .rounded(self.scaled(theme::RADIUS_ROW))
            .bg(theme::overlay())
            .text_color(theme::text())
    }

    fn title(&self) -> Div {
        let details = (self.width >= self.scaled(DETAILS_MIN_WIDTH) && !self.details.is_empty())
            .then(|| {
                caption(self.details.clone())
                    .typeset(self.text(BADGE_TEXT))
                    .min_w_0()
                    .truncate()
            });
        self.pill()
            .min_w_0()
            .gap(self.scaled(px(8.)))
            .px(self.scaled(px(8.)))
            .h(self.scaled(LABEL_HEIGHT))
            .child(
                avatar(
                    &self.name,
                    self.seed,
                    self.picture.clone(),
                    self.scaled(AVATAR),
                    false,
                )
                .flex_none(),
            )
            .child(
                div()
                    .min_w_0()
                    .typeset(self.text(NAME))
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(self.name.clone()),
            )
            .children(details)
    }

    fn warning(&self) -> Stateful<Div> {
        let label = (self.width >= self.scaled(WARNING_TEXT_MIN_WIDTH)).then(|| {
            div()
                .typeset(self.text(BADGE_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(TOO_SLOW)
        });
        self.pill()
            .id("too-slow")
            .ml_auto()
            .flex_none()
            .gap(self.scaled(px(6.)))
            .px(self.scaled(if label.is_some() { px(8.) } else { px(4.) }))
            .h(self.scaled(LABEL_HEIGHT))
            .child(
                icon("triangle-alert")
                    .size(self.scaled(BADGE_ICON))
                    .text_color(theme::warn_text()),
            )
            .children(label)
            .tooltip(|window, cx| Tooltip::new(TOO_SLOW_HINT).build(window, cx))
    }
}
