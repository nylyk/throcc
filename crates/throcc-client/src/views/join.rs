use gpui::{Context, Div, FontWeight, IntoElement, ParentElement, Styled, div, px};
use throcc_client_core::account::SignInError;
use throcc_client_core::connection::ConnectError;
use throcc_client_core::control::LoginError;
use throcc_proto::messages::AuthError;

use super::common::{caption, moves_window, primary};
use crate::app::{Dialog, ThroccApp};
use crate::theme;

/// Turns a failed sign-in into a sentence for the join screen.
pub fn explain(error: &SignInError) -> String {
    match error {
        SignInError::NotEnrolled(address) => {
            format!("You haven't joined {address} yet. Ask someone there for an invite code.")
        }
        SignInError::Login(LoginError::Refused(AuthError::BadInvite)) | SignInError::Login(LoginError::Pake(_)) => {
            "That code didn't work. It may be mistyped, used up or expired, or something between you and the server isn't the server. Ask for a fresh one.".into()
        }
        SignInError::Login(LoginError::Refused(AuthError::RateLimited)) => {
            "Too many wrong codes from your network. Wait ten minutes, then try again.".into()
        }
        SignInError::Login(LoginError::Refused(AuthError::UnknownKey)) => {
            "This server doesn't know you any more. Join again with a fresh invite code.".into()
        }
        SignInError::Connect(ConnectError::Resolve(address)) => format!("Couldn't find {address}. Check the address."),
        SignInError::Connect(ConnectError::Connection(_)) => {
            "Couldn't reach the server. Check the address, and that UDP port 8476 is open.".into()
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
        .text_size(px(13.))
        .child(error)
}

/// The window with no server connected: how a join is going, or what to do next.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let hint = if app.joining {
        "Joining…"
    } else if app.known_servers.is_empty() {
        "Add a server with its address and an invite code."
    } else {
        "Pick a server on the left."
    };
    let error = app
        .join_error
        .clone()
        .filter(|_| app.dialog != Some(Dialog::AddServer));
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
        .child(
            div()
                .text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("throcc"),
        )
        .child(caption(hint))
        .children(error.map(|e| div().w(px(420.)).child(error_box(e))))
        .children((app.known_servers.is_empty() && !app.joining).then(|| {
            primary("add-first-server", "Add server")
                .h(theme::TOUCH)
                .on_click(cx.listener(|app, _, window, cx| app.open_add_server(window, cx)))
        }))
}
