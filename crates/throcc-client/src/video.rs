use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div};
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;
use crate::theme;
use crate::views::common::caption;

/// The watched share's picture. The only place GPU handles meet gpui.
pub fn surface(
    _app: &ThroccApp,
    _user: UserId,
    _window: &mut Window,
    _cx: &mut Context<ThroccApp>,
) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::text_3())
        .child(caption("Waiting for the first picture…"))
        .into_any_element()
}
