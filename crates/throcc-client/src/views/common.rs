use std::sync::Arc;

use gpui::{
    AnyElement, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    ObjectFit, ParentElement, Pixels, RenderImage, SharedString, Stateful, Styled, StyledImage,
    TitlebarOptions, WindowBackgroundAppearance, WindowOptions, div, img, point, px,
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
        theme::transparent()
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
                .text_color(theme::on_avatar())
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

pub fn sharing_icon() -> Icon {
    icon("monitor")
        .size(px(14.))
        .text_color(theme::danger_text())
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

/// A key on the keyboard, as in a shortcut hint.
pub fn key_cap(text: impl Into<SharedString>) -> Div {
    mono(text)
        .flex_none()
        .px_1p5()
        .rounded(theme::RADIUS_ROW)
        .bg(theme::raised())
        .text_size(px(12.))
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

/// Room the macOS traffic lights take at the left of a window's top bar.
pub const TRAFFIC_LIGHTS: Pixels = px(if cfg!(target_os = "macos") { 76. } else { 0. });

/// `options` with the titlebar drawn by the app on macOS: the traffic lights sit in our top
/// bar at `lights_top`, and a `blurred` window shows the desktop behind see-through views.
pub fn app_titlebar(options: WindowOptions, lights_top: Pixels, blurred: bool) -> WindowOptions {
    if !cfg!(target_os = "macos") {
        return options;
    }
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), lights_top)),
        }),
        app_owns_titlebar_drag: true,
        window_background: if blurred {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        ..options
    }
}

/// Lets a top bar move the window, and zoom it on double-click, on macOS where the app
/// draws the titlebar. Presses a button inside took are left alone.
pub fn moves_window<E: InteractiveElement>(element: E) -> E {
    if !cfg!(target_os = "macos") {
        return element;
    }
    element.on_mouse_down(MouseButton::Left, |event, window, _| {
        if window.default_prevented() {
            return;
        }
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

/// How long the controls over a fullscreen stream stay after the pointer last moved.
pub const CONTROLS_LINGER: std::time::Duration = std::time::Duration::from_secs(3);

/// A stream as a video player shows it: the picture on black, `title` over a shade at the
/// top and `controls` over a shade at the bottom, both while `shown`.
pub fn player(picture: AnyElement, title: Div, controls: Div, shown: bool) -> Div {
    let edge = || {
        div()
            .absolute()
            .left_0()
            .right_0()
            .flex()
            .items_center()
            .gap_3()
            .px_6()
    };
    div()
        .size_full()
        .relative()
        .bg(theme::player_bg())
        .text_color(theme::player_text())
        .child(picture)
        .children(shown.then(|| {
            edge()
                .top_0()
                .h(px(96.))
                .pb_8()
                .bg(theme::player_shade(false))
                .child(title)
        }))
        .children(shown.then(|| {
            edge()
                .bottom_0()
                .h(px(112.))
                .pt_8()
                .justify_end()
                .bg(theme::player_shade(true))
                .child(controls)
        }))
}

/// The sharer's picture over "`name`'s screen" and `details`, for the top of a player.
pub fn player_title(
    name: &str,
    seed: u64,
    picture: Option<Arc<RenderImage>>,
    details: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(avatar(name, seed, picture, px(28.), false))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(16.))
                        .child(format!("{name}'s screen")),
                )
                .child(div().text_size(px(12.)).opacity(0.75).child(details.into())),
        )
}

/// A round icon button over a fullscreen stream.
pub fn player_button(id: impl Into<ElementId>, icon_name: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(44.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(theme::player_hover()))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .child(
            icon(icon_name)
                .size(px(22.))
                .text_color(theme::player_text()),
        )
}
