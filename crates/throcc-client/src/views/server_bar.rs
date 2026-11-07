use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::menu::ContextMenuExt;
use gpui_component::tooltip::Tooltip;

use super::common::{avatar, danger_menu_item, icon, moves_window, server_label, server_seed};
use crate::app::{Dialog, Screen, SettingsPage, ThroccApp};
use crate::theme;

/// The column of known servers along the window's left edge, under the traffic lights on
/// macOS, with buttons to add one and to open the settings.
/// The server bar, for the app's cached part.
pub fn part(app: &mut ThroccApp, _window: &mut Window, cx: &mut Context<ThroccApp>) -> AnyElement {
    render(app, cx).into_any_element()
}

fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let connected = app.connected_server().cloned();
    let connected_picture = app.picture(connected.as_ref().and_then(|s| s.avatar), cx);
    let servers =
        app.known_servers
            .iter()
            .enumerate()
            .map(|(i, (address, _))| {
                let here = connected.as_ref().is_some_and(|s| &s.address == address);
                let name = app.server_name(address);
                let picture = if here {
                    connected_picture.clone()
                } else {
                    app.server_looks
                        .get(address)
                        .and_then(|look| look.picture.clone())
                };
                let tooltip = server_label(name.as_deref(), address).into();
                let initials_from = name.unwrap_or_else(|| address.clone());
                let address = address.clone();
                let open = address.clone();
                let server = item(("server", i), tooltip, here)
                    .child(avatar(
                        &initials_from,
                        server_seed(&address),
                        picture,
                        px(36.),
                        false,
                    ))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.open_server(open.clone(), window, cx)
                    }));
                with_server_menu(server, address, here, cx)
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
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.show_screen(Screen::Settings(SettingsPage::Profile), window, cx)
                        })),
                ),
        )
}

/// `element` with a right-click menu to leave the server we are on, or to take any other
/// off the list.
fn with_server_menu(
    element: Stateful<Div>,
    address: String,
    here: bool,
    cx: &Context<ThroccApp>,
) -> AnyElement {
    let this = cx.entity().downgrade();
    let (label, icon_name) = if here {
        ("Leave Server", "log-out")
    } else {
        ("Remove from List", "trash")
    };
    element
        .context_menu(move |menu, _, _| {
            let this = this.clone();
            let address = address.clone();
            menu.item(
                danger_menu_item(label, icon_name).on_click(move |_, _, cx| {
                    let dialog = if here {
                        Dialog::LeaveServer
                    } else {
                        Dialog::RemoveServer(address.clone())
                    };
                    this.update(cx, |app, cx| app.set_dialog(Some(dialog), cx))
                        .ok();
                }),
            )
        })
        .into_any_element()
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
