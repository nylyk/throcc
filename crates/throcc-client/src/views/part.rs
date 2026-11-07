use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, Subscription, WeakEntity, Window, div,
};
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;

/// Draws one part of the main window from the app.
pub type Draw = Rc<dyn Fn(&mut ThroccApp, &mut Window, &mut Context<ThroccApp>) -> AnyElement>;

/// The parts of the main window drawn as views of their own, by what each shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    ServerBar,
    Sidebar,
    RoomHeader,
    RoomControls,
    /// The card of someone whose share isn't shown.
    Card(UserId),
    /// The labels and controls over someone's share in a tile.
    TileOverlay(UserId),
    /// The labels and controls over a share filling the window.
    FullscreenOverlay(UserId),
}

/// A part of the main window drawn from the app's state as a view of its own, so the window
/// reuses it while only a share redraws. It redraws whenever the app is notified.
pub struct AppPart {
    app: WeakEntity<ThroccApp>,
    draw: Option<Draw>,
    _redraw_with_app: Subscription,
}

impl AppPart {
    pub fn new(app: &Entity<ThroccApp>, cx: &mut Context<Self>) -> Self {
        Self {
            app: app.downgrade(),
            draw: None,
            _redraw_with_app: cx.observe(app, |_, _, cx| cx.notify()),
        }
    }

    /// Draws with `draw` the next time this part redraws.
    pub fn set_draw(&mut self, draw: Draw) {
        self.draw = Some(draw);
    }
}

impl Render for AppPart {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(draw) = self.draw.clone() else {
            return div().into_any_element();
        };
        self.app
            .update(cx, |app, cx| draw(app, window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}
