use gpui::{Div, ParentElement, Styled, div};

use throcc_client_core::state::ConnectionStatus;

use super::common::Typeset;
use crate::app::{Screen, ThroccApp};
use crate::theme;

/// The reconnecting or failed strip across the top of settings and server pages.
pub fn render(app: &ThroccApp) -> Option<Div> {
    if matches!(app.screen, Screen::Join | Screen::Main) {
        return None;
    }
    let (text, warn) = match &app.state.connection {
        ConnectionStatus::Reconnecting { attempt, reason } => (
            format!("Reconnecting (attempt {attempt}): {reason}. You’ll be put back in your room."),
            true,
        ),
        ConnectionStatus::Failed(reason) => (format!("Disconnected: {reason}"), false),
        _ => return None,
    };
    let (bg, fg) = if warn {
        (theme::warn_bg(), theme::warn_text())
    } else {
        (theme::danger_bg(), theme::danger_text())
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
            .child(text),
    )
}
