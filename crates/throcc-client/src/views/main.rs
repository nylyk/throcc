use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::{Disableable, Sizable, Size};
use throcc_client_core::client::Command;
use throcc_client_core::state::{Person, Preset, State, share_label};
use throcc_proto::ids::{RoomId, UserId};

use super::common::{
    avatar, caption, card, chip, control_button, ghost_icon, icon, live_badge, primary, secondary,
    section, title,
};
use crate::app::{Dialog, Screen, SettingsPage, ThroccApp};
use crate::{theme, video};

pub fn render(
    app: &mut ThroccApp,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let content = match app.state.current.clone() {
        Some(current) => room(app, current.room, window, cx).into_any_element(),
        None => lobby(app, cx).into_any_element(),
    };
    div()
        .size_full()
        .flex()
        .child(sidebar(app, cx))
        .child(div().flex_1().min_w_0().h_full().child(content))
}

fn people_in(state: &State, room: RoomId) -> Vec<(UserId, Person)> {
    state
        .occupants(room)
        .map(|(u, p)| (*u, p.clone()))
        .collect()
}

fn sidebar(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let pictures = app.pictures_of_people();
    let state = app.state.clone();
    let server = state
        .server
        .as_ref()
        .map_or("server".to_owned(), |s| s.address.clone());
    let current_room = state.current.as_ref().map(|c| c.room);
    let manages = app.manages_rooms();
    let rooms = state.rooms.iter().map(|room| {
        let people = people_in(&state, room.id);
        let sharing = people.iter().any(|(_, p)| p.sharing.is_some());
        let selected = current_room == Some(room.id);
        let id = room.id;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("room", room.id.0 as usize))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .min_h(px(36.))
                    .rounded(theme::RADIUS_ROW)
                    .cursor_pointer()
                    .when_selected(selected)
                    .hover(|s| s.bg(theme::raised()))
                    .child(icon("volume-2").text_color(theme::text_3()).size(px(14.)))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(room.name.clone()),
                    )
                    .children(sharing.then(live_badge))
                    .children((!people.is_empty()).then(|| caption(people.len().to_string())))
                    .children(manages.then(|| {
                        ghost_icon(("edit-room", id.0 as usize), "pencil", "Edit room")
                            .with_size(Size::Small)
                            .on_click(cx.listener(move |app, _, window, cx| {
                                cx.stop_propagation();
                                app.open_edit_room(id, window, cx);
                            }))
                    }))
                    .on_click(
                        cx.listener(move |app, _, _, _| app.command(Command::Join(Some(id)))),
                    ),
            )
            .children(
                people
                    .into_iter()
                    .map(|(user, person)| occupant(user, &person, pictures.get(&user).cloned())),
            )
    });

    let me = state.me;
    let my_name = me
        .and_then(|m| state.person(m).map(|p| p.name.clone()))
        .unwrap_or_else(|| "You".into());
    let mic = state.current.as_ref().is_some_and(|c| c.mic);
    let deafened = state.current.as_ref().is_some_and(|c| c.deafened);
    let in_room = state.current.is_some();

    div()
        .w(theme::SIDEBAR_WIDTH)
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(theme::sidebar())
        .border_r_1()
        .border_color(theme::control())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h(theme::HEADER_HEIGHT)
                .flex_none()
                .border_b_1()
                .border_color(theme::control())
                .child(avatar(&server, 0, None, px(28.), false))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(server),
                )
                .child(
                    ghost_icon("leave-server", "log-out", "Leave this server")
                        .on_click(cx.listener(|app, _, _, cx| app.leave_server(cx))),
                ),
        )
        .child(
            div()
                .id("rooms")
                .flex_1()
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pl_2()
                        .child(section("Rooms"))
                        .children(app.manages_rooms().then(|| {
                            ghost_icon("new-room", "plus", "New room").on_click(
                                cx.listener(|app, _, window, cx| app.open_new_room(window, cx)),
                            )
                        })),
                )
                .children(rooms)
                .children(
                    state
                        .rooms
                        .is_empty()
                        .then(|| div().px_2().child(caption("No rooms yet."))),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h(theme::FOOTER_HEIGHT)
                .flex_none()
                .border_t_1()
                .border_color(theme::control())
                .child(avatar(
                    &my_name,
                    me.map_or(0, |m| m.0),
                    me.and_then(|m| pictures.get(&m).cloned()),
                    px(28.),
                    me.is_some_and(|m| state.person(m).is_some_and(|p| p.speaking)),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(my_name),
                        )
                        .child(caption(if in_room { "Connected" } else { "In the lobby" })),
                )
                .child(
                    ghost_icon(
                        "profile-mic",
                        if mic { "mic" } else { "mic-off" },
                        "Mute (⌘⇧M)",
                    )
                    .disabled(!in_room)
                    .on_click(cx.listener(move |app, _, _, _| app.command(Command::SetMic(!mic)))),
                )
                .child(
                    ghost_icon(
                        "profile-deafen",
                        if deafened {
                            "headphone-off"
                        } else {
                            "headphones"
                        },
                        "Deafen (⌘⇧D)",
                    )
                    .disabled(!in_room)
                    .on_click(
                        cx.listener(move |app, _, _, _| {
                            app.command(Command::SetDeafened(!deafened))
                        }),
                    ),
                )
                .child(
                    ghost_icon("profile-settings", "settings", "Settings").on_click(cx.listener(
                        |app, _, _, cx| {
                            app.screen = Screen::Settings(SettingsPage::Audio);
                            cx.notify();
                        },
                    )),
                ),
        )
}

trait Selected {
    fn when_selected(self, selected: bool) -> Self;
}

impl<T: Styled> Selected for T {
    fn when_selected(self, selected: bool) -> Self {
        if selected {
            self.bg(theme::raised())
        } else {
            self
        }
    }
}

fn occupant(
    user: UserId,
    person: &Person,
    picture: Option<std::sync::Arc<gpui::RenderImage>>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .pl(px(28.))
        .pr_2()
        .min_h(px(30.))
        .child(avatar(
            &person.name,
            user.0,
            picture,
            px(20.),
            person.speaking,
        ))
        .child(
            div()
                .flex_1()
                .truncate()
                .text_color(theme::text_2_muted())
                .text_size(px(13.))
                .child(person.name.clone()),
        )
        .children(person.sharing.is_some().then(live_badge))
        .children((!person.mic).then(|| icon("mic-off").size(px(14.)).text_color(theme::text_3())))
        .children(person.deafened.then(|| {
            icon("headphone-off")
                .size(px(14.))
                .text_color(theme::text_3())
        }))
}

fn lobby(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let pictures = app.pictures_of_people();
    let manages = app.manages_rooms();
    let state = app.state.clone();
    let cards = state.rooms.iter().map(|room| {
        let people = people_in(&state, room.id);
        let sharing = people.iter().filter(|(_, p)| p.sharing.is_some()).count();
        let id = room.id;
        card()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(280.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(16.))
                            .child(room.name.clone()),
                    )
                    .child(caption(people.len().to_string())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_h(px(32.))
                    .children(people.iter().take(6).map(|(u, p)| {
                        avatar(&p.name, u.0, pictures.get(u).cloned(), px(24.), p.speaking)
                    }))
                    .children(people.is_empty().then(|| caption("Empty")))
                    .children((sharing > 0).then(|| {
                        chip(
                            format!("{sharing} sharing"),
                            theme::danger_bg(),
                            theme::danger_text(),
                        )
                    })),
            )
            .child(
                primary(("join-room", id.0 as usize), "Join")
                    .w_full()
                    .on_click(
                        cx.listener(move |app, _, _, _| app.command(Command::Join(Some(id)))),
                    ),
            )
    });
    div()
        .id("lobby")
        .size_full()
        .overflow_y_scroll()
        .p_8()
        .flex()
        .flex_col()
        .gap_4()
        .child(title("Lobby"))
        .child(caption(
            "You're connected but not in a room. Nobody sees or hears you until you join one.",
        ))
        .child(div().flex().flex_wrap().gap_4().children(cards))
        .children(state.rooms.is_empty().then(|| {
            card().child(caption(if manages {
                "There are no rooms yet. Create one with + next to Rooms."
            } else {
                "There are no rooms yet. A manager or admin can create one."
            }))
        }))
}

fn room(
    app: &mut ThroccApp,
    room: RoomId,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let pictures = app.pictures_of_people();
    let state = app.state.clone();
    let name = state.room(room).map_or("Room".into(), |r| r.name.clone());
    let people = people_in(&state, room);
    let me = state.me;
    let current = state.current.clone();
    let watching: Vec<UserId> = current
        .as_ref()
        .map(|c| c.watching.clone())
        .unwrap_or_default();
    let sharers: Vec<(UserId, Person)> = people
        .iter()
        .filter(|(u, p)| p.sharing.is_some() && Some(*u) != me)
        .cloned()
        .collect();
    let staged = sharers.iter().find(|(u, _)| watching.contains(u)).cloned();
    let rtt = state.health.rtt_ms;

    let header = div()
        .flex()
        .items_center()
        .gap_3()
        .px_6()
        .h(theme::HEADER_HEIGHT)
        .flex_none()
        .border_b_1()
        .border_color(theme::control())
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(16.))
                .child(name),
        )
        .child(caption(format!("{} people", people.len())))
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    icon("shield-check")
                        .size(px(14.))
                        .text_color(theme::ok_text()),
                )
                .child(chip(
                    "End-to-end encrypted",
                    theme::ok_bg(),
                    theme::ok_text(),
                )),
        )
        .child(
            Button::new("rtt")
                .label(format!("{rtt:.0} ms"))
                .icon(icon("activity"))
                .ghost()
                .tooltip("Connection")
                .on_click(cx.listener(|app, _, _, cx| {
                    app.dialog = Some(Dialog::Connection);
                    cx.notify();
                })),
        );

    let stage = match &staged {
        Some((user, person)) => {
            let user = *user;
            let config = person.sharing.map(|c| share_label(&c)).unwrap_or_default();
            div()
                .flex_1()
                .min_h(px(240.))
                .relative()
                .rounded(theme::RADIUS_TILE)
                .bg(gpui::black())
                .overflow_hidden()
                .child(video::surface(app, user, window, cx))
                .child(
                    div()
                        .absolute()
                        .top_3()
                        .left_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(theme::RADIUS_ROW)
                        .bg(theme::surface().opacity(0.85))
                        .child(avatar(
                            &person.name,
                            user.0,
                            pictures.get(&user).cloned(),
                            px(18.),
                            person.speaking,
                        ))
                        .child(
                            div()
                                .text_size(px(13.))
                                .child(format!("{}'s screen", person.name)),
                        )
                        .child(caption(config)),
                )
                .child(
                    div()
                        .absolute()
                        .top_3()
                        .right_3()
                        .flex()
                        .gap_1()
                        .child(
                            ghost_icon("stop-watching", "eye-off", "Stop watching").on_click(
                                cx.listener(move |app, _, _, _| {
                                    app.command(Command::Unwatch(user))
                                }),
                            ),
                        )
                        .child(
                            ghost_icon("fullscreen", "maximize-2", "Fullscreen").on_click(
                                cx.listener(|_, _, window, _| window.toggle_fullscreen()),
                            ),
                        ),
                )
                .into_any_element()
        }
        None => div()
            .flex_1()
            .min_h(px(160.))
            .rounded(theme::RADIUS_TILE)
            .border_1()
            .border_color(theme::control())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                icon("screen-share")
                    .size(px(28.))
                    .text_color(theme::text_3()),
            )
            .child(caption(if sharers.is_empty() {
                "Nobody is sharing. Share your screen from the bar below."
            } else {
                "Pick a share on the right to watch it."
            }))
            .into_any_element(),
    };

    let others: Vec<(UserId, Person)> = sharers
        .iter()
        .filter(|(u, _)| Some(*u) != staged.as_ref().map(|s| s.0))
        .cloned()
        .collect();
    let rail = (!others.is_empty()).then(|| {
        div()
            .w(px(240.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .children(others.iter().map(|(user, person)| {
                let user = *user;
                card()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(avatar(
                                &person.name,
                                user.0,
                                pictures.get(&user).cloned(),
                                px(22.),
                                person.speaking,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .child(format!("{} is sharing", person.name)),
                            )
                            .child(live_badge()),
                    )
                    .child(caption(
                        person.sharing.map(|c| share_label(&c)).unwrap_or_default(),
                    ))
                    .child(
                        secondary(("watch", user.0 as usize), "Watch")
                            .w_full()
                            .on_click(
                                cx.listener(move |app, _, _, _| app.command(Command::Watch(user))),
                            ),
                    )
            }))
    });

    let tiles = div()
        .flex()
        .flex_wrap()
        .gap_3()
        .children(people.iter().map(|(user, person)| {
            div()
                .w(px(148.))
                .h(px(112.))
                .rounded(theme::RADIUS_TILE)
                .bg(theme::surface())
                .border_1()
                .border_color(if person.speaking {
                    theme::speaking()
                } else {
                    theme::control()
                })
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(avatar(
                    &person.name,
                    user.0,
                    pictures.get(user).cloned(),
                    px(40.),
                    person.speaking,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(13.))
                                .truncate()
                                .child(person.name.clone()),
                        )
                        .children(
                            (!person.mic)
                                .then(|| icon("mic-off").size(px(13.)).text_color(theme::text_3())),
                        )
                        .children(person.deafened.then(|| {
                            icon("headphone-off")
                                .size(px(13.))
                                .text_color(theme::text_3())
                        })),
                )
        }));

    let mic = current.as_ref().is_some_and(|c| c.mic);
    let deafened = current.as_ref().is_some_and(|c| c.deafened);
    let sharing = current.as_ref().is_some_and(|c| c.sharing.is_some());
    let controls = div()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .px_3()
        .h(theme::FOOTER_HEIGHT)
        .flex_none()
        .border_t_1()
        .border_color(theme::control())
        .child(
            control_button(
                "mic",
                if mic { "mic" } else { "mic-off" },
                "Microphone (⌘⇧M)",
                mic,
            )
            .on_click(cx.listener(move |app, _, _, _| app.command(Command::SetMic(!mic)))),
        )
        .child(
            control_button(
                "deafen",
                if deafened {
                    "headphone-off"
                } else {
                    "headphones"
                },
                "Deafen (⌘⇧D)",
                !deafened,
            )
            .on_click(
                cx.listener(move |app, _, _, _| app.command(Command::SetDeafened(!deafened))),
            ),
        )
        .child(
            Button::new("share")
                .label(if sharing {
                    "Stop sharing"
                } else {
                    "Share screen"
                })
                .icon(icon(if sharing {
                    "screen-share-off"
                } else {
                    "monitor-up"
                }))
                .with_variant(if sharing {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Secondary
                })
                .rounded(theme::RADIUS_BUTTON)
                .h(theme::TOUCH)
                .on_click(cx.listener(move |app, _, _, cx| {
                    if sharing {
                        app.command(Command::StopShare);
                    } else {
                        app.dialog = Some(Dialog::Share {
                            preset: Preset::Default,
                            audio: true,
                        });
                    }
                    cx.notify();
                })),
        )
        .child(
            control_button("settings", "settings", "Settings", true).on_click(cx.listener(
                |app, _, _, cx| {
                    app.screen = Screen::Settings(SettingsPage::Audio);
                    cx.notify();
                },
            )),
        )
        .child(
            Button::new("leave")
                .label("Leave")
                .icon(icon("log-out"))
                .danger()
                .rounded(theme::RADIUS_BUTTON)
                .h(theme::TOUCH)
                .on_click(cx.listener(|app, _, _, _| app.command(Command::Join(None)))),
        );

    let body: AnyElement = div()
        .flex_1()
        .min_h_0()
        .flex()
        .gap_4()
        .p_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_4()
                .child(stage)
                .child(tiles),
        )
        .children(rail)
        .into_any_element();

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(body)
        .child(controls)
}
