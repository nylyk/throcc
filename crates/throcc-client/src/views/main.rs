use std::sync::Arc;

use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    RenderImage, Size as Area, StatefulInteractiveElement, Styled, Window, div, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::{Disableable, Sizable, Size};
use throcc_client_core::client::Command;
use throcc_client_core::state::{Person, State, share_label};
use throcc_proto::ids::{RoomId, UserId};

use super::common::{
    TRAFFIC_LIGHTS, avatar, caption, card, control_button, ghost_icon, icon, moves_window, player,
    player_button, primary, section, sharing_icon, title,
};
use super::settings::{resize_handle, sidebar_column};
use crate::app::{Dialog, Screen, ServerPage, SettingsPage, ThroccApp};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    if let Some(user) = app.fullscreen.filter(|user| app.shows_stream(*user)) {
        return fullscreen(app, user, window, cx).into_any_element();
    }
    let content = match app.state.current.clone() {
        Some(current) => room(app, current.room, window, cx).into_any_element(),
        None => lobby(app, cx).into_any_element(),
    };
    div()
        .size_full()
        .flex()
        .child(sidebar(app, cx))
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
    let person = app.state.person(user).cloned();
    let picture = app.picture(person.as_ref().and_then(|p| p.avatar));
    let name = person.as_ref().map_or("Someone".into(), |p| p.name.clone());
    let config = person
        .as_ref()
        .and_then(|p| p.sharing)
        .map(|c| share_label(&c))
        .unwrap_or_default();
    let mine = Some(user) == app.state.me;
    let title = div()
        .flex()
        .items_center()
        .gap_3()
        .child(avatar(&name, user.0, picture, px(28.), false))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(16.))
                        .child(format!("{name}'s screen")),
                )
                .child(div().text_size(px(12.)).opacity(0.75).child(config)),
        );
    let buttons = div()
        .flex()
        .gap_2()
        .child(
            player_button("fullscreen-stop-watching", "eye-off").on_click(cx.listener(
                move |app, _, window, cx| {
                    if mine {
                        app.preview_self = false;
                    } else {
                        app.command(Command::Unwatch(user));
                    }
                    app.exit_fullscreen(window, cx);
                },
            )),
        )
        .child(
            player_button("exit-fullscreen", "minimize-2")
                .on_click(cx.listener(|app, _, window, cx| app.exit_fullscreen(window, cx))),
        );
    let controls = std::time::Instant::now() < app.controls_until;
    let stream = app
        .feeds
        .surface(app.client.as_ref(), user, px(0.), window, cx);
    div()
        .id("fullscreen")
        .track_focus(&app.fullscreen_focus)
        .size_full()
        .on_mouse_move(cx.listener(|app, _, _, cx| app.show_controls(cx)))
        .on_key_down(cx.listener(|app, event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                app.exit_fullscreen(window, cx);
            }
        }))
        .child(player(stream, title, buttons, controls))
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
        .map_or("server".to_owned(), |s| s.title().to_owned());
    let server_picture = app.picture(state.server.as_ref().and_then(|s| s.avatar));
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
                    .children(sharing.then(sharing_icon))
                    .children((!people.is_empty()).then(|| caption(people.len().to_string())))
                    .children(manages.then(|| {
                        ghost_icon(("edit-room", id.0 as usize), "pencil", "Edit room")
                            .with_size(Size::Small)
                            .on_click(cx.listener(move |app, _, window, cx| {
                                cx.stop_propagation();
                                app.open_edit_room(id, window, cx);
                            }))
                    }))
                    .on_click(cx.listener(move |app, _, _, _| {
                        if app.state.current.as_ref().is_none_or(|c| c.room != id) {
                            app.command(Command::Join(Some(id)));
                        }
                    })),
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

    sidebar_column(app.sidebar_width)
        .child(
            moves_window(div())
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pl(px(12.) + TRAFFIC_LIGHTS)
                .h(theme::HEADER_HEIGHT)
                .flex_none()
                .border_b_1()
                .border_color(theme::control())
                .child(avatar(&server, 0, server_picture, px(28.), false))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(server),
                )
                .child(
                    ghost_icon("server-settings", "settings-2", "Server settings").on_click(
                        cx.listener(|app, _, window, cx| {
                            app.fill_server(window, cx);
                            app.screen = Screen::Server(ServerPage::Overview);
                            cx.notify();
                        }),
                    ),
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
                    toggle_icon("profile-mic", "mic", "mic-off", !mic, "Mute (⌘⇧M)")
                        .disabled(!in_room)
                        .on_click(
                            cx.listener(move |app, _, _, _| app.command(Command::SetMic(!mic))),
                        ),
                )
                .child(
                    toggle_icon(
                        "profile-deafen",
                        "headphones",
                        "headphone-off",
                        deafened,
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
                        |app, _, window, cx| {
                            app.fill_profile(window, cx);
                            app.screen = Screen::Settings(SettingsPage::Profile);
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(resize_handle(cx))
}

/// A ghost icon that turns red, showing `active_icon`, while `active`.
fn toggle_icon(
    id: &'static str,
    icon_name: &'static str,
    active_icon: &'static str,
    active: bool,
    tooltip: &'static str,
) -> Button {
    let shown = if active { active_icon } else { icon_name };
    let shown = icon(shown);
    Button::new(id)
        .icon(if active {
            shown.text_color(theme::danger_text())
        } else {
            shown
        })
        .ghost()
        .rounded(theme::RADIUS_ROW)
        .tooltip(tooltip)
}

trait Selected {
    fn when_selected(self, selected: bool) -> Self;
}

impl<T: Styled> Selected for T {
    fn when_selected(self, selected: bool) -> Self {
        if selected {
            self.bg(theme::selected())
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
        .children(person.sharing.is_some().then(sharing_icon))
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
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .ml_2()
                            .text_size(px(12.))
                            .text_color(theme::danger_text())
                            .child(sharing_icon())
                            .child(sharing.to_string())
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
    let tiles: Vec<Tile> = people
        .iter()
        .map(|(user, person)| {
            let user = *user;
            let mine = Some(user) == me;
            let sharing = person.sharing.is_some();
            Tile {
                user,
                person: person.clone(),
                picture: pictures.get(&user).cloned(),
                mine,
                watched: sharing
                    && if mine {
                        app.preview_self
                    } else {
                        watching.contains(&user)
                    },
                popped: app.popouts.contains_key(&user),
            }
        })
        .collect();
    let rtt = state.health.rtt_ms;

    let header = moves_window(div())
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
                        app.dialog = Some(Dialog::Share);
                    }
                    cx.notify();
                })),
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

    let focus = app
        .focus
        .filter(|f| tiles.iter().any(|t| t.user == *f && t.shows_stream()));
    let body = match focus {
        Some(focus) => focused(app, &tiles, focus, window, cx),
        None => grid(app, &tiles, window, cx),
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(body)
        .child(controls)
}

/// One person in the room, and what their tile shows.
struct Tile {
    user: UserId,
    person: Person,
    picture: Option<Arc<RenderImage>>,
    mine: bool,
    watched: bool,
    popped: bool,
}

impl Tile {
    fn shows_stream(&self) -> bool {
        self.watched && !self.popped
    }
}

const TILE_GAP: Pixels = px(12.);
const ROOM_PADDING: Pixels = px(16.);
const STRIP_HEIGHT: Pixels = px(124.);

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
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let viewport = window.viewport_size();
    let area = size(
        viewport.width - app.sidebar_width - ROOM_PADDING * 2.,
        viewport.height - theme::HEADER_HEIGHT - theme::FOOTER_HEIGHT - ROOM_PADDING * 2.,
    );
    let (_, tile_size) = grid_fit(tiles.len(), area, TILE_GAP);
    div()
        .flex_1()
        .min_h_0()
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
                .child(tile(app, t, Layout::Grid, window, cx))
        }))
        .into_any_element()
}

fn focused(
    app: &mut ThroccApp,
    tiles: &[Tile],
    focus: UserId,
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
    let viewport = window.viewport_size();
    let strip_room = if others.is_empty() {
        px(0.)
    } else {
        STRIP_HEIGHT + TILE_GAP
    };
    let stage_size = fit_aspect(
        size(
            viewport.width - app.sidebar_width - ROOM_PADDING * 2.,
            viewport.height
                - theme::HEADER_HEIGHT
                - theme::FOOTER_HEIGHT
                - ROOM_PADDING * 2.
                - strip_room,
        ),
        aspect,
    );
    let stage = focused.map(|t| tile(app, t, Layout::Stage, window, cx));
    let strip_tile = size(STRIP_HEIGHT * (16. / 9.), STRIP_HEIGHT);
    let strip_width = (strip_tile.width + TILE_GAP) * others.len() as f32;
    let room_width = window.viewport_size().width - app.sidebar_width - ROOM_PADDING * 2.;
    let strip = (!others.is_empty()).then(|| {
        div()
            .id("strip")
            .flex_none()
            .h(STRIP_HEIGHT)
            .flex()
            .gap(TILE_GAP)
            .when_fits(strip_width <= room_width)
            .overflow_x_scroll()
            .children(others.into_iter().map(|t| {
                div()
                    .flex_none()
                    .w(strip_tile.width)
                    .h(strip_tile.height)
                    .child(tile(app, t, Layout::Strip, window, cx))
            }))
    });
    div()
        .flex_1()
        .min_h_0()
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

trait Centered {
    fn when_fits(self, fits: bool) -> Self;
}

impl<T: Styled> Centered for T {
    fn when_fits(self, fits: bool) -> Self {
        if fits { self.justify_center() } else { self }
    }
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
        });
    if !t.shows_stream() {
        return frame
            .bg(theme::surface())
            .child(person_card(t, compact, cx))
            .into_any_element();
    }
    let config = t
        .person
        .sharing
        .map(|c| share_label(&c))
        .unwrap_or_default();
    let label = div()
        .absolute()
        .bottom_2()
        .left_2()
        .max_w(relative(0.9))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme::RADIUS_ROW)
        .bg(theme::overlay())
        .child(avatar(
            &t.person.name,
            user.0,
            t.picture.clone(),
            px(18.),
            t.person.speaking,
        ))
        .child(
            div()
                .text_size(px(13.))
                .truncate()
                .child(t.person.name.clone()),
        )
        .children((!compact).then(|| caption(config)));
    let mine = t.mine;
    let buttons = (!compact).then(|| {
        div()
            .absolute()
            .top_2()
            .right_2()
            .flex()
            .gap_1()
            .p_1()
            .rounded(theme::RADIUS_ROW)
            .bg(theme::overlay())
            .children((!mine).then(|| {
                ghost_icon(
                    ("pop-out", user.0 as usize),
                    "picture-in-picture-2",
                    "Pop out",
                )
                .on_click(cx.listener(move |app, _, _, cx| {
                    cx.stop_propagation();
                    app.pop_out(user, cx);
                }))
            }))
            .child(
                ghost_icon(
                    ("stop-watching", user.0 as usize),
                    "eye-off",
                    "Stop watching",
                )
                .on_click(cx.listener(move |app, _, _, cx| {
                    cx.stop_propagation();
                    if mine {
                        app.preview_self = false;
                    } else {
                        app.command(Command::Unwatch(user));
                    }
                    cx.notify();
                })),
            )
            .child(
                ghost_icon(("fullscreen", user.0 as usize), "maximize-2", "Fullscreen").on_click(
                    cx.listener(move |app, _, window, cx| {
                        cx.stop_propagation();
                        app.enter_fullscreen(user, window, cx);
                    }),
                ),
            )
    });
    let radius = theme::RADIUS_TILE - px(1.);
    let picture = app
        .feeds
        .surface(app.client.as_ref(), user, radius, window, cx);
    frame
        .cursor_pointer()
        .bg(theme::stage())
        .child(picture)
        .child(label)
        .children(buttons)
        .on_click(cx.listener(move |app, _, _, cx| {
            app.focus = (layout != Layout::Stage).then_some(user);
            cx.notify();
        }))
        .into_any_element()
}

/// A tile for someone whose share isn't shown here: their picture and name, and a way to
/// watch it when they share.
fn person_card(t: &Tile, compact: bool, cx: &mut Context<ThroccApp>) -> Div {
    let user = t.user;
    let person = &t.person;
    let action = person.sharing.map(|_| {
        let (label, icon_name) = if t.popped {
            ("Bring back", "panel-top-open")
        } else if t.mine {
            ("Preview", "eye")
        } else if compact {
            ("Watch", "eye")
        } else {
            ("Watch stream", "eye")
        };
        let popped = t.popped;
        let mine = t.mine;
        Button::new(("watch", user.0 as usize))
            .label(label)
            .icon(icon(icon_name))
            .primary()
            .rounded(theme::RADIUS_BUTTON)
            .with_size(if compact { Size::Small } else { Size::Medium })
            .on_click(cx.listener(move |app, _, _, cx| {
                if popped {
                    app.close_popout(user, cx);
                } else if mine {
                    app.preview_self = true;
                } else {
                    app.command(Command::Watch(user));
                }
                cx.notify();
            }))
    });
    let status = person.sharing.map(|config| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(sharing_icon())
            .children((!compact).then(|| caption(share_label(&config))))
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
            if compact { px(32.) } else { px(56.) },
            person.speaking,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .max_w_full()
                .child(
                    div()
                        .text_size(px(if compact { 12. } else { 14. }))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(if t.mine {
                            format!("{} (you)", person.name)
                        } else {
                            person.name.clone()
                        }),
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
        .children((!compact).then_some(status).flatten())
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
