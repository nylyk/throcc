use std::time::Instant;

use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription,
    WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div, px, size,
};
use gpui_component::Root;
use throcc_client_core::state::share_label;
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;
use crate::theme;
use crate::video::Feeds;
use crate::views::common::{CONTROLS_LINGER, avatar, caption, player, player_button};

/// One watched share in a window of its own.
pub struct PopOut {
    app: WeakEntity<ThroccApp>,
    user: UserId,
    feeds: Feeds,
    controls_until: Instant,
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
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(320.), px(228.))),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            window.set_window_title(&title);
            let view = cx.new(|cx| Self {
                app,
                user,
                feeds: Feeds::default(),
                controls_until: Instant::now(),
                focus: cx.focus_handle(),
                _released: cx.on_release(move |this: &mut Self, cx| {
                    let app = this.app.clone();
                    cx.defer(move |cx| {
                        app.update(cx, |app, cx| {
                            app.popouts.remove(&user);
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
}

impl PopOut {
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
        let user = self.user;
        let Some(app) = self.app.upgrade() else {
            return div().id("popout").size_full().bg(theme::bg());
        };
        let (client, state) = {
            let app = app.read(cx);
            (app.client.clone(), app.state.clone())
        };
        let person = state.person(user).cloned();
        let picture = app.update(cx, |app, _| app.picture(person.as_ref()?.avatar));
        let name = person.as_ref().map_or("Someone".into(), |p| p.name.clone());
        let room = state
            .current
            .as_ref()
            .and_then(|c| state.room(c.room))
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let config = person.as_ref().and_then(|p| p.sharing);
        let details = match config {
            Some(config) => format!("{room} · {}", share_label(&config)),
            None => room,
        };
        let fullscreen = window.is_fullscreen();
        let picture_of_sharer = picture;
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
        let title = div()
            .flex()
            .items_center()
            .gap_3()
            .child(avatar(&name, user.0, picture_of_sharer, px(28.), false))
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
                    .child(div().text_size(px(12.)).opacity(0.75).child(details)),
            );
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
        div()
            .id("popout")
            .track_focus(&self.focus)
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::font())
            .text_size(px(14.))
            .on_mouse_move(cx.listener(|this, _, _, cx| this.show_controls(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.set_fullscreen(false, window, cx);
                }
            }))
            .child(player(picture, title, buttons, controls))
    }
}
