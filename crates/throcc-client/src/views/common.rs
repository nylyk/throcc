use std::sync::Arc;

use gpui::{
    Context, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    ObjectFit, ParentElement, Pixels, Point, RenderImage, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledImage, Task, TitlebarOptions,
    WindowBackgroundAppearance, WindowOptions, div, img, px, relative,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::slider::{SliderEvent, SliderState};
use gpui_component::{Icon, Sizable, Size};
use throcc_client_core::state::{MAX_VOLUME, Person, State, VerifiedIdentity};
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;

use crate::theme;
use crate::updates::Release;

/// Sets text in a step of the type scale.
pub trait Typeset: Styled + Sized {
    fn typeset(self, style: theme::TextStyle) -> Self {
        self.text_size(style.size)
            .line_height(style.line_height)
            .font_weight(style.weight)
    }
}

impl<E: Styled> Typeset for E {}

/// Whether the identity key the server shows for `user` replaced one the user verified.
pub fn key_changed(state: &State, user: UserId) -> bool {
    state.key_warnings.iter().any(|w| w.user == user)
}

/// The name `person` was verified under, where the server now shows another: the server
/// picks the name, so it alone doesn't say who is verified.
pub fn verified_as(verified: &[VerifiedIdentity], person: &Person) -> Option<String> {
    let key = Fingerprint(person.identity_key.filter(|_| person.verified)?);
    let stored = &verified.iter().find(|v| v.key == key)?.name;
    (*stored != person.name).then(|| stored.clone())
}

/// What we know of the identity key the server shows for someone.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// It replaced a key the user verified.
    KeyChanged,
    Unverified,
    /// Verified, or our own.
    Verified,
}

pub fn trust(state: &State, user: UserId) -> Trust {
    if key_changed(state, user) {
        Trust::KeyChanged
    } else if state.me == Some(user) || state.person(user).is_some_and(|p| p.verified) {
        Trust::Verified
    } else {
        Trust::Unverified
    }
}

pub fn icon(name: &'static str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}

const QUIETEST_DB: f32 = -36.0;

/// The gain for a volume slider at `loudness`, from 0 (silent) to 2 where 1 plays the sound
/// as it arrives. Each half rises evenly in decibels: from -36 dB to unity, then to +12 dB.
pub fn loudness_to_gain(loudness: f32) -> f32 {
    let loudness = loudness.clamp(0.0, 2.0);
    if loudness == 0.0 {
        0.0
    } else if loudness <= 1.0 {
        10f32.powf(QUIETEST_DB * (1.0 - loudness) / 20.0)
    } else {
        MAX_VOLUME.powf(loudness - 1.0)
    }
}

/// Where a volume slider sits for `gain`, the inverse of [`loudness_to_gain`].
pub fn gain_to_loudness(gain: f32) -> f32 {
    let gain = gain.clamp(0.0, MAX_VOLUME);
    if gain == 0.0 {
        0.0
    } else if gain <= 1.0 {
        (1.0 - 20.0 * gain.log10() / QUIETEST_DB).max(0.0)
    } else {
        1.0 + gain.ln() / MAX_VOLUME.ln()
    }
}

/// A 0 to 200 % volume slider, set to `gain`.
pub fn volume_slider(gain: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(200.)
        .step(1.)
        .default_value(gain_to_loudness(gain) * 100.)
}

/// Where a slider was moved to.
pub fn slider_value(event: &SliderEvent) -> f32 {
    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
    value.start()
}

/// The gain a [`volume_slider`] was moved to.
pub fn slider_gain(event: &SliderEvent) -> f32 {
    loudness_to_gain(slider_value(event) / 100.)
}

/// A server as "name (address)", or its address alone when it has no name.
pub fn server_label(name: Option<&str>, address: &str) -> String {
    match name {
        Some(name) => format!("{name} ({address})"),
        None => address.to_owned(),
    }
}

/// A bitrate in kbps, in Mbps from 1000 up.
pub fn rate(kbps: u32) -> String {
    if kbps >= 1000 {
        format!("{:.1} Mbps", kbps as f32 / 1000.0)
    } else {
        format!("{kbps} kbps")
    }
}

pub fn initials(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".into(), |c| c.to_uppercase().to_string())
}

/// An avatar colour seed for the server at `address`, the same wherever it is drawn.
pub fn server_seed(address: &str) -> u64 {
    address.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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
                .line_height(relative(1.))
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
        .typeset(theme::CAPTION)
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

pub fn sharing_icon(size: Pixels) -> Icon {
    icon("monitor").size(size).text_color(theme::danger_text())
}

pub fn audience(count: usize, icon_size: Pixels) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            icon("user")
                .size(icon_size)
                .text_color(theme::danger_text()),
        )
        .child(count.to_string())
}

pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .typeset(theme::CAPTION)
        .text_color(theme::text_3())
        .child(text.into())
}

/// A quiet link to a newer release's page.
pub fn update_notice(release: &Release) -> Stateful<Div> {
    let url = release.url.clone();
    div()
        .id("update-notice")
        .flex()
        .items_center()
        .gap_2()
        .min_h(px(28.))
        .cursor_pointer()
        .typeset(theme::CALLOUT)
        .text_color(theme::link())
        .hover(|s| s.underline())
        .child(icon("circle-arrow-up").size(px(16.)))
        .child(format!("throcc {} is available", release.version))
        .on_click(move |_, _, cx| cx.open_url(&url))
}

/// The update notice as a notification that stays until dismissed.
pub fn update_notification(release: &Release) -> Notification {
    struct UpdateNotification;
    let release = release.clone();
    Notification::new()
        .id::<UpdateNotification>()
        .content(move |_, _, _| update_notice(&release).into_any_element())
        .action(|_, _, cx| {
            secondary("dismiss-update", "Dismiss").on_click(
                cx.listener(|notification, _, window, cx| notification.dismiss(window, cx)),
            )
        })
}

/// Explanatory text in a dialog or card.
pub fn description(text: impl Into<SharedString>) -> Div {
    div()
        .typeset(theme::CALLOUT)
        .text_color(theme::text_2())
        .child(text.into())
}

pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(theme::mono_font()).child(text.into())
}

/// A key on the keyboard, as in a shortcut hint.
pub fn key_cap(text: impl Into<SharedString>) -> Div {
    div()
        .child(text.into())
        .flex_none()
        .px_1p5()
        .rounded(theme::RADIUS_ROW)
        .bg(theme::raised())
        .typeset(theme::CAPTION)
}

pub fn title(text: impl Into<SharedString>) -> Div {
    div().typeset(theme::TITLE_1).child(text.into())
}

pub fn section(text: impl Into<SharedString>) -> Div {
    div()
        .typeset(theme::CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
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

/// A ghost icon that turns red, showing `active_icon`, while `active`.
pub fn toggle_icon(
    id: &'static str,
    icon_name: &'static str,
    active_icon: &'static str,
    active: bool,
    tooltip: &'static str,
) -> Button {
    let shown = if active { active_icon } else { icon_name };
    let shown = icon(shown).size(px(16.));
    Button::new(id)
        .child(if active {
            shown.text_color(theme::danger_text())
        } else {
            shown
        })
        .ghost()
        .size_8()
        .px_0()
        .rounded(theme::RADIUS_ROW)
        .tooltip(tooltip)
}

/// A menu item in the danger colours, for leaving or removing something.
pub fn danger_menu_item(label: &'static str, icon_name: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| div().text_color(theme::danger_text()).child(label))
        .icon(icon(icon_name).text_color(theme::danger_text()))
}

/// Why what was typed in the field above can't be used.
pub fn field_error(error: impl Into<SharedString>) -> Div {
    div()
        .typeset(theme::CALLOUT)
        .text_color(theme::danger_text())
        .child(error.into())
}

/// What went wrong, in the warning colours.
pub fn error_box(error: String) -> Div {
    div()
        .p_3()
        .rounded(theme::RADIUS_ROW)
        .bg(theme::warn_bg())
        .border_1()
        .border_color(theme::warn_border())
        .text_color(theme::warn_text())
        .typeset(theme::CALLOUT)
        .child(error)
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

/// `options` with the app id, and the app's own titlebar on macOS with the traffic lights in
/// our views at `lights`.
/// A `blurred` window shows the desktop behind see-through views on macOS and Linux.
pub fn app_titlebar(options: WindowOptions, lights: Point<Pixels>, blurred: bool) -> WindowOptions {
    let options = WindowOptions {
        window_background: if blurred && theme::SEES_DESKTOP {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        app_id: Some(crate::APP_ID.to_owned()),
        ..options
    };
    if !cfg!(target_os = "macos") {
        return options;
    }
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(lights),
        }),
        app_owns_titlebar_drag: true,
        ..options
    }
}

/// A bar across the top of a column at the header height, which moves the window.
pub fn top_bar() -> Div {
    moves_window(div())
        .flex()
        .items_center()
        .h(theme::HEADER_HEIGHT)
        .flex_none()
        .border_b_1()
        .border_color(theme::control())
}

/// A bar across the bottom of a column at the footer height.
pub fn bottom_bar() -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .h(theme::FOOTER_HEIGHT)
        .flex_none()
        .border_t_1()
        .border_color(theme::control())
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
pub const CONTROLS_LINGER: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a tile's overlay stays after the pointer leaves the tile.
pub const OVERLAY_LINGER: std::time::Duration = std::time::Duration::from_secs(1);

pub fn notify_after<T: 'static>(delay: std::time::Duration, cx: &mut Context<T>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(delay).await;
        this.update(cx, |_, cx| cx.notify()).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_middle_of_a_volume_slider_plays_as_it_arrives() {
        assert_eq!(loudness_to_gain(1.0), 1.0);
        assert_eq!(loudness_to_gain(0.0), 0.0);
        assert_eq!(loudness_to_gain(2.0), MAX_VOLUME);
        assert!((loudness_to_gain(0.5) - 10f32.powf(-18.0 / 20.0)).abs() < 1e-6);
    }

    #[test]
    fn gain_and_loudness_round_trip() {
        for step in 0..=200 {
            let loudness = step as f32 / 100.0;
            let back = gain_to_loudness(loudness_to_gain(loudness));
            assert!(
                (back - loudness).abs() < 1e-4,
                "{loudness} came back as {back}"
            );
        }
    }
}
