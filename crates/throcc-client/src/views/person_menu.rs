use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Stateful, StatefulInteractiveElement, Styled, anchored, div, px,
};
use gpui_component::slider::Slider;
use throcc_client_core::client::Command;
use throcc_client_core::state::Sound;
use throcc_proto::ids::UserId;

use super::common::{gain_to_loudness, icon, mono, section};
use super::main::toggle_icon;
use crate::app::{PlaybackControl, ThroccApp};
use crate::theme;

/// The person menu at the pointer, over a backdrop that closes it.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let Some(menu) = &app.person_menu else {
        return div().into_any_element();
    };
    let user = menu.user;
    let position = menu.position;
    let Some(person) = app.state.person(user).cloned() else {
        return div().into_any_element();
    };
    let mine = Some(user) == app.state.me;
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
                .font_weight(FontWeight::SEMIBOLD)
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
        .children(
            person
                .sharing
                .is_some()
                .then(|| stream_actions(app, user, mine, cx)),
        );
    div()
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
        .into_any_element()
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
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section(label))
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
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().flex_1().child(Slider::new(&control.slider)))
                .child(
                    mono(format!("{percent:.0}%"))
                        .w(px(44.))
                        .flex_none()
                        .text_right()
                        .text_size(px(12.)),
                ),
        )
}

/// What can be done with `user`'s stream here, as menu rows.
fn stream_actions(app: &ThroccApp, user: UserId, mine: bool, cx: &mut Context<ThroccApp>) -> Div {
    let rows = div()
        .flex()
        .flex_col()
        .pt_1()
        .mt_1()
        .border_t_1()
        .border_color(theme::control());
    if app.popouts.contains_key(&user) {
        return rows.child(
            action("person-menu-bring-back", "panel-top-open", "Bring back").on_click(cx.listener(
                move |app, _, _, cx| {
                    app.close_person_menu(cx);
                    app.close_popout(user, cx);
                },
            )),
        );
    }
    if !app.shows_stream(user) {
        let label = if mine { "Preview" } else { "Watch stream" };
        return rows.child(
            action("person-menu-watch", "eye", label).on_click(cx.listener(
                move |app, _, _, cx| {
                    app.close_person_menu(cx);
                    if mine {
                        app.preview_self = true;
                    } else {
                        app.command(Command::Watch(user));
                    }
                },
            )),
        );
    }
    rows.children((!mine).then(|| {
        action("person-menu-pop-out", "picture-in-picture-2", "Pop out").on_click(cx.listener(
            move |app, _, _, cx| {
                app.close_person_menu(cx);
                app.pop_out(user, cx);
            },
        ))
    }))
    .child(
        action("person-menu-fullscreen", "maximize-2", "Fullscreen").on_click(cx.listener(
            move |app, _, window, cx| {
                app.close_person_menu(cx);
                app.enter_fullscreen(user, window, cx);
            },
        )),
    )
    .child(
        action(
            "person-menu-stop-watching",
            "eye-off",
            if mine {
                "Stop preview"
            } else {
                "Stop watching"
            },
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.close_person_menu(cx);
            app.stop_watching(user);
        })),
    )
}

fn action(id: &'static str, icon_name: &'static str, label: &'static str) -> Stateful<Div> {
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
        .child(icon(icon_name).size(px(16.)).text_color(theme::text_2()))
        .child(label)
}
