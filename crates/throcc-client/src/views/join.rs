use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::Disableable;
use gpui_component::input::Input;
use throcc_client_core::account::SignInError;
use throcc_client_core::connection::ConnectError;
use throcc_client_core::control::LoginError;
use throcc_proto::messages::AuthError;

use super::common::{avatar, caption, card, mono, primary, section, title};
use crate::app::ThroccApp;
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

pub fn render(
    app: &mut ThroccApp,
    _window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let servers = app.known_servers.clone();
    let form = card()
        .w(px(420.))
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Join a server"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("Address"))
                .child(Input::new(&app.address).font_family(theme::mono_font())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("Invite code"))
                .child(Input::new(&app.code).font_family(theme::mono_font()))
                .child(caption("The code also proves you reached the real server. Leave it empty for a server you've joined before.")),
        )
        .children(app.join_error.clone().map(|error| {
            div()
                .p_3()
                .rounded(theme::RADIUS_ROW)
                .bg(theme::warn_bg())
                .border_1()
                .border_color(theme::warn_border())
                .text_color(theme::warn_text())
                .text_size(px(13.))
                .child(error)
        }))
        .child(
            primary("join", if app.joining { "Joining…" } else { "Join" })
                .w_full()
                .h(theme::TOUCH)
                .disabled(app.joining)
                .on_click(cx.listener(|app, _, window, cx| {
                    app.join(window, cx);
                    cx.notify();
                })),
        );

    let known = card()
        .w(px(420.))
        .flex()
        .flex_col()
        .gap_2()
        .child(section("Your servers"))
        .children(
            servers
                .iter()
                .enumerate()
                .map(|(i, (address, fingerprint))| {
                    let address = address.clone();
                    div()
                        .id(("server", i))
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .min_h(theme::TOUCH)
                        .rounded(theme::RADIUS_ROW)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::raised()))
                        .child(avatar(&address, i as u64, None, px(28.), false))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::MEDIUM).child(address.clone()))
                                .child(
                                    mono(fingerprint.short())
                                        .text_size(px(12.))
                                        .text_color(theme::text_3()),
                                ),
                        )
                        .on_click(cx.listener(move |app, _, window, cx| {
                            let address = address.clone();
                            app.address
                                .update(cx, |s, cx| s.set_value(address, window, cx));
                            app.code.update(cx, |s, cx| s.set_value("", window, cx));
                            app.join(window, cx);
                            cx.notify();
                        }))
                }),
        );

    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        .child(
            div()
                .text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("throcc"),
        )
        .child(form)
        .children((!servers.is_empty()).then_some(known))
}
