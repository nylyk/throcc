use gpui::{Context, Div, IntoElement, ParentElement, Styled, div, px};
use throcc_client_core::client::UiCommand;
use throcc_client_core::state::NoticeAction;

use super::common::{description, error_box, moves_window, primary};
use super::mascot;
use crate::app::{Dialog, ThroccApp};
use crate::theme;

/// The window with no server connected: how a join is going, or what to do next.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let first_run = app.known_servers.is_empty();
    let error = app
        .join_error
        .clone()
        .filter(|_| app.dialog != Some(Dialog::AddServer));
    let mascot = app.mascot.pick(if error.is_some() {
        mascot::UPSET
    } else if first_run {
        mascot::WELCOMERS
    } else {
        mascot::GREETERS
    });
    let hint = if app.joining {
        Some("Joining…")
    } else if first_run {
        Some("Add your first server to get started.")
    } else if error.is_some() {
        Some("Try again, or pick another server on the left.")
    } else {
        None
    };
    frame(mascot)
        .children(hint.map(description))
        .children(error.map(|e| div().w(px(420.)).child(error_box(e))))
        .children((first_run && !app.joining).then(|| {
            primary("add-first-server", "Add Server")
                .h_10()
                .px_4()
                .on_click(cx.listener(|app, _, window, cx| app.open_add_server(window, cx)))
        }))
}

/// The window in place of the room while the connection is lost, or `None` while it holds.
pub fn render_lost(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> Option<Div> {
    let notice = app.state.connection.notice()?;
    let awaiting = notice.action == Some(NoticeAction::Reconnect);
    Some(
        frame(app.mascot.pick(mascot::UPSET))
            .child(description(notice.hint))
            .children(
                notice
                    .problem
                    .map(|e| div().w(px(420.)).child(error_box(e))),
            )
            .children(awaiting.then(|| {
                primary("reconnect", "Reconnect")
                    .h_10()
                    .px_4()
                    .on_click(cx.listener(|app, _, _, _| app.command(UiCommand::Reconnect)))
            })),
    )
}

/// A window-filling column centred on `mascot`, for screens with no server connected.
pub fn frame(mascot: &'static str) -> Div {
    div()
        .size_full()
        .relative()
        .bg(theme::bg())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .child(
            moves_window(div())
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(theme::HEADER_HEIGHT),
        )
        .child(mascot::drawing(mascot))
}
