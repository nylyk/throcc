use gpui::{Div, ParentElement, Styled, div};

use super::common::Typeset;
use crate::app::{Screen, ThroccApp};
use crate::theme;

/// The reconnecting or failed strip across the top of settings and server pages.
pub fn render(app: &ThroccApp) -> Option<Div> {
    if matches!(app.screen, Screen::Join | Screen::Main) {
        return None;
    }
    let notice = app.state.connection.notice()?;
    let (bg, fg) = if notice.serious {
        (theme::danger_bg(), theme::danger_text())
    } else {
        (theme::warn_bg(), theme::warn_text())
    };
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .px_4()
            .py_2()
            .bg(bg)
            .text_color(fg)
            .typeset(theme::CALLOUT)
            .child(notice.summary),
    )
}
