use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::input::Input;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use throcc_client_core::client::Command;
use throcc_client_core::state::{FRAME_RATES, Resolution};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::messages::Codec;

use super::common::{caption, card, chip, danger, mono, primary, secondary, section, title};
use crate::app::{Dialog, ThroccApp};
use crate::theme;

/// The modal for `dialog`, over a dimmed window.
pub fn render(app: &mut ThroccApp, dialog: Dialog, cx: &mut Context<ThroccApp>) -> AnyElement {
    let body = match dialog {
        Dialog::Share => share(app, cx),
        Dialog::Connection => connection(app, cx),
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
        Dialog::EditRoom {
            room,
            confirm_delete,
        } => edit_room(app, room, confirm_delete, cx),
    };
    div()
        .id("scrim")
        .absolute()
        .inset_0()
        .bg(theme::scrim())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                app.dialog = None;
                cx.notify();
            }),
        )
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
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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

fn edit_room(
    app: &mut ThroccApp,
    room: RoomId,
    confirm_delete: bool,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let name = app
        .state
        .room(room)
        .map_or_else(|| "this room".to_owned(), |r| r.name.clone());
    if confirm_delete {
        let occupied = app.state.occupants(room).next().is_some();
        return div()
            .flex()
            .flex_col()
            .gap_4()
            .child(title(format!("Delete {name}?")))
            .child(caption(if occupied {
                "Everyone in it goes back to the lobby. This can't be undone."
            } else {
                "This can't be undone."
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(secondary("keep-room", "Cancel").on_click(cx.listener(
                        move |app, _, _, cx| {
                            app.dialog = Some(Dialog::EditRoom {
                                room,
                                confirm_delete: false,
                            });
                            cx.notify();
                        },
                    )))
                    .child(
                        danger("delete-room", format!("Delete {name}")).on_click(cx.listener(
                            move |app, _, _, cx| {
                                app.command(Command::DeleteRoom(room));
                                app.dialog = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .into_any_element();
    }
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Edit room"))
        .child(Input::new(&app.room_name))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    danger("ask-delete-room", "Delete room").on_click(cx.listener(
                        move |app, _, _, cx| {
                            app.dialog = Some(Dialog::EditRoom {
                                room,
                                confirm_delete: true,
                            });
                            cx.notify();
                        },
                    )),
                )
                .child(div().flex_1())
                .child(close_button(cx))
                .child(
                    primary("rename-room", "Save")
                        .on_click(cx.listener(|app, _, _, cx| app.rename_room(cx))),
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

/// A row of mutually exclusive choices.
fn choices<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: impl IntoIterator<Item = (T, String)>,
    selected: T,
    cx: &mut Context<ThroccApp>,
    on_pick: impl Fn(&mut ThroccApp, T, &mut Context<ThroccApp>) + Clone + 'static,
) -> Div {
    div()
        .flex()
        .gap_1()
        .p_1()
        .rounded(theme::RADIUS_BUTTON)
        .bg(theme::raised())
        .children(options.into_iter().enumerate().map(|(i, (value, label))| {
            let on_pick = on_pick.clone();
            let picked = value == selected;
            div()
                .id((id, i))
                .flex_1()
                .flex()
                .justify_center()
                .py_1()
                .rounded(theme::RADIUS_ROW)
                .cursor_pointer()
                .when(picked, |s| s.bg(theme::surface()))
                .text_color(if picked {
                    theme::text()
                } else {
                    theme::text_3()
                })
                .child(label)
                .on_click(cx.listener(move |app, _, _, cx| on_pick(app, value, cx)))
        }))
}

fn warning_box() -> Div {
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
}

fn mbps(kbps: u32) -> String {
    format!("{} Mbps", kbps as f32 / 1000.0)
}

fn share(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let choice = app.share_choice;
    let audio = app.share_audio;
    let offered = choice.bitrates();
    let resolutions = choices(
        "resolution",
        Resolution::ALL.map(|r| (r, r.label().to_owned())),
        choice.resolution,
        cx,
        |app, resolution, cx| {
            app.set_share_choice(app.share_choice.with_resolution(resolution), cx)
        },
    );
    let frame_rates = choices(
        "fps",
        FRAME_RATES.map(|fps| (fps, format!("{fps} fps"))),
        choice.fps,
        cx,
        |app, fps, cx| app.set_share_choice(app.share_choice.with_fps(fps), cx),
    );
    let bitrate = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .justify_between()
                .child(section("Bitrate"))
                .child(mono(mbps(choice.kbps)).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(Slider::new(&app.bitrate))
        .child(
            div()
                .flex()
                .justify_between()
                .child(caption(mbps(offered.first().copied().unwrap_or(0))))
                .child(caption(mbps(choice.max_kbps()))),
        );
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Share your screen"))
        .child(section("Resolution"))
        .child(resolutions)
        .child(section("Frame rate"))
        .child(frame_rates)
        .child(bitrate)
        .child(
            Switch::new("share-audio")
                .checked(audio)
                .label("Share audio: everything except throcc itself")
                .on_click(cx.listener(|app, checked: &bool, _, cx| {
                    app.share_audio = *checked;
                    cx.notify();
                })),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(close_button(cx))
                .child(
                    primary("start-share", "Start sharing").on_click(cx.listener(
                        |app, _, _, cx| {
                            let config = app.share_choice.config(Codec::Av1, app.share_audio);
                            app.command(Command::StartShare(config));
                            app.dialog = None;
                            cx.notify();
                        },
                    )),
                ),
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

fn connection(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let health = &app.state.health;
    let kbps = |k: u32| {
        if k >= 1000 {
            format!("{:.1} Mbps", k as f32 / 1000.0)
        } else {
            format!("{k} kbps")
        }
    };
    let table_row = |cells: [String; 4]| {
        let [track, set, rate, last] = cells;
        div()
            .flex()
            .gap_2()
            .py_1()
            .border_b_1()
            .border_color(theme::control())
            .child(div().flex_1().truncate().child(track))
            .child(mono(set).w(px(90.)))
            .child(mono(rate).w(px(90.)))
            .child(mono(last).w(px(60.)))
    };
    let table_head = |rate: &'static str, last: &'static str| {
        div()
            .flex()
            .gap_2()
            .text_color(theme::text_3())
            .text_size(px(12.))
            .child(div().flex_1().child("Track"))
            .child(div().w(px(90.)).child("Set"))
            .child(div().w(px(90.)).child(rate))
            .child(div().w(px(60.)).child(last))
    };
    let receiving = health.receiving.iter().map(|row| {
        table_row([
            row.label.clone(),
            row.set_kbps.map_or("–".to_owned(), kbps),
            row.arriving_kbps.map_or("muted".to_owned(), kbps),
            row.loss.map_or("–".to_owned(), |l| format!("{l:.1}%")),
        ])
    });
    let sending = health.sending.iter().map(|row| {
        table_row([
            row.label.to_owned(),
            kbps(row.set_kbps),
            kbps(row.leaving_kbps),
            row.dropped.map_or("–".to_owned(), |d| d.to_string()),
        ])
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
        .child(table_head("Arriving", "Loss"))
        .children(receiving)
        .child(section("Sending"))
        .child(if health.sending.is_empty() {
            caption("Nothing").into_any_element()
        } else {
            table_head("Leaving", "Dropped").into_any_element()
        })
        .children(sending)
        .child(div().flex().justify_end().child(close_button(cx)))
        .into_any_element()
}

fn verify(app: &ThroccApp, user: UserId, cx: &mut Context<ThroccApp>) -> AnyElement {
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
        .child(div().flex().gap_2().child(if warning.is_some() {
            chip("Key changed", theme::warn_bg(), theme::warn_text())
        } else if verified {
            chip("Verified", theme::ok_bg(), theme::ok_text())
        } else {
            chip("Not verified", theme::raised(), theme::text_2())
        }));
    if let Some(warning) = warning {
        let shown = warning.now.0;
        content = content.child(
            warning_box()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{name}'s key changed")))
                .child("Check before you trust it.")
                .child(div().flex().gap_2().child(caption("Before")).child(mono(warning.pinned.short())))
                .child(div().flex().gap_2().child(caption("Now")).child(mono(warning.now.short())))
                .child(caption("This happens when someone reinstalls, and also when a server swaps keys to listen in."))
                .child(div().flex().child(primary("accept-key", "Accept new key").on_click(cx.listener(move |app, _, _, cx| {
                    app.command(Command::AcceptKey(user, shown));
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

fn remove(user: UserId, name: &str, cascade: bool, cx: &mut Context<ThroccApp>) -> AnyElement {
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
    expected: Fingerprint,
    actual: Fingerprint,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let address = address.to_owned();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("This server's key changed"))
        .child(
            warning_box()
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
