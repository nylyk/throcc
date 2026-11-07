use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, Subscription, WeakEntity, Window, div,
};

use crate::app::ThroccApp;

/// Draws one part of the main window from the app.
pub type Draw = fn(&mut ThroccApp, &mut Window, &mut Context<ThroccApp>) -> AnyElement;

/// A part of the main window drawn from the app's state as a view of its own, so the window
/// reuses it while only a share redraws. It redraws whenever the app is notified.
pub struct AppPart {
    app: WeakEntity<ThroccApp>,
    draw: Draw,
    _redraw_with_app: Subscription,
}

impl AppPart {
    pub fn new(app: &Entity<ThroccApp>, draw: Draw, cx: &mut Context<Self>) -> Self {
        Self {
            app: app.downgrade(),
            draw,
            _redraw_with_app: cx.observe(app, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for AppPart {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draw = self.draw;
        self.app
            .update(cx, |app, cx| draw(app, window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}
