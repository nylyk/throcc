use gpui::{
    AnyElement, AnyWindowHandle, Context, Div, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Stateful, StatefulInteractiveElement, Styled, Window, anchored, div, px,
};
use gpui_component::Icon;
use gpui_component::slider::Slider;
use throcc_client_core::client::Command;
use throcc_client_core::state::{Person, Sound};
use throcc_proto::ids::UserId;
use throcc_proto::permissions;

use super::common::{CONTROLS_LINGER, Typeset, gain_to_loudness, icon, mono, section, toggle_icon};
use crate::app::{Dialog, PlaybackControl, ThroccApp};
use crate::theme;

/// The person menu at the pointer, over a backdrop that closes it, if it is open in `window`.
pub fn render(
    app: &mut ThroccApp,
    window: &Window,
    cx: &mut Context<ThroccApp>,
) -> Option<AnyElement> {
    let menu = app
        .person_menu
        .as_ref()
        .filter(|menu| menu.window == window.window_handle())?;
    let user = menu.user;
    let position = menu.position;
    let person = app.state.person(user).cloned()?;
    let mine = Some(user) == app.state.me;
    let in_popout = app
        .popouts
        .values()
        .any(|popout| AnyWindowHandle::from(*popout) == menu.window);
    let streams_audio = person.sharing.is_some_and(|c| c.audio);
    let panel = div()
        .id("person-menu")
        .w(px(264.))
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border_strong())
        .rounded(theme::RADIUS_CARD)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .px_2()
                .py_1()
                .truncate()
                .typeset(theme::HEADLINE)
                .child(person.name.clone()),
        )
        .children(
            (!mine).then(|| playback("Voice", Sound::Voice, &menu.voice, ("mic", "mic-off"), cx)),
        )
        .children((!mine && streams_audio).then(|| {
            playback(
                "Stream audio",
                Sound::Share,
                &menu.share,
                ("volume-2", "volume-x"),
                cx,
            )
        }))
        .children((!in_popout).then(|| {
            group().child(
                action("person-menu-profile", "circle-user", "Profile").on_click(cx.listener(
                    move |app, _, _, cx| {
                        app.close_person_menu(cx);
                        app.open_profile(user, cx);
                    },
                )),
            )
        }))
        .children(
            person
                .sharing
                .is_some()
                .then(|| stream_actions(app, user, mine, cx)),
        )
        .children(moderation(app, user, &person, in_popout, cx));
    let menu = div()
        .id("person-menu-backdrop")
        .absolute()
        .inset_0()
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, _, _, cx| app.close_person_menu(cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|app, _, _, cx| app.close_person_menu(cx)),
        )
        .child(
            anchored()
                .position(position)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
        )
        .into_any_element();
    Some(menu)
}

/// A sound's mute toggle and volume slider.
fn playback(
    label: &'static str,
    sound: Sound,
    control: &PlaybackControl,
    icons: (&'static str, &'static str),
    cx: &mut Context<ThroccApp>,
) -> Div {
    let muted = control.playback.muted;
    let percent = gain_to_loudness(control.playback.volume) * 100.;
    let toggle_id = match sound {
        Sound::Voice => "person-menu-voice-mute",
        Sound::Share => "person-menu-share-mute",
    };
    div()
        .flex()
        .flex_col()
        .px_2()
        .pb_1()
        .child(section(label))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    toggle_icon(
                        toggle_id,
                        icons.0,
                        icons.1,
                        muted,
                        if muted { "Unmute" } else { "Mute" },
                    )
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.change_playback(sound, |p| p.muted = !p.muted, cx)
                    })),
                )
                .child(div().flex_1().child(Slider::new(&control.slider)))
                .child(
                    mono(format!("{percent:.0}%"))
                        .w(px(44.))
                        .flex_none()
                        .text_right()
                        .typeset(theme::CAPTION),
                ),
        )
}

/// What can be done with `user`'s stream here, as menu rows.
fn stream_actions(app: &ThroccApp, user: UserId, mine: bool, cx: &mut Context<ThroccApp>) -> Div {
    let rows = group();
    if !app.shows_stream(user) {
        let (id, icon_name, label) = if app.popouts.contains_key(&user) {
            ("person-menu-bring-back", "panel-top-open", "Bring Back")
        } else if mine {
            ("person-menu-watch", "eye", "Preview")
        } else {
            ("person-menu-watch", "eye", "Watch Stream")
        };
        return rows.child(action(id, icon_name, label).on_click(cx.listener(
            move |app, _, _, cx| {
                app.close_person_menu(cx);
                app.watch(user, cx);
            },
        )));
    }
    rows.children((!mine).then(|| {
        action("person-menu-pop-out", "picture-in-picture-2", "Pop Out").on_click(cx.listener(
            move |app, _, window, cx| {
                app.close_person_menu(cx);
                if app.fullscreen_stream() == Some(user) {
                    app.exit_fullscreen(window, cx);
                }
                app.pop_out(user, CONTROLS_LINGER, cx);
            },
        ))
    }))
    .children((app.fullscreen_stream() != Some(user)).then(|| {
        action("person-menu-fullscreen", "maximize-2", "Full Screen").on_click(cx.listener(
            move |app, _, window, cx| {
                app.close_person_menu(cx);
                app.enter_fullscreen(user, window, cx);
            },
        ))
    }))
    .child(
        action(
            "person-menu-stop-watching",
            "eye-off",
            if mine {
                "Stop Preview"
            } else {
                "Stop Watching"
            },
        )
        .on_click(cx.listener(move |app, _, window, cx| {
            app.close_person_menu(cx);
            app.stop_watching(user, window, cx);
        })),
    )
}

/// Kicking `user` from the room, and removing them from the server outside pop-outs, where
/// our rank allows.
fn moderation(
    app: &ThroccApp,
    user: UserId,
    person: &Person,
    in_popout: bool,
    cx: &mut Context<ThroccApp>,
) -> Option<Div> {
    let me = app.state.role?;
    let other = Some(user) != app.state.me;
    let kick_room = person
        .room
        .filter(|_| other && permissions::can_kick_from_room(me, person.role));
    let kick = kick_room.is_some();
    let remove = other && permissions::can_remove(me, person.role) && !in_popout;
    if !kick && !remove {
        return None;
    }
    let name = person.name.clone();
    Some(
        group()
            .children(kick.then(|| {
                danger_action("person-menu-kick", "door-open", "Kick from Room").on_click(
                    cx.listener(move |app, _, _, cx| {
                        app.close_person_menu(cx);
                        if let Some(room) = kick_room {
                            app.command(Command::KickFromRoom { user, room });
                        }
                    }),
                )
            }))
            .children(remove.then(|| {
                danger_action("person-menu-remove", "user-x", "Remove from Server").on_click(
                    cx.listener(move |app, _, _, cx| {
                        app.close_person_menu(cx);
                        let dialog = Dialog::Remove {
                            user,
                            name: name.clone(),
                            cascade: false,
                        };
                        app.set_dialog(Some(dialog), cx);
                    }),
                )
            })),
    )
}

/// Menu rows set apart from those above by a rule.
fn group() -> Div {
    div()
        .flex()
        .flex_col()
        .pt_2()
        .mt_1()
        .border_t_1()
        .border_color(theme::control())
}

fn action(id: &'static str, icon_name: &'static str, label: &'static str) -> Stateful<Div> {
    row(id, icon(icon_name).text_color(theme::text_2()), label)
}

fn danger_action(id: &'static str, icon_name: &'static str, label: &'static str) -> Stateful<Div> {
    row(id, icon(icon_name).text_color(theme::danger_text()), label)
        .text_color(theme::danger_text())
}

fn row(id: &'static str, icon: Icon, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(32.))
        .rounded(theme::RADIUS_ROW)
        .cursor_pointer()
        .hover(|s| s.bg(theme::raised()))
        .child(icon.size(px(16.)))
        .child(label)
}
