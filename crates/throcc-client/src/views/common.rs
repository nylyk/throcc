use std::sync::Arc;

use gpui::{
    Div, ElementId, FontWeight, Hsla, IntoElement, ObjectFit, ParentElement, Pixels, RenderImage,
    SharedString, Styled, StyledImage, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable, Size};

use crate::theme;

pub fn icon(name: &'static str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}

pub fn initials(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".into(), |c| c.to_uppercase().to_string())
}

/// The person's picture, or their initials on a muted hue, with the speaking ring when
/// they talk.
pub fn avatar(
    name: &str,
    seed: u64,
    picture: Option<Arc<RenderImage>>,
    size: Pixels,
    speaking: bool,
) -> Div {
    let ring = if speaking {
        theme::speaking()
    } else {
        gpui::transparent_black()
    };
    div()
        .flex_none()
        .size(size + px(4.))
        .rounded_full()
        .border_2()
        .border_color(ring)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .size(size)
                .rounded_full()
                .bg(theme::avatar(seed))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::text())
                .text_size(size * 0.42)
                .font_weight(FontWeight::MEDIUM)
                .overflow_hidden()
                .child(match picture {
                    Some(picture) => img(picture)
                        .size(size)
                        .rounded_full()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element(),
                    None => initials(name).into_any_element(),
                }),
        )
}

pub fn chip(text: impl Into<SharedString>, bg: Hsla, fg: Hsla) -> Div {
    div()
        .flex_none()
        .px_2()
        .py(px(2.))
        .rounded_full()
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

pub fn live_badge() -> Div {
    chip("LIVE", theme::danger_bg(), theme::danger_text())
}

pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(theme::text_3())
        .child(text.into())
}

pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(theme::mono_font()).child(text.into())
}

pub fn title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(20.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn section(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_3())
        .child(text.into())
}

pub fn card() -> Div {
    div()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::control())
        .rounded(theme::RADIUS_CARD)
        .p_5()
}

pub fn primary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id)
        .label(label.into())
        .primary()
        .rounded(theme::RADIUS_BUTTON)
        .with_size(Size::Medium)
}

pub fn secondary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id)
        .label(label.into())
        .secondary()
        .rounded(theme::RADIUS_BUTTON)
        .with_size(Size::Medium)
}

pub fn danger(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id)
        .label(label.into())
        .danger()
        .rounded(theme::RADIUS_BUTTON)
        .with_size(Size::Medium)
}

/// A square control-bar button: an icon, red when `on` is false.
pub fn control_button(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    tooltip: &'static str,
    on: bool,
) -> Button {
    let button = Button::new(id)
        .icon(icon(icon_name))
        .rounded(theme::RADIUS_BUTTON)
        .tooltip(tooltip)
        .w(theme::TOUCH)
        .h(theme::TOUCH);
    if on {
        button.secondary()
    } else {
        button.danger()
    }
}

pub fn ghost_icon(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    tooltip: &'static str,
) -> Button {
    Button::new(id)
        .icon(icon(icon_name))
        .ghost()
        .rounded(theme::RADIUS_ROW)
        .tooltip(tooltip)
}

/// "2c4e 9a1b": the first two groups of a fingerprint.
pub fn short_key(key: &[u8; 32]) -> String {
    throcc_proto::Fingerprint(*key).short()
}
