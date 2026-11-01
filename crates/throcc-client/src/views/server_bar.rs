use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Div, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::tooltip::Tooltip;

use super::common::{avatar, icon, moves_window};
use crate::app::{Screen, SettingsPage, ThroccApp};
use crate::theme;

/// The column of known servers along the window's left edge, under the traffic lights on
/// macOS, with buttons to add one and to open the settings.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let connected = app.connected_server().cloned();
    let connected_picture = app.picture(connected.as_ref().and_then(|s| s.avatar), cx);
    let servers = app
        .known_servers
        .iter()
        .enumerate()
        .map(|(i, (address, _))| {
            let here = connected.as_ref().filter(|s| &s.address == address);
            let name: SharedString = here
                .map_or(address.as_str(), |s| s.title())
                .to_owned()
                .into();
            let picture = here.and(connected_picture.clone());
            let address = address.clone();
            item(("server", i), name.clone(), here.is_some())
                .child(avatar(&name, i as u64, picture, px(36.), false))
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.open_server(address.clone(), window, cx)
                }))
        });
    let in_settings = matches!(app.screen, Screen::Settings(_));
    let top = if cfg!(target_os = "macos") {
        theme::HEADER_HEIGHT
    } else {
        px(12.)
    };
    div()
        .w(theme::SERVER_BAR_WIDTH)
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(theme::sidebar_glass())
        .border_r_1()
        .border_color(theme::control())
        .child(moves_window(div()).h(top).flex_none())
        .child(
            div()
                .id("servers")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .pb_3()
                .children(servers)
                .child(
                    item("add-server", "Add server".into(), false)
                        .text_color(theme::text_2())
                        .child(icon("plus").size(px(20.)))
                        .on_click(
                            cx.listener(|app, _, window, cx| app.open_add_server(window, cx)),
                        ),
                ),
        )
        .child(
            div()
                .h(theme::FOOTER_HEIGHT)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    item("settings", "Settings".into(), in_settings)
                        .text_color(theme::text_2())
                        .child(icon("settings").size(px(20.)))
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.screen = Screen::Settings(SettingsPage::Audio);
                            cx.notify();
                        })),
                ),
        )
}

fn item(id: impl Into<ElementId>, tooltip: SharedString, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(56.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_TILE)
        .cursor_pointer()
        .map(|s| {
            if selected {
                s.bg(theme::selected())
            } else {
                s.hover(|s| s.bg(theme::raised()))
            }
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}
