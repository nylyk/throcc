use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, Div, FontFeatures, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, RenderImage, Size as Area, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, px, size,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Sizable, Size};
use throcc_client_core::client::UiCommand;
use throcc_client_core::state::{Person, State, share_label};
use throcc_proto::ids::{RoomId, UserId};

use super::common::{
    CONTROLS_LINGER, TRAFFIC_LIGHTS, Trust, Typeset, audience, avatar, bottom_bar, caption,
    danger_menu_item, description, ghost_icon, icon, primary, rate, section, sharing_icon,
    toggle_icon, top_bar, trust,
};
use super::mascot;
use super::overlay::ShareOverlay;
use super::page::{resize_handle, sidebar_column};
use crate::app::{Dialog, Screen, ServerPage, ThroccApp, ToggleDeafen, ToggleMic};
use crate::theme;

/// Decoded profile pictures by person.
type Pictures = HashMap<UserId, Arc<RenderImage>>;

pub fn render(
    app: &mut ThroccApp,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    if let Some(user) = app.fullscreen_stream() {
        return fullscreen(app, user, window, cx).into_any_element();
    }
    let pictures = app.pictures_of_people(cx);
    let content = match app.state.current.as_ref().map(|c| c.room) {
        Some(current) => room(app, current, &pictures, window, cx).into_any_element(),
        None => lobby(app, cx).into_any_element(),
    };
    div()
        .size_full()
        .flex()
        .child(sidebar(app, &pictures, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(theme::bg())
                .child(content),
        )
        .into_any_element()
}

/// One share filling the window. Its controls show while the pointer moves.
fn fullscreen(
    app: &mut ThroccApp,
    user: UserId,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let state = app.state.clone();
    let person = state.person(user);
    let picture = app.picture(person.and_then(|p| p.avatar), cx);
    let buttons = vec![
        ghost_icon("fullscreen-stop-watching", "eye-off", "Stop Watching")
            .on_click(cx.listener(move |app, _, window, cx| app.stop_watching(user, window, cx))),
        ghost_icon("exit-fullscreen", "minimize-2", "Exit Full Screen")
            .on_click(cx.listener(|app, _, window, cx| app.exit_fullscreen(window, cx))),
    ];
    let overlay = ShareOverlay {
        name: person.map_or("Someone", |p| &p.name).to_owned().into(),
        seed: user.0,
        picture,
        details: person
            .and_then(|p| p.sharing)
            .map(|c| share_label(&c))
            .unwrap_or_default()
            .into(),
        width: window.viewport_size().width,
        title_inset: if window.is_fullscreen() {
            px(0.)
        } else {
            TRAFFIC_LIGHTS
        },
        buttons,
        audience: person.map_or(0, |p| p.audience),
        controls: std::time::Instant::now() < app.controls_until,
        too_slow: state.health.slow_shares.contains(&user),
        fullscreen: true,
    };
    let stream = app
        .feeds
        .surface(app.client.as_ref(), user, px(0.), window, cx);
    div()
        .id("fullscreen")
        .size_full()
        .relative()
        .bg(theme::player_bg())
        .on_mouse_move(cx.listener(|app, _, _, cx| app.show_controls(cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                app.open_person_menu(user, event.position, window, cx)
            }),
        )
        .child(stream)
        .children(overlay.render())
}

fn people_in(state: &State, room: RoomId) -> Vec<(UserId, &Person)> {
    state.occupants(room).map(|(u, p)| (*u, p)).collect()
}

fn sidebar(
    app: &mut ThroccApp,
    pictures: &Pictures,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let state = app.state.clone();
    let server_address = state.server.as_ref().map_or("", |s| s.address.as_str());
    let server_title = state.server.as_ref().map_or("", |s| s.title());
    let named = state.server.as_ref().is_some_and(|s| s.name.is_some());
    let (my_name, my_seed, my_picture) = app.own_look(cx);
    let current_room = state.current.as_ref().map(|c| c.room);
    let manages = app.manages_rooms();
    let rooms = state.rooms.iter().map(|room| {
        let people = people_in(&state, room.id);
        let sharing = people.iter().any(|(_, p)| p.sharing.is_some());
        let selected = current_room == Some(room.id);
        let id = room.id;
        let row = div()
            .id(("room", room.id.0 as usize))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .min_h(px(36.))
            .rounded(theme::RADIUS_ROW)
            .cursor_pointer()
            .when(selected, |s| s.bg(theme::selected()))
            .hover(|s| s.bg(theme::raised()))
            .child(room_marker(room.locked, selected))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(room.name.clone()),
            )
            .children((!people.is_empty()).then(|| caption(people.len().to_string())))
            .child(div().flex_1())
            .children(sharing.then(|| sharing_icon(px(16.))))
            .on_click(cx.listener(move |app, _, window, cx| app.join_room(id, window, cx)));
        div()
            .flex()
            .flex_col()
            .child(with_room_menu(row, id, selected, manages, cx))
            .children(people.into_iter().map(|(user, person)| {
                let trust = trust(&state, user);
                occupant(user, person, pictures.get(&user).cloned(), trust, cx)
            }))
    });

    let (mic, deafened) = (state.mic, state.deafened);
    let in_room = state.current.is_some();

    // The dropdown wraps its trigger in elements that grow to the text, so the name gets an
    // explicit width: the sidebar minus the paddings, gap and caret.
    let server_name_width = app.sidebar_width - px(44.);
    let this = cx.entity().downgrade();
    let server_menu = Button::new("server-menu")
        .ghost()
        .rounded(theme::RADIUS_ROW)
        .h(px(40.))
        .px_2()
        .dropdown_caret(true)
        .child(
            div()
                .max_w(server_name_width)
                .flex()
                .items_center()
                .gap_1p5()
                .typeset(theme::TITLE_3)
                .child(
                    div()
                        .flex_none()
                        .max_w(server_name_width)
                        .truncate()
                        .child(server_title.to_owned()),
                )
                .children(named.then(|| {
                    div()
                        .min_w_0()
                        .truncate()
                        .typeset(theme::CALLOUT)
                        .text_color(theme::text_3())
                        .child(format!("({server_address})"))
                })),
        )
        .dropdown_menu(move |menu, _, _| {
            let settings = this.clone();
            let invite = this.clone();
            let leave = this.clone();
            menu.min_w(px(200.))
                .item(
                    PopupMenuItem::new("Server Settings")
                        .icon(icon("settings-2"))
                        .on_click(move |_, window, cx| {
                            settings
                                .update(cx, |app, cx| {
                                    let overview = Screen::Server(ServerPage::Overview);
                                    app.show_screen(overview, window, cx);
                                })
                                .ok();
                        }),
                )
                .item(
                    PopupMenuItem::new("Invite People")
                        .icon(icon("user-plus"))
                        .on_click(move |_, window, cx| {
                            invite
                                .update(cx, |app, cx| {
                                    let members = Screen::Server(ServerPage::Members);
                                    app.show_screen(members, window, cx);
                                })
                                .ok();
                        }),
                )
                .separator()
                .item(
                    danger_menu_item("Leave Server", "log-out").on_click(move |_, _, cx| {
                        leave
                            .update(cx, |app, cx| app.set_dialog(Some(Dialog::LeaveServer), cx))
                            .ok();
                    }),
                )
        });

    let speaking = state
        .me
        .and_then(|m| state.person(m))
        .is_some_and(|p| p.speaking);
    let profile = div()
        .id("own-profile")
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .cursor_pointer()
        .on_click(cx.listener(|app, _, _, cx| app.set_dialog(Some(Dialog::OwnProfile), cx)))
        .child(avatar(&my_name, my_seed, my_picture, px(36.), speaking))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(my_name),
                )
                .child(caption(if in_room { "Connected" } else { "In the lobby" }).truncate()),
        );

    sidebar_column(app.sidebar_width)
        .child(top_bar().px_2().child(server_menu))
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
                        .children(manages.then(|| {
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
            bottom_bar().child(profile).child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        toggle_icon("profile-mic", "mic", "mic-off", !mic, "Mute")
                            .tooltip_with_action("Mute", &ToggleMic, None)
                            .on_click(cx.listener(move |app, _, _, _| {
                                app.command(UiCommand::SetMic(!mic))
                            })),
                    )
                    .child(
                        toggle_icon(
                            "profile-deafen",
                            "headphones",
                            "headphone-off",
                            deafened,
                            "Deafen",
                        )
                        .tooltip_with_action("Deafen", &ToggleDeafen, None)
                        .on_click(cx.listener(move |app, _, _, _| {
                            app.command(UiCommand::SetDeafened(!deafened))
                        })),
                    ),
            ),
        )
        .child(resize_handle(cx))
}

/// `element` with a right-click menu to leave `room` while `joined`, and to edit it for
/// those who manage rooms.
fn with_room_menu(
    element: Stateful<Div>,
    room: RoomId,
    joined: bool,
    manages: bool,
    cx: &Context<ThroccApp>,
) -> AnyElement {
    if !joined && !manages {
        return element.into_any_element();
    }
    let this = cx.entity().downgrade();
    element
        .context_menu(move |menu, _, _| {
            let (leave, edit, delete) = (this.clone(), this.clone(), this.clone());
            menu.when(joined, |menu| {
                menu.item(
                    PopupMenuItem::new("Leave Room")
                        .icon(icon("phone-off"))
                        .on_click(move |_, _, cx| {
                            leave
                                .update(cx, |app, _| app.command(UiCommand::Join(None)))
                                .ok();
                        }),
                )
            })
            .when(joined && manages, |menu| menu.separator())
            .when(manages, |menu| {
                menu.item(
                    PopupMenuItem::new("Edit Room…")
                        .icon(icon("pencil"))
                        .on_click(move |_, window, cx| {
                            edit.update(cx, |app, cx| app.open_edit_room(room, window, cx))
                                .ok();
                        }),
                )
                .separator()
                .item(
                    danger_menu_item("Delete Room", "trash").on_click(move |_, _, cx| {
                        delete
                            .update(cx, |app, cx| {
                                app.set_dialog(Some(Dialog::DeleteRoom(room)), cx)
                            })
                            .ok();
                    }),
                )
            })
        })
        .into_any_element()
}

fn occupant(
    user: UserId,
    person: &Person,
    picture: Option<std::sync::Arc<gpui::RenderImage>>,
    trust: Trust,
    cx: &Context<ThroccApp>,
) -> Stateful<Div> {
    div()
        .id(("occupant", user.0 as usize))
        .flex()
        .items_center()
        .gap_2p5()
        .pl(px(24.))
        .pr_2()
        .min_h(px(36.))
        .rounded(theme::RADIUS_ROW)
        .cursor_pointer()
        .hover(|s| s.bg(theme::raised()))
        .on_click(cx.listener(move |app, _, _, cx| app.open_profile(user, cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                app.open_person_menu(user, event.position, window, cx)
            }),
        )
        .child(avatar(
            &person.name,
            user.0,
            picture,
            px(26.),
            person.speaking,
        ))
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_2_muted())
                        .child(person.name.clone()),
                )
                .children(trust_marker(user, trust)),
        )
        .child(div().flex_1())
        .children(person.sharing.is_some().then(|| sharing_icon(px(16.))))
        .children(muted_icon(person, px(16.)))
}

/// The sign beside the name of someone not verified, or whose verified key changed: a danger
/// sign for a changed key, a quiet one otherwise.
fn trust_marker(user: UserId, trust: Trust) -> Option<Stateful<Div>> {
    let (name, size, color, label) = match trust {
        Trust::KeyChanged => ("triangle-alert", 16., theme::danger_text(), "Key changed"),
        Trust::Unverified => ("shield-question-mark", 14., theme::text_3(), "Not verified"),
        Trust::Verified => return None,
    };
    let marker = div()
        .id(("trust", user.0 as usize))
        .flex()
        .flex_none()
        .child(icon(name).size(px(size)).text_color(color))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx));
    Some(marker)
}

/// The headphones-off icon while `person` is deafened, which mutes too, or else the
/// mic-off icon while muted.
fn muted_icon(person: &Person, size: Pixels) -> Option<Icon> {
    let name = if person.deafened {
        "headphone-off"
    } else if !person.mic {
        "mic-off"
    } else {
        return None;
    };
    Some(icon(name).size(size).text_color(theme::text_3()))
}

fn lobby(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let no_rooms = app.state.rooms.is_empty();
    let header = top_bar()
        .px_6()
        .child(div().typeset(theme::TITLE_3).child("Lobby"));
    let body = div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .child(mascot::drawing(app.mascot.pick(mascot::IDLE)))
        .child(description(if no_rooms {
            "There are no rooms yet."
        } else {
            "Join a room on the left."
        }))
        .children((no_rooms && app.manages_rooms()).then(|| {
            primary("first-room", "New Room")
                .h_10()
                .px_4()
                .on_click(cx.listener(|app, _, window, cx| app.open_new_room(window, cx)))
        }));
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(body)
}

fn room(
    app: &mut ThroccApp,
    room: RoomId,
    pictures: &Pictures,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let state = app.state.clone();
    let name = state.room(room).map_or("Room".into(), |r| r.name.clone());
    let tiles: Vec<Tile> = people_in(&state, room)
        .into_iter()
        .map(|(user, person)| Tile {
            user,
            person,
            picture: pictures.get(&user).cloned(),
            mine: Some(user) == state.me,
            shows_stream: app.shows_stream(user),
            popped: app.popouts.contains_key(&user),
        })
        .collect();
    let rtt = state.health.rtt_ms;

    let header = top_bar()
        .gap_3()
        .px_6()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .typeset(theme::TITLE_3)
                .child(name),
        )
        .child(
            Button::new("connection")
                .flex_none()
                .label(format!(
                    "{rtt:.0} ms  ↓ {}  ↑ {}",
                    rate(state.health.download_kbps),
                    rate(state.health.upload_kbps)
                ))
                .icon(icon("activity"))
                .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
                .ghost()
                .tooltip("Connection")
                .on_click(
                    cx.listener(|app, _, _, cx| app.set_dialog(Some(Dialog::Connection), cx)),
                ),
        );

    let sharing = state.current.as_ref().is_some_and(|c| c.sharing.is_some());
    let controls = bottom_bar()
        .justify_center()
        .child(
            Button::new("share")
                .child(
                    icon(if sharing {
                        "screen-share-off"
                    } else {
                        "monitor-up"
                    })
                    .size(px(16.)),
                )
                .tooltip(if sharing {
                    "Stop sharing"
                } else {
                    "Share screen"
                })
                .with_variant(if sharing {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Secondary
                })
                .rounded(theme::RADIUS_CALL_BUTTON)
                .w(theme::TOUCH)
                .h(theme::TOUCH)
                .px_0()
                .on_click(cx.listener(move |app, _, _, cx| {
                    if sharing {
                        app.command(UiCommand::StopShare);
                        cx.notify();
                    } else {
                        app.set_dialog(Some(Dialog::Share), cx);
                    }
                })),
        )
        .child(
            Button::new("leave")
                .child(icon("phone-off").size(px(16.)))
                .tooltip("Leave the room")
                .danger()
                .rounded(theme::RADIUS_CALL_BUTTON)
                .w(theme::TOUCH)
                .h(theme::TOUCH)
                .px_0()
                .on_click(cx.listener(|app, _, _, _| app.command(UiCommand::Join(None)))),
        );

    let focus = app
        .focus
        .filter(|f| tiles.iter().any(|t| t.user == *f && t.shows_stream));
    let viewport = window.viewport_size();
    let area = room_area(size(
        viewport.width - app.sidebar_width - app.room_chrome.width,
        viewport.height - app.room_chrome.height,
    ));
    let alone = !sharing && tiles.iter().all(|t| t.mine);
    let body = if alone {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(mascot::drawing(app.mascot.pick(mascot::LONELY)))
            .child(description("No one else is here yet :("))
            .into_any_element()
    } else {
        match focus {
            Some(focus) => focused(app, &tiles, focus, area, window, cx),
            None => grid(app, &tiles, area, window, cx),
        }
    };
    let this = cx.entity().downgrade();
    let measure = canvas(
        move |bounds, window, cx| {
            this.update(cx, |app, cx| {
                let viewport = window.viewport_size();
                let chrome = size(
                    viewport.width - app.sidebar_width - bounds.size.width,
                    viewport.height - bounds.size.height,
                );
                if app.room_chrome != chrome {
                    app.room_chrome = chrome;
                    cx.notify();
                }
            })
            .ok();
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full();

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(measure)
                .child(body),
        )
        .child(controls)
}

/// The dot before a room's name in the sidebar, or a lock when it needs a password.
fn room_marker(locked: bool, selected: bool) -> AnyElement {
    let color = if selected {
        theme::accent()
    } else {
        theme::text_3()
    };
    if locked {
        return icon("lock")
            .size(px(12.))
            .flex_none()
            .text_color(color)
            .into_any_element();
    }
    div()
        .size(px(6.))
        .flex_none()
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// One person in the room, and what their tile shows.
struct Tile<'a> {
    user: UserId,
    person: &'a Person,
    picture: Option<Arc<RenderImage>>,
    mine: bool,
    shows_stream: bool,
    popped: bool,
}

const TILE_GAP: Pixels = px(12.);
const ROOM_PADDING: Pixels = px(16.);
const STRIP_HEIGHT: Pixels = px(104.);
const STRIP_MIN_WIDTH: Pixels = px(96.);

/// How many columns of 16:9 tiles make `count` of them as large as fits `area`, and their size.
fn grid_fit(count: usize, area: Area<Pixels>, gap: Pixels) -> (usize, Area<Pixels>) {
    let (width, height, gap) = (
        f32::from(area.width),
        f32::from(area.height),
        f32::from(gap),
    );
    (1..=count.max(1))
        .map(|columns| {
            let rows = count.max(1).div_ceil(columns);
            let cell_width = (width - gap * (columns - 1) as f32) / columns as f32;
            let cell_height = (height - gap * (rows - 1) as f32) / rows as f32;
            let tile_width = cell_width.min(cell_height * 16.0 / 9.0).max(0.0).floor();
            (columns, tile_width)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(columns, tile_width)| {
            let tile = size(px(tile_width), px((tile_width * 9.0 / 16.0).floor()));
            (columns, tile)
        })
        .unwrap_or((1, size(px(0.), px(0.))))
}

/// The space inside the room's padding, given the `room` between its header and control bar.
fn room_area(room: Area<Pixels>) -> Area<Pixels> {
    size(
        (room.width - ROOM_PADDING * 2.).max(px(0.)),
        (room.height - ROOM_PADDING * 2.).max(px(0.)),
    )
}

/// The largest size of shape `aspect` (width over height) that fits `area`.
fn fit_aspect(area: Area<Pixels>, aspect: f32) -> Area<Pixels> {
    let width = f32::from(area.width)
        .min(f32::from(area.height) * aspect)
        .max(0.)
        .floor();
    size(px(width), px((width / aspect).floor()))
}

fn grid(
    app: &mut ThroccApp,
    tiles: &[Tile],
    area: Area<Pixels>,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let (_, tile_size) = grid_fit(tiles.len(), area, TILE_GAP);
    div()
        .size_full()
        .overflow_hidden()
        .p(ROOM_PADDING)
        .flex()
        .flex_wrap()
        .content_center()
        .items_center()
        .justify_center()
        .gap(TILE_GAP)
        .children(tiles.iter().map(|t| {
            div()
                .flex_none()
                .w(tile_size.width)
                .h(tile_size.height)
                .child(tile(app, t, Layout::Grid, tile_size, window, cx))
        }))
        .into_any_element()
}

fn focused(
    app: &mut ThroccApp,
    tiles: &[Tile],
    focus: UserId,
    area: Area<Pixels>,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let others: Vec<&Tile> = tiles.iter().filter(|t| t.user != focus).collect();
    let focused = tiles.iter().find(|t| t.user == focus);
    let aspect = focused
        .and_then(|t| t.person.sharing)
        .map(|c| f32::from(c.width) / f32::from(c.height))
        .filter(|aspect| aspect.is_finite() && *aspect > 0.)
        .unwrap_or(16. / 9.);
    let strip_room = if others.is_empty() {
        px(0.)
    } else {
        STRIP_HEIGHT + TILE_GAP
    };
    let stage_size = fit_aspect(size(area.width, area.height - strip_room), aspect);
    let stage = focused.map(|t| tile(app, t, Layout::Stage, stage_size, window, cx));
    let fitting_width = (area.width - TILE_GAP * others.len().saturating_sub(1) as f32)
        / others.len().max(1) as f32;
    let tile_width = fitting_width
        .min(STRIP_HEIGHT * (16. / 9.))
        .max(STRIP_MIN_WIDTH);
    let strip_tile = size(tile_width, STRIP_HEIGHT);
    let strip_width = (strip_tile.width + TILE_GAP) * others.len() as f32;
    let strip = (!others.is_empty()).then(|| {
        div()
            .id("strip")
            .flex_none()
            .h(STRIP_HEIGHT)
            .flex()
            .gap(TILE_GAP)
            .when(strip_width <= area.width, |s| s.justify_center())
            .overflow_x_scroll()
            .children(others.into_iter().map(|t| {
                div()
                    .flex_none()
                    .w(strip_tile.width)
                    .h(strip_tile.height)
                    .child(tile(app, t, Layout::Strip, strip_tile, window, cx))
            }))
    });
    div()
        .size_full()
        .p(ROOM_PADDING)
        .flex()
        .flex_col()
        .gap(TILE_GAP)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex_none()
                        .w(stage_size.width)
                        .h(stage_size.height)
                        .children(stage),
                ),
        )
        .children(strip)
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Grid,
    Stage,
    Strip,
}

fn tile(
    app: &mut ThroccApp,
    t: &Tile,
    layout: Layout,
    size: Area<Pixels>,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let user = t.user;
    let compact = layout == Layout::Strip;
    let frame = div()
        .id(("tile", user.0 as usize))
        .size_full()
        .relative()
        .rounded(theme::RADIUS_TILE)
        .overflow_hidden()
        .border_1()
        .border_color(if t.person.speaking {
            theme::speaking()
        } else {
            theme::control()
        })
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                app.open_person_menu(user, event.position, window, cx)
            }),
        );
    let speaking_ring = t.person.speaking.then(|| {
        div()
            .absolute()
            .inset_0()
            .rounded(theme::RADIUS_TILE - px(1.))
            .border_1()
            .border_color(theme::speaking())
    });
    if !t.shows_stream {
        return frame
            .bg(theme::surface())
            .child(person_card(t, size.height, compact, cx))
            .children(speaking_ring)
            .into_any_element();
    }
    let mine = t.mine;
    let buttons = if compact {
        Vec::new()
    } else {
        let pop_out = (!mine).then(|| {
            ghost_icon(
                ("pop-out", user.0 as usize),
                "picture-in-picture-2",
                "Pop Out",
            )
            .on_click(cx.listener(move |app, _, _, cx| {
                cx.stop_propagation();
                app.pop_out(user, CONTROLS_LINGER, cx);
            }))
        });
        let stop = ghost_icon(
            ("stop-watching", user.0 as usize),
            "eye-off",
            "Stop Watching",
        )
        .on_click(cx.listener(move |app, _, window, cx| {
            cx.stop_propagation();
            app.stop_watching(user, window, cx);
        }));
        let fullscreen = ghost_icon(("fullscreen", user.0 as usize), "maximize-2", "Full Screen")
            .on_click(cx.listener(move |app, _, window, cx| {
                cx.stop_propagation();
                app.enter_fullscreen(user, window, cx);
            }));
        pop_out.into_iter().chain([stop, fullscreen]).collect()
    };
    let overlay = ShareOverlay {
        name: t.person.name.clone().into(),
        seed: user.0,
        picture: t.picture.clone(),
        details: t
            .person
            .sharing
            .map(|c| share_label(&c))
            .unwrap_or_default()
            .into(),
        width: size.width,
        title_inset: px(0.),
        buttons,
        audience: t.person.audience,
        controls: app.shows_overlay(user),
        too_slow: app.state.health.slow_shares.contains(&user),
        fullscreen: false,
    };
    let radius = theme::RADIUS_TILE - px(1.);
    let picture = app
        .feeds
        .surface(app.client.as_ref(), user, radius, window, cx);
    frame
        .cursor_pointer()
        .bg(theme::surface())
        .child(picture)
        .children(overlay.render())
        .children(speaking_ring)
        .on_hover(cx.listener(move |app, hovered: &bool, _, cx| app.hover_tile(user, *hovered, cx)))
        .on_click(cx.listener(move |app, _, _, cx| {
            app.focus = (layout != Layout::Stage).then_some(user);
            cx.notify();
        }))
        .into_any_element()
}

/// Whether a person card `height` tall is compact, and how large its picture is, so that it
/// fits whole.
fn card_fit(height: Pixels, sharing: bool, compact: bool) -> (bool, Pixels) {
    let full_rest = if sharing { px(126.) } else { px(56.) };
    if !compact && height - full_rest >= px(32.) {
        let picture = (height - full_rest)
            .min((height * 0.35).max(px(32.)))
            .min(px(80.));
        return (false, picture.floor());
    }
    let compact_rest = if sharing { px(76.) } else { px(48.) };
    (
        true,
        (height - compact_rest).clamp(px(16.), px(32.)).floor(),
    )
}

/// A tile for someone whose share isn't shown here: their picture and name, and a way to
/// watch it when they share.
fn person_card(t: &Tile, height: Pixels, compact: bool, cx: &mut Context<ThroccApp>) -> Div {
    let user = t.user;
    let person = t.person;
    let (compact, picture_size) = card_fit(height, person.sharing.is_some(), compact);
    let action = person.sharing.map(|_| {
        let (label, icon_name) = if t.popped {
            ("Bring Back", "panel-top-open")
        } else if t.mine {
            ("Preview", "eye")
        } else if compact {
            ("Watch", "eye")
        } else {
            ("Watch Stream", "eye")
        };
        Button::new(("watch", user.0 as usize))
            .label(label)
            .icon(icon(icon_name))
            .primary()
            .rounded(theme::RADIUS_BUTTON)
            .with_size(if compact { Size::Small } else { Size::Medium })
            .on_click(cx.listener(move |app, _, _, cx| app.watch(user, cx)))
    });
    let status = person.sharing.map(|config| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(sharing_icon(px(14.)))
            .child(caption(share_label(&config)))
            .child(
                audience(person.audience, px(14.))
                    .ml_1()
                    .typeset(theme::CAPTION)
                    .text_color(theme::text_2()),
            )
    });
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(if compact { px(4.) } else { px(10.) })
        .p_2()
        .child(avatar(
            &person.name,
            user.0,
            t.picture.clone(),
            picture_size,
            false,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .max_w_full()
                .child(
                    div()
                        .typeset(if compact { theme::CAPTION } else { theme::BODY })
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(if t.mine {
                            format!("{} (you)", person.name)
                        } else {
                            person.name.clone()
                        }),
                )
                .children(muted_icon(person, px(13.))),
        )
        .children(status.filter(|_| !compact))
        .children(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(count: usize, width: f32, height: f32) -> (usize, f32, f32) {
        let (columns, tile) = grid_fit(count, size(px(width), px(height)), px(10.));
        (columns, f32::from(tile.width), f32::from(tile.height))
    }

    #[test]
    fn one_tile_fills_the_tighter_side() {
        assert_eq!(fit(1, 1600., 900.), (1, 1600., 900.));
        assert_eq!(fit(1, 800., 900.), (1, 800., 450.));
        assert_eq!(fit(1, 1600., 450.), (1, 800., 450.));
    }

    #[test]
    fn the_stage_takes_the_shape_of_the_stream() {
        let wide = fit_aspect(size(px(1000.), px(500.)), 16. / 9.);
        assert_eq!(
            (f32::from(wide.width), f32::from(wide.height)),
            (888., 499.)
        );
        let tall = fit_aspect(size(px(1000.), px(900.)), 16. / 10.);
        assert_eq!(
            (f32::from(tall.width), f32::from(tall.height)),
            (1000., 625.)
        );
    }

    #[test]
    fn tiles_spread_out_to_use_the_room() {
        assert_eq!(fit(2, 1610., 600.), (2, 800., 450.));
        assert_eq!(fit(2, 600., 1000.).0, 1);
        assert_eq!(fit(4, 1610., 910.), (2, 800., 450.));
        assert_eq!(fit(5, 1610., 910.).0, 3);
    }

    #[test]
    fn person_cards_fit_their_tile() {
        for sharing in [false, true] {
            for height in (112..400).map(|h| px(h as f32)) {
                let (compact, picture) = card_fit(height, sharing, false);
                let rest = match (compact, sharing) {
                    (false, true) => 126.,
                    (false, false) => 56.,
                    (true, true) => 76.,
                    (true, false) => 48.,
                };
                assert!(f32::from(picture) + rest <= f32::from(height), "{height:?}");
                if !compact {
                    assert!((32. ..=80.).contains(&f32::from(picture)), "{height:?}");
                }
            }
        }
    }

    #[test]
    fn tiles_always_fit_the_area() {
        for count in 1..=16 {
            for (width, height) in [(400., 300.), (1000., 500.), (700., 1200.), (2500., 1300.)] {
                let (columns, tile_width, tile_height) = fit(count, width, height);
                let rows = count.div_ceil(columns);
                assert!(tile_width * columns as f32 + 10. * (columns - 1) as f32 <= width);
                assert!(tile_height * rows as f32 + 10. * (rows - 1) as f32 <= height);
            }
        }
    }
}
