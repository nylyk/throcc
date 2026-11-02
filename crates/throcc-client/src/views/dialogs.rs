use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_component::Disableable;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use throcc_client_core::client::Command;
use throcc_client_core::state::{FRAME_RATES, Resolution, WarningCause};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::messages::Codec;

use super::common::{
    Typeset, caption, card, chip, danger, description, error_box, field_error, mono, primary, rate,
    secondary, section, title,
};
use crate::app::{Dialog, ThroccApp, typed_name, typed_password};
use crate::theme;

/// The modal for `dialog`, over a dimmed window.
pub fn render(app: &mut ThroccApp, dialog: Dialog, cx: &mut Context<ThroccApp>) -> AnyElement {
    let body = match dialog {
        Dialog::Share => share(app, cx),
        Dialog::Connection => connection(app, cx),
        Dialog::Verify { user, number } => verify(app, user, number, cx),
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
        Dialog::EditRoom(room) => edit_room(app, room, cx),
        Dialog::DeleteRoom(room) => delete_room(app, room, cx),
        Dialog::RoomPassword { room, error } => room_password(app, room, error, cx),
        Dialog::LeaveServer => leave_server(app, cx),
        Dialog::AddServer => add_server(app, cx),
        Dialog::RemoveServer(address) => remove_server(app, address, cx),
    };
    div()
        .id("scrim")
        .absolute()
        .inset_0()
        .occlude()
        .bg(theme::scrim())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, _, _, cx| app.set_dialog(None, cx)),
        )
        .flex()
        .items_center()
        .justify_center()
        .child(
            card()
                .id("dialog")
                .w(px(560.))
                .max_w(relative(0.9))
                .max_h(relative(0.9))
                .overflow_y_scroll()
                .rounded(theme::RADIUS_DIALOG)
                .border_color(theme::border_strong())
                .p_6()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(body),
        )
        .into_any_element()
}

/// A dialog's column, under its `heading`.
fn body(heading: impl Into<SharedString>) -> Div {
    div().flex().flex_col().gap_4().child(title(heading))
}

/// A dialog's buttons along the bottom, starting with `cancel`, which closes it.
fn actions(cancel: &'static str, cx: &mut Context<ThroccApp>) -> Div {
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(close_button(cancel, cx))
}

/// `button`, which sends the command `command` makes and closes the dialog.
fn sends(
    button: Button,
    command: impl Fn(&ThroccApp) -> Command + 'static,
    cx: &mut Context<ThroccApp>,
) -> Button {
    button.on_click(cx.listener(move |app, _, _, cx| {
        app.command(command(app));
        app.set_dialog(None, cx);
    }))
}

/// The room dialogs' name field, with why the server would refuse the typed name.
fn room_name(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> Div {
    let error = typed_name(&app.room_name, cx).err();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Input::new(&app.room_name))
        .children(error.map(field_error))
}

/// Why the server would refuse the password typed in the room dialogs.
fn room_password_error(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> Option<Div> {
    typed_password(&app.room_password, cx)
        .err()
        .map(field_error)
}

fn new_room(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    body("New Room")
        .child(room_name(app, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Password"))
                .child(Input::new(&app.room_password))
                .children(room_password_error(app, cx))
                .child(caption("Leave it empty to let anyone join.")),
        )
        .child(
            actions("Cancel", cx).child(
                primary("create-room", "Create")
                    .disabled(!app.can_create_room(cx))
                    .on_click(cx.listener(|app, _, _, cx| app.create_room(cx))),
            ),
        )
        .into_any_element()
}

fn delete_room(app: &mut ThroccApp, room: RoomId, cx: &mut Context<ThroccApp>) -> AnyElement {
    let name = app
        .state
        .room(room)
        .map_or_else(|| "this room".to_owned(), |r| r.name.clone());
    let occupied = app.state.occupants(room).next().is_some();
    body(format!("Delete “{name}”?"))
        .child(description(if occupied {
            "Everyone in it goes back to the lobby. This can’t be undone."
        } else {
            "This can’t be undone."
        }))
        .child(actions("Cancel", cx).child(sends(
            danger("delete-room", "Delete"),
            move |_| Command::DeleteRoom(room),
            cx,
        )))
        .into_any_element()
}

fn edit_room(app: &mut ThroccApp, room: RoomId, cx: &mut Context<ThroccApp>) -> AnyElement {
    let locked = app.state.room(room).is_some_and(|r| r.locked);
    body("Edit Room")
        .child(room_name(app, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Password"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&app.room_password)))
                        .children(locked.then(|| {
                            sends(
                                secondary("unlock-room", "Remove"),
                                move |_| Command::SetRoomPassword(room, None),
                                cx,
                            )
                        })),
                )
                .children(room_password_error(app, cx))
                .child(caption(if locked {
                    "Members need the password to join. Type a new one to change it."
                } else {
                    "Anyone can join. Type a password to lock the room."
                })),
        )
        .child(
            actions("Cancel", cx).child(
                primary("save-room", "Save")
                    .disabled(!app.can_save_room(cx))
                    .on_click(cx.listener(|app, _, _, cx| app.save_room(cx))),
            ),
        )
        .into_any_element()
}

fn room_password(
    app: &mut ThroccApp,
    room: RoomId,
    error: Option<String>,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let name = app
        .state
        .room(room)
        .map_or_else(|| "this room".to_owned(), |r| r.name.clone());
    body(format!("Join “{name}”"))
        .child(description(
            "This room is locked. Enter its password to join.",
        ))
        .child(Input::new(&app.room_password))
        .children(room_password_error(app, cx).or(error.map(field_error)))
        .child(
            actions("Cancel", cx).child(
                primary("join-locked-room", "Join")
                    .disabled(!app.can_join_with_password(cx))
                    .on_click(cx.listener(|app, _, _, cx| app.join_with_password(cx))),
            ),
        )
        .into_any_element()
}

fn close_button(label: &'static str, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    secondary("close-dialog", label).on_click(cx.listener(|app, _, _, cx| app.set_dialog(None, cx)))
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
    callout(theme::warn_bg(), theme::warn_border(), theme::warn_text())
}

fn danger_box() -> Div {
    callout(
        theme::danger_bg(),
        theme::danger_border(),
        theme::danger_text(),
    )
}

fn callout(bg: Hsla, border: Hsla, text: Hsla) -> Div {
    div()
        .p_4()
        .rounded(theme::RADIUS_TILE)
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(text)
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
                .items_center()
                .justify_between()
                .child(section("Bitrate"))
                .child(mono(mbps(choice.kbps)).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(Slider::new(&app.bitrate))
        .child(
            div()
                .flex()
                .justify_between()
                .child(
                    caption(mbps(offered.first().copied().unwrap_or(0)))
                        .font_family(theme::mono_font()),
                )
                .child(caption(mbps(choice.max_kbps())).font_family(theme::mono_font())),
        );
    body("Share Your Screen")
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
        .child(actions("Cancel", cx).child(sends(
            primary("start-share", "Start Sharing"),
            |app| Command::StartShare(app.share_choice.config(Codec::Av1, app.share_audio)),
            cx,
        )))
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
        .child(mono(value).typeset(theme::TITLE_2))
}

fn connection(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let health = &app.state.health;
    let table_row = |label: String, detail: String, [set, rate, last]: [String; 3], ruled: bool| {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .py_2()
            .when(ruled, |row| row.border_b_1().border_color(theme::control()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().truncate().child(label))
                    .child(mono(set).w(px(90.)))
                    .child(mono(rate).w(px(90.)))
                    .child(mono(last).w(px(60.))),
            )
            .child(caption(detail).truncate())
    };
    let table_head = |rate: &'static str, last: &'static str| {
        div()
            .flex()
            .gap_2()
            .pb_1()
            .border_b_1()
            .border_color(theme::control())
            .text_color(theme::text_3())
            .typeset(theme::CAPTION)
            .font_weight(FontWeight::MEDIUM)
            .child(div().flex_1().child("Track"))
            .child(div().w(px(90.)).child("Set"))
            .child(div().w(px(90.)).child(rate))
            .child(div().w(px(60.)).child(last))
    };
    let heading = |text: &'static str| div().pb_1().typeset(theme::HEADLINE).child(text);
    let sending = health.sending.iter().enumerate().map(|(i, row)| {
        table_row(
            row.label.to_owned(),
            row.detail.clone(),
            [
                rate(row.set_kbps),
                rate(row.leaving_kbps),
                row.dropped.map_or("–".to_owned(), |d| d.to_string()),
            ],
            i + 1 < health.sending.len(),
        )
    });
    let receiving = health.receiving.iter().enumerate().map(|(i, row)| {
        table_row(
            row.label.clone(),
            row.detail.clone(),
            [
                row.set_kbps.map_or("–".to_owned(), rate),
                row.arriving_kbps.map_or("Muted".to_owned(), rate),
                row.loss.map_or("–".to_owned(), |l| format!("{l:.1}%")),
            ],
            i + 1 < health.receiving.len(),
        )
    });
    body("Connection")
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
                .flex()
                .flex_col()
                .gap_1()
                .child(heading("Sending"))
                .child(if health.sending.is_empty() {
                    caption("Nothing").into_any_element()
                } else {
                    table_head("Leaving", "Dropped").into_any_element()
                })
                .children(sending),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(heading("Receiving"))
                .child(if health.receiving.is_empty() {
                    caption("Nothing").into_any_element()
                } else {
                    table_head("Arriving", "Loss").into_any_element()
                })
                .children(receiving),
        )
        .child(div().flex().justify_end().child(close_button("Done", cx)))
        .into_any_element()
}

fn verify(
    app: &ThroccApp,
    user: UserId,
    number: Option<String>,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let name = app
        .state
        .person(user)
        .map_or(format!("User {}", user.0), |p| p.name.clone());
    let verified = app.state.person(user).is_some_and(|p| p.verified);
    let causes = || {
        app.state
            .key_warnings
            .iter()
            .filter(|w| w.user == user)
            .map(|w| &w.cause)
    };
    let key_change = causes().find_map(|c| match c {
        WarningCause::KeyChanged { pinned, now } => Some((*pinned, *now)),
        WarningCause::SameName { .. } => None,
    });
    let namesake = causes().find_map(|c| match c {
        WarningCause::SameName { known } => Some(*known),
        WarningCause::KeyChanged { .. } => None,
    });
    let changed = key_change.is_some();
    let mut content = body(name.clone()).child(div().flex().gap_2().child(if changed {
        chip("Key changed", theme::danger_bg(), theme::danger_text())
    } else if namesake.is_some() {
        chip("Same name", theme::danger_bg(), theme::danger_text())
    } else if verified {
        chip("Verified", theme::ok_bg(), theme::ok_text())
    } else {
        chip("Not verified", theme::raised(), theme::text_2())
    }));
    let key = |label: &'static str, key: String| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(caption(label).w(px(48.)))
            .child(mono(key))
    };
    if let Some((pinned, now)) = key_change {
        content = content.child(
            danger_box()
                .child(div().typeset(theme::HEADLINE).child(format!("{name}’s key changed")))
                .child(format!(
                    "The server is showing a different key for {name} than the one you know. An honest server never does this, so someone may be trying to listen in."
                ))
                .child(key("Before", pinned.short()))
                .child(key("Now", now.short()))
                .child(caption("Nothing you say or share is encrypted to the new key, so it can’t hear or see you.")),
        );
    }
    if let Some(known) = namesake {
        content = content.child(
            danger_box()
                .child(div().typeset(theme::HEADLINE).child(format!("Someone you know is also called {name}")))
                .child(
                    "This is a new account using the name of a person you met here before. A server trying to listen in would pose as them just like this.",
                )
                .child(key("Known", format!("User {}", known.0)))
                .child(key("New", format!("User {}", user.0)))
                .child(caption("Compare safety numbers with the real person before you trust this one. Marking them verified clears this warning.")),
        );
    }
    let groups: Vec<String> = number.as_deref().map(safety_groups).unwrap_or_default();
    content
        .child(section("Safety number"))
        .child(if groups.is_empty() {
            caption("You haven’t seen this person’s key in a room yet.").into_any_element()
        } else {
            div()
                .grid()
                .grid_cols(4)
                .gap_3()
                .font_family(theme::mono_font())
                .typeset(theme::TITLE_2)
                .font_weight(FontWeight::NORMAL)
                .children(groups.into_iter().map(|g| div().child(g)))
                .into_any_element()
        })
        .child(description(if changed {
            format!(
                "This number belongs to the key you knew. If it matches {name}’s, you know the real {name}, but the server is still showing a different key."
            )
        } else {
            format!(
                "Compare this with {name} in person or through another app. If both match, nobody between you is swapping keys."
            )
        }))
        .child(
            actions("Done", cx).child(
                primary("mark-verified", if verified { "Mark as Not Verified" } else { "Mark as Verified" })
                    .disabled(!verified && number.is_none())
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
    body(format!("Remove “{name}”?"))
        .child(description(
            "They are disconnected at once and need a new invite to come back.",
        ))
        .child(
            Switch::new("cascade")
                .checked(cascade)
                .label("Also remove everyone they invited, and everyone those people invited")
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    let dialog = Dialog::Remove {
                        user,
                        name: name_for_toggle.clone(),
                        cascade: *checked,
                    };
                    app.set_dialog(Some(dialog), cx);
                })),
        )
        .child(actions("Cancel", cx).child(sends(
            danger("confirm-remove", "Remove"),
            move |_| Command::RemoveUser { user, cascade },
            cx,
        )))
        .into_any_element()
}

fn add_server(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    body("Add Server")
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
                .child(caption("The code also proves you reached the real server. Leave it empty for a server you’ve joined before.")),
        )
        .children(app.join_error.clone().map(error_box))
        .child(
            actions("Cancel", cx).child(
                primary("join", if app.joining { "Joining…" } else { "Join" })
                    .disabled(app.joining)
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.join(window, cx);
                        cx.notify();
                    })),
            ),
        )
        .into_any_element()
}

fn leave_server(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let server = app
        .state
        .server
        .as_ref()
        .map_or("this server".to_owned(), |s| s.title().to_owned());
    body(format!("Leave “{server}”?"))
        .child(description(
            "You lose your membership and the server leaves your list. Coming back needs a new invite.",
        ))
        .child(actions("Cancel", cx).child(sends(
            danger("confirm-leave", "Leave"),
            |_| Command::LeaveServer,
            cx,
        )))
        .into_any_element()
}

fn remove_server(app: &ThroccApp, address: String, cx: &mut Context<ThroccApp>) -> AnyElement {
    let server = app.server_name(&address).unwrap_or_else(|| address.clone());
    body(format!("Remove “{server}”?"))
        .child(description(
            "It leaves your list, but you stay a member. Add it again with its address, no invite needed. To end your membership, connect and choose Leave Server.",
        ))
        .child(
            actions("Cancel", cx).child(
                danger("confirm-remove-server", "Remove").on_click(cx.listener(
                    move |app, _, _, cx| app.remove_server(&address, cx),
                )),
            ),
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
    body("This server’s key changed")
        .child(
            warning_box()
                .child(caption("You trusted"))
                .child(mono(expected.to_string()).typeset(theme::CAPTION))
                .child(caption("It now has"))
                .child(mono(actual.to_string()).typeset(theme::CAPTION)),
        )
        .child(description(
            "This happens when the server was reinstalled, and also when someone is pretending to be it. The safe way back is a fresh invite code: add the server again with it, and the code proves the new key.",
        ))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(primary("use-invite", "Use a Fresh Invite").on_click(cx.listener(|app, _, window, cx| {
                    app.code.update(cx, |s, cx| s.focus(window, cx));
                    app.set_dialog(Some(Dialog::AddServer), cx);
                })))
                .child(danger("trust-anyway", "Trust the New Key").on_click(cx.listener(move |app, _, window, cx| {
                    app.trust_new_key(&address, actual, window, cx);
                    cx.notify();
                }))),
        )
        .into_any_element()
}
