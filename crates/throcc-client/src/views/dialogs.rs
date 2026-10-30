use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::input::Input;
use gpui_component::switch::Switch;
use throcc_client_core::client::Command;
use throcc_client_core::state::Preset;
use throcc_proto::messages::Codec;

use super::common::{caption, card, danger, mono, primary, secondary, section, title};
use crate::app::{Dialog, ThroccApp};
use crate::theme;

/// The modal for `dialog`, over a dimmed window.
pub fn render(
    app: &mut ThroccApp,
    dialog: Dialog,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let body = match dialog {
        Dialog::Share { preset, audio } => share(preset, audio, cx),
        Dialog::Connection => connection(app),
        Dialog::Verify(user) => verify(app, user, cx),
        Dialog::Remove {
            user,
            name,
            cascade,
        } => remove(user, &name, cascade, cx),
        Dialog::PinMismatch {
            address,
            expected,
            actual,
        } => pin_mismatch(&address, expected, actual, cx),
        Dialog::NewRoom => new_room(app, cx),
    };
    let _ = window;
    div()
        .absolute()
        .inset_0()
        .bg(gpui::black().opacity(0.6))
        .flex()
        .items_center()
        .justify_center()
        .child(
            card()
                .id("dialog")
                .w(px(560.))
                .max_h(px(720.))
                .overflow_y_scroll()
                .rounded(theme::RADIUS_DIALOG)
                .border_color(theme::border_strong())
                .p_6()
                .child(body),
        )
        .into_any_element()
}

fn new_room(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("New room"))
        .child(Input::new(&app.room_name))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(close_button(cx))
                .child(
                    primary("create-room", "Create")
                        .on_click(cx.listener(|app, _, _, cx| app.create_room(cx))),
                ),
        )
        .into_any_element()
}

fn close_button(cx: &mut Context<ThroccApp>) -> impl IntoElement {
    secondary("close-dialog", "Close").on_click(cx.listener(|app, _, _, cx| {
        app.dialog = None;
        cx.notify();
    }))
}

fn share(preset: Preset, audio: bool, cx: &mut Context<ThroccApp>) -> AnyElement {
    let presets = Preset::ALL.iter().map(|p| {
        let p = *p;
        let config = p.config(Codec::Av1, audio);
        let selected = p == preset;
        div()
            .id(("preset", p as usize))
            .p_3()
            .rounded(theme::RADIUS_BUTTON)
            .border_1()
            .border_color(if selected {
                theme::accent()
            } else {
                theme::control()
            })
            .bg(if selected {
                theme::raised()
            } else {
                theme::surface()
            })
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().font_weight(FontWeight::MEDIUM).child(p.label()))
            .child(caption(format!(
                "{}p{} · {} Mbps",
                config.height,
                config.fps,
                config.kbps as f32 / 1000.0
            )))
            .on_click(cx.listener(move |app, _, _, cx| {
                app.dialog = Some(Dialog::Share { preset: p, audio });
                cx.notify();
            }))
    });
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Share your screen"))
        .child(section("Source"))
        .child(caption(
            "Your system's own picker opens when you start sharing, so you choose a display or window there.",
        ))
        .child(section("Quality"))
        .child(div().grid().grid_cols(2).gap_2().children(presets))
        .child(
            Switch::new("share-audio")
                .checked(audio)
                .label("Share audio: everything except throcc itself")
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    app.dialog = Some(Dialog::Share { preset, audio: *checked });
                    cx.notify();
                })),
        )
        .child(caption("Encoder: chosen when sharing starts, best of AV1, HEVC and H.264 in hardware."))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(close_button(cx))
                .child(primary("start-share", "Start sharing").on_click(cx.listener(move |app, _, _, cx| {
                    app.command(Command::StartShare(preset.config(Codec::Av1, audio)));
                    app.dialog = None;
                    cx.notify();
                }))),
        )
        .into_any_element()
}

fn stat(label: &str, value: String) -> impl IntoElement {
    card()
        .p_3()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(caption(label.to_owned()))
        .child(
            mono(value)
                .text_size(px(18.))
                .font_weight(FontWeight::SEMIBOLD),
        )
}

fn connection(app: &ThroccApp) -> AnyElement {
    let health = &app.state.health;
    let rows = health.receiving.iter().map(|row| {
        let kbps = |k: Option<u32>| {
            k.map_or("–".to_owned(), |k| {
                if k >= 1000 {
                    format!("{:.1} Mbps", k as f32 / 1000.0)
                } else {
                    format!("{k} kbps")
                }
            })
        };
        div()
            .flex()
            .gap_2()
            .py_1()
            .border_b_1()
            .border_color(theme::control())
            .child(div().flex_1().truncate().child(row.label.clone()))
            .child(mono(kbps(row.set_kbps)).w(px(90.)))
            .child(
                mono(if row.arriving_kbps.is_none() {
                    "muted".to_owned()
                } else {
                    kbps(row.arriving_kbps)
                })
                .w(px(90.)),
            )
            .child(mono(row.loss.map_or("–".to_owned(), |l| format!("{l:.1}%"))).w(px(60.)))
    });
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Connection"))
        .child(caption("Last 2 seconds"))
        .child(
            div()
                .flex()
                .gap_2()
                .child(stat("Round trip", format!("{:.0} ms", health.rtt_ms)))
                .child(stat("Upload loss", format!("{:.1}%", health.upload_loss)))
                .child(stat(
                    "Download loss",
                    format!("{:.1}%", health.download_loss),
                ))
                .child(stat("Dropped for you", health.dropped_for_you.to_string())),
        )
        .child(
            div()
                .p_3()
                .rounded(theme::RADIUS_ROW)
                .bg(theme::raised())
                .child(health.verdict.clone()),
        )
        .child(section("Receiving"))
        .child(
            div()
                .flex()
                .gap_2()
                .text_color(theme::text_3())
                .text_size(px(12.))
                .child(div().flex_1().child("Track"))
                .child(div().w(px(90.)).child("Set"))
                .child(div().w(px(90.)).child("Arriving"))
                .child(div().w(px(60.)).child("Loss")),
        )
        .children(rows)
        .child(section("Sending"))
        .child(mono(health.sending.clone()))
        .into_any_element()
}

fn verify(
    app: &ThroccApp,
    user: throcc_proto::ids::UserId,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let name = app
        .state
        .person(user)
        .map_or(format!("User {}", user.0), |p| p.name.clone());
    let verified = app.state.person(user).is_some_and(|p| p.verified);
    let warning = app
        .state
        .key_warnings
        .iter()
        .find(|w| w.user == user)
        .cloned();
    let number = app.safety_number(user);
    let mut content = div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title(name.clone()))
        .child(div().flex().gap_2().child(if verified {
            super::common::chip("Verified", theme::ok_bg(), theme::ok_text())
        } else {
            super::common::chip("Not verified", theme::raised(), theme::text_2())
        }));
    if let Some(warning) = warning {
        content = content.child(
            div()
                .p_4()
                .rounded(theme::RADIUS_TILE)
                .bg(theme::warn_bg())
                .border_1()
                .border_color(theme::warn_border())
                .text_color(theme::warn_text())
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{name}'s key changed")))
                .child("Check before you trust it.")
                .child(div().flex().gap_2().child(caption("Before")).child(mono(warning.pinned.short())))
                .child(div().flex().gap_2().child(caption("Now")).child(mono(warning.now.short())))
                .child(caption("This happens when someone reinstalls, and also when a server swaps keys to listen in."))
                .child(div().flex().child(primary("accept-key", "Accept new key").on_click(cx.listener(move |app, _, _, cx| {
                    app.command(Command::AcceptKey(user));
                    cx.notify();
                })))),
        );
    }
    let groups: Vec<String> = number.as_deref().map(safety_groups).unwrap_or_default();
    content
        .child(section("Safety number"))
        .child(if groups.is_empty() {
            caption("You haven't seen this person's key in a room yet.").into_any_element()
        } else {
            div()
                .grid()
                .grid_cols(4)
                .gap_3()
                .font_family(theme::mono_font())
                .text_size(px(18.))
                .children(groups.into_iter().map(|g| div().child(g)))
                .into_any_element()
        })
        .child(caption(format!(
            "Compare this with {name} in person or through another app. If both match, nobody between you is swapping keys."
        )))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(close_button(cx))
                .child(
                    primary("mark-verified", if verified { "Mark as not verified" } else { "Mark as verified" })
                        .on_click(cx.listener(move |app, _, _, cx| {
                            app.command(Command::SetVerified(user, !verified));
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
}

fn safety_groups(number: &str) -> Vec<String> {
    number
        .as_bytes()
        .chunks(5)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .map(str::to_owned)
        .collect()
}

fn remove(
    user: throcc_proto::ids::UserId,
    name: &str,
    cascade: bool,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let name_for_toggle = name.to_owned();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title(format!("Remove {name}?")))
        .child(caption(
            "They are disconnected at once and need a new invite to come back.",
        ))
        .child(
            Switch::new("cascade")
                .checked(cascade)
                .label("Also remove everyone they invited, and everyone those people invited")
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    app.dialog = Some(Dialog::Remove {
                        user,
                        name: name_for_toggle.clone(),
                        cascade: *checked,
                    });
                    cx.notify();
                })),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(close_button(cx))
                .child(danger("confirm-remove", "Remove").on_click(cx.listener(
                    move |app, _, _, cx| {
                        app.command(Command::RemoveUser { user, cascade });
                        app.dialog = None;
                        cx.notify();
                    },
                ))),
        )
        .into_any_element()
}

fn pin_mismatch(
    address: &str,
    expected: throcc_proto::Fingerprint,
    actual: throcc_proto::Fingerprint,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let address = address.to_owned();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("This server's key changed"))
        .child(
            div()
                .p_4()
                .rounded(theme::RADIUS_TILE)
                .bg(theme::warn_bg())
                .border_1()
                .border_color(theme::warn_border())
                .text_color(theme::warn_text())
                .flex()
                .flex_col()
                .gap_2()
                .child(caption("You trusted"))
                .child(mono(expected.to_string()).text_size(px(12.)))
                .child(caption("It now has"))
                .child(mono(actual.to_string()).text_size(px(12.))),
        )
        .child(caption(
            "This happens when the server was reinstalled, and also when someone is pretending to be it. The safe way back is a fresh invite code: enter it on the join screen and the code proves the new key.",
        ))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(primary("use-invite", "Use a fresh invite").on_click(cx.listener(|app, _, window, cx| {
                    app.dialog = None;
                    app.code.update(cx, |s, cx| s.focus(window, cx));
                    cx.notify();
                })))
                .child(danger("trust-anyway", "Trust the new key").on_click(cx.listener(move |app, _, window, cx| {
                    app.trust_new_key(&address, actual, window, cx);
                    cx.notify();
                }))),
        )
        .into_any_element()
}
