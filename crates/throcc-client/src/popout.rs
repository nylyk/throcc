use std::time::Instant;

use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, StatefulInteractiveElement,
    Styled, Subscription, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div,
    point, px, size,
};
use gpui_component::Root;
use throcc_client_core::state::share_label;
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;
use crate::theme;
use crate::video::Feeds;
use crate::views::common::{
    CONTROLS_LINGER, TRAFFIC_LIGHTS, Typeset, app_titlebar, caption, moves_window, player,
    player_button, player_title,
};

const TRAFFIC_LIGHTS_AT: Point<Pixels> = point(px(20.), px(25.));

/// One watched share in a window of its own.
pub struct PopOut {
    app: WeakEntity<ThroccApp>,
    user: UserId,
    feeds: Feeds,
    controls_until: Instant,
    traffic_lights_shown: bool,
    focus: FocusHandle,
    _released: Subscription,
}

impl PopOut {
    pub fn open(
        app: WeakEntity<ThroccApp>,
        user: UserId,
        title: String,
        cx: &mut App,
    ) -> Option<WindowHandle<Root>> {
        let bounds = Bounds::centered(None, size(px(960.), px(588.)), cx);
        let options = app_titlebar(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(320.), px(228.))),
                ..Default::default()
            },
            TRAFFIC_LIGHTS_AT,
            false,
        );
        let opened = cx.open_window(options, |window, cx| {
            window.set_window_title(&title);
            let handle = window.window_handle();
            let view = cx.new(|cx| Self {
                app,
                user,
                feeds: Feeds::default(),
                controls_until: Instant::now(),
                traffic_lights_shown: true,
                focus: cx.focus_handle(),
                _released: cx.on_release(move |this: &mut Self, cx| {
                    let app = this.app.clone();
                    cx.defer(move |cx| {
                        app.update(cx, |app, cx| {
                            app.popouts.remove(&user);
                            app.person_menu.take_if(|menu| menu.window == handle);
                            cx.notify();
                        })
                        .ok();
                    });
                }),
            });
            cx.new(|cx| Root::new(view, window, cx))
        });
        opened
            .inspect_err(|e| tracing::warn!("could not open a window for the share: {e:#}"))
            .ok()
    }

    fn set_fullscreen(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if window.is_fullscreen() != on {
            window.toggle_fullscreen();
        }
        self.show_controls(cx);
    }

    fn show_controls(&mut self, cx: &mut Context<Self>) {
        self.controls_until = Instant::now() + CONTROLS_LINGER;
        cx.notify();
    }
}

impl Render for PopOut {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.feeds.sweep(window, cx);
        let user = self.user;
        let Some(app) = self.app.upgrade() else {
            return div().id("popout").size_full().bg(theme::bg());
        };
        let (client, state) = {
            let app = app.read(cx);
            (app.client.clone(), app.state.clone())
        };
        let person = state.person(user);
        let sharer_picture = app.update(cx, |app, cx| app.picture(person?.avatar, cx));
        let name = person.map_or("Someone", |p| &p.name);
        let room = state
            .current
            .as_ref()
            .and_then(|c| state.room(c.room))
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let config = person.and_then(|p| p.sharing);
        let details = match config {
            Some(config) => format!("{room} · {}", share_label(&config)),
            None => room,
        };
        let fullscreen = window.is_fullscreen();
        let picture = if config.is_some() {
            self.feeds
                .surface(client.as_ref(), user, px(0.), window, cx)
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(caption(format!("{name} stopped sharing.")))
                .into_any_element()
        };
        let title = player_title(name, user.0, sharer_picture, details).pl(if fullscreen {
            px(0.)
        } else {
            TRAFFIC_LIGHTS
        });
        let buttons = div()
            .flex()
            .gap_2()
            .child(
                player_button("return", "panel-top-open")
                    .on_click(|_, window, _| window.remove_window()),
            )
            .child(
                player_button(
                    "popout-fullscreen",
                    if fullscreen {
                        "minimize-2"
                    } else {
                        "maximize-2"
                    },
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_fullscreen(!fullscreen, window, cx)
                })),
            );
        let controls = Instant::now() < self.controls_until;
        if controls != self.traffic_lights_shown {
            show_traffic_lights(window, controls);
            self.traffic_lights_shown = controls;
        }
        moves_window(div().id("popout"))
            .track_focus(&self.focus)
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::font())
            .typeset(theme::BODY)
            .on_mouse_move(cx.listener(|this, _, _, cx| this.show_controls(cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.app
                        .update(cx, |app, cx| {
                            app.open_person_menu(user, event.position, window, cx)
                        })
                        .ok();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    let closed_menu = this
                        .app
                        .update(cx, |app, cx| app.close_person_menu_in(window, cx))
                        .unwrap_or(false);
                    if !closed_menu {
                        this.set_fullscreen(false, window, cx);
                    }
                }
            }))
            .child(player(picture, title, buttons, controls))
            .children(app.update(cx, |app, cx| {
                crate::views::person_menu::render(app, window, cx)
            }))
    }
}

#[cfg(target_os = "macos")]
fn show_traffic_lights(gpui_window: &Window, shown: bool) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(gpui_window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: an AppKit handle's `ns_view` is the window's live content view.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else {
        return;
    };
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = window.standardWindowButton(button) {
            button.setHidden(!shown);
        }
    }
    if shown {
        gpui_window.set_traffic_light_position(TRAFFIC_LIGHTS_AT);
    }
}

#[cfg(not(target_os = "macos"))]
fn show_traffic_lights(_: &Window, _: bool) {}
