use gpui::{Context, Div, IntoElement, ParentElement, Styled, div, px};
use throcc_client_core::account::SignInError;
use throcc_client_core::connection::ConnectError;
use throcc_client_core::control::LoginError;
use throcc_client_core::state::ConnectionStatus;
use throcc_proto::messages::AuthError;

use super::common::{Typeset, description, moves_window, primary};
use super::mascot;
use crate::app::{Dialog, ThroccApp};
use crate::theme;

/// Turns a failed sign-in into a sentence for the join screen.
pub fn explain(error: &SignInError) -> String {
    match error {
        SignInError::NotEnrolled(address) => {
            format!("You haven’t joined {address} yet. Ask someone there for an invite code.")
        }
        SignInError::Login(LoginError::Refused(AuthError::BadInvite)) | SignInError::Login(LoginError::Pake(_)) => {
            "That code didn’t work. It may be mistyped, used up or expired, or something between you and the server isn’t the server. Ask for a fresh one.".into()
        }
        SignInError::Login(LoginError::Refused(AuthError::RateLimited)) => {
            "Too many wrong codes from your network. Wait ten minutes, then try again.".into()
        }
        SignInError::Login(LoginError::Refused(AuthError::UnknownKey)) => {
            "This server doesn’t know you anymore. Join again with a fresh invite code.".into()
        }
        SignInError::Connect(ConnectError::Resolve(address)) => format!("Couldn’t find {address}. Check the address."),
        SignInError::Connect(ConnectError::Connection(_)) => {
            "Couldn’t reach the server. Check the address, and that UDP port 8476 is open.".into()
        }
        other => other.to_string(),
    }
}

/// A failed join, in the warning colours.
pub(super) fn error_box(error: String) -> Div {
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

/// The window with no server connected: how a join is going, or what to do next.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let first_run = app.known_servers.is_empty();
    let error = app
        .join_error
        .clone()
        .filter(|_| app.dialog != Some(Dialog::AddServer));
    let mascot = if error.is_some() {
        app.upset
    } else if first_run {
        mascot::HAPPY
    } else {
        app.greeter
    };
    let hint = if app.joining {
        "Joining…"
    } else if first_run {
        "Add a server with its address and an invite code."
    } else if error.is_some() {
        "Try again, or pick another server on the left."
    } else {
        "Join a server on the left, or add a new one."
    };
    frame(mascot)
        .children(first_run.then(|| div().typeset(theme::LARGE_TITLE).child("Welcome to throcc")))
        .child(description(hint))
        .children(error.map(|e| div().w(px(420.)).child(error_box(e))))
        .children((first_run && !app.joining).then(|| {
            primary("add-first-server", "Add Server")
                .h_10()
                .px_4()
                .on_click(cx.listener(|app, _, window, cx| app.open_add_server(window, cx)))
        }))
}

/// The window in place of the room while the connection is lost, or `None` while it holds.
pub fn render_lost(app: &ThroccApp) -> Option<Div> {
    let (hint, error) = match &app.state.connection {
        ConnectionStatus::Reconnecting { attempt, reason } => (
            format!("Reconnecting, attempt {attempt}. You’ll be put back in your room."),
            format!("Lost the connection: {reason}."),
        ),
        ConnectionStatus::Failed(reason) => (
            "Pick the server on the left to try again.".to_owned(),
            reason.clone(),
        ),
        _ => return None,
    };
    Some(
        frame(app.upset)
            .child(description(hint))
            .child(div().w(px(420.)).child(error_box(error))),
    )
}

fn frame(mascot: &'static str) -> Div {
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
