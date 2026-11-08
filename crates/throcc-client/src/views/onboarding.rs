use gpui::{Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::Disableable;
use gpui_component::input::Input;

use super::common::{Typeset, card, description, field_error, primary};
use super::join::frame;
use super::mascot;
use super::page::{field, picture_editor, save_line};
use crate::app::{PictureFor, ThroccApp, typed_name};
use crate::theme;

/// The window before we have a name. It asks for one, and optionally a picture.
pub fn render(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> impl IntoElement {
    let typed = typed_name(&app.name, cx);
    let shown_name = typed.clone().ok().flatten().unwrap_or_default();
    let picture = picture_editor(app, PictureFor::Profile, &shown_name, 0, None, true, cx);
    let finish = primary("finish-onboarding", "Continue")
        .disabled(!matches!(typed, Ok(Some(_))))
        .on_click(cx.listener(|app, _, window, cx| app.finish_onboarding(window, cx)));
    let finish = save_line(app.profile_form.status.as_ref(), str::to_owned, finish);
    frame(app.mascot.pick(mascot::WELCOMERS))
        .child(div().typeset(theme::LARGE_TITLE).child("Welcome to throcc"))
        .child(description("Choose the name people see you as."))
        .child(
            card()
                .w(px(420.))
                .flex()
                .flex_col()
                .gap_4()
                .child(picture)
                .child(field("Name", Input::new(&app.name)).children(typed.err().map(field_error)))
                .child(finish),
        )
}
