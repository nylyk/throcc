use gpui::{
    App, AppContext, Bounds, Context, FontWeight, IntoElement, ParentElement, Render, Styled,
    Subscription, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div, px, size,
};
use gpui_component::Root;
use throcc_client_core::state::share_label;
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;
use crate::theme;
use crate::video::Feeds;
use crate::views::common::{avatar, caption, ghost_icon, mono};

/// One watched share in a window of its own.
pub struct PopOut {
    app: WeakEntity<ThroccApp>,
    user: UserId,
    feeds: Feeds,
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

impl Render for PopOut {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let user = self.user;
        let Some(app) = self.app.upgrade() else {
            return div().size_full().bg(theme::bg());
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
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(48.))
            .pl_3()
            .pr(px(6.))
            .flex_none()
            .bg(theme::sidebar())
            .border_b_1()
            .border_color(theme::control())
            .child(avatar(&name, user.0, picture, px(24.), false))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{name}'s screen")),
            )
            .child(
                mono(details)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(theme::text_3()),
            )
            .child(
                ghost_icon("return", "panel-top-open", "Return to the main window")
                    .on_click(|_, window, _| window.remove_window()),
            )
            .child(
                ghost_icon("popout-fullscreen", "maximize-2", "Fullscreen")
                    .on_click(|_, window, _| window.toggle_fullscreen()),
            );
        let picture = if config.is_some() {
            self.feeds.surface(client.as_ref(), user, window, cx)
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(caption(format!("{name} stopped sharing.")))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::font())
            .text_size(px(14.))
            .child(header)
            .child(div().flex_1().min_h_0().child(picture))
    }
}
