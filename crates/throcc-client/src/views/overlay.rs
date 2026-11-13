use std::sync::Arc;

use gpui::{
    Div, FontWeight, InteractiveElement, ParentElement, Pixels, RenderImage, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, Size};

use throcc_client_core::state::Health;
use throcc_proto::ids::UserId;

use super::common::{Typeset, audience, avatar, caption, icon};
use crate::theme;

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

/// What the pill over a watched share warns of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareWarning {
    /// Our connection can't keep up with the share.
    SlowConnection,
    /// This device decodes the share too slowly.
    CantKeepUp,
}

impl ShareWarning {
    /// The warning over `user`'s share, if any, the connection's first.
    pub fn of(health: &Health, user: UserId) -> Option<Self> {
        if health.slow_shares.contains(&user) {
            Some(Self::SlowConnection)
        } else if health.lagging_shares.contains(&user) {
            Some(Self::CantKeepUp)
        } else {
            None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SlowConnection => "Slow connection",
            Self::CantKeepUp => "Can’t keep up",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::SlowConnection => "Frames are dropping. Ask the sharer for a lower bitrate.",
            Self::CantKeepUp => {
                "This device decodes the share too slowly. Ask the sharer for a lower resolution or frame rate."
            }
        }
    }
}

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
    /// Whether the name, audience and buttons show. The warning shows regardless.
    pub controls: bool,
    pub warning: Option<ShareWarning>,
    pub fullscreen: bool,
}

impl ShareOverlay {
    /// The overlay's labels, each placed absolutely in the element that holds the share.
    pub fn render(self) -> Vec<Div> {
        let inset = self.scaled(INSET);
        let mut labels = Vec::new();
        if self.controls || self.warning.is_some() {
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
                    .children(self.warning.map(|warning| self.warning_pill(warning))),
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

    fn warning_pill(&self, warning: ShareWarning) -> Stateful<Div> {
        let label = (self.width >= self.scaled(WARNING_TEXT_MIN_WIDTH)).then(|| {
            div()
                .typeset(self.text(BADGE_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(warning.label())
        });
        self.pill()
            .id("share-warning")
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
            .tooltip(move |window, cx| Tooltip::new(warning.hint()).build(window, cx))
    }
}
