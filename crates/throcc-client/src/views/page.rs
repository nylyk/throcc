use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, SharedString, Stateful, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::Disableable;
use gpui_component::button::Button;
use gpui_component::switch::Switch;
use throcc_proto::ids::Hash;

use super::common::{Typeset, avatar, caption, ghost_icon, icon, secondary, top_bar};
use crate::app::{PictureFor, SaveStatus, Screen, ThroccApp};
use crate::theme;

/// A settings-style screen laid out like the main one: the sidebar under a bar with a back
/// arrow and `heading`, the page under a bar with its `page_title`.
pub fn frame(
    width: Pixels,
    heading: &'static str,
    links: Div,
    page_title: &'static str,
    content: AnyElement,
    cx: &mut Context<ThroccApp>,
) -> Div {
    let sidebar = sidebar_column(width)
        .child(
            top_bar()
                .gap_2()
                .px_3()
                .child(
                    ghost_icon("settings-back", "arrow-left", "Back").on_click(cx.listener(
                        |app, _, _, cx| {
                            app.screen = if app.client.is_some() {
                                Screen::Main
                            } else {
                                Screen::Join
                            };
                            cx.notify();
                        },
                    )),
                )
                .child(div().typeset(theme::TITLE_3).child(heading)),
        )
        .child(links)
        .child(resize_handle(cx));
    let page = div()
        .bg(theme::bg())
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            top_bar()
                .px_6()
                .child(div().typeset(theme::TITLE_3).child(page_title)),
        )
        .child(
            div()
                .id("settings-content")
                .flex_1()
                .overflow_y_scroll()
                .p_8()
                .child(div().max_w(px(640.)).child(content)),
        );
    div().size_full().flex().child(sidebar).child(page)
}

/// A sidebar `width` wide, see-through to the blurred desktop on macOS.
pub fn sidebar_column(width: Pixels) -> Div {
    div()
        .relative()
        .w(width)
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(theme::sidebar_glass())
        .border_r_1()
        .border_color(theme::control())
}

/// The strip along a sidebar's right edge that drags its width.
pub fn resize_handle(cx: &mut Context<ThroccApp>) -> Stateful<Div> {
    div()
        .id("sidebar-resize")
        .absolute()
        .top_0()
        .right_0()
        .h_full()
        .w(px(6.))
        .cursor_col_resize()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                app.resizing_sidebar = true;
                cx.stop_propagation();
            }),
        )
}

/// The column of links to `pages` in a settings-style sidebar, each showing its `screen`.
pub fn nav<P: Copy + PartialEq + 'static>(
    pages: impl IntoIterator<Item = (P, &'static str, &'static str)>,
    current: P,
    screen: fn(P) -> Screen,
    cx: &mut Context<ThroccApp>,
) -> Div {
    div()
        .flex_1()
        .p_3()
        .flex()
        .flex_col()
        .gap_1()
        .children(
            pages
                .into_iter()
                .enumerate()
                .map(|(i, (page, label, icon_name))| {
                    div()
                        .id(("nav", i))
                        .px_3()
                        .min_h(px(36.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(theme::RADIUS_ROW)
                        .cursor_pointer()
                        .when(page == current, |s| s.bg(theme::raised()))
                        .hover(|s| s.bg(theme::raised()))
                        .child(icon(icon_name).size(px(16.)).text_color(theme::text_2()))
                        .child(label)
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.show_screen(screen(page), window, cx)
                        }))
                }),
        )
}

/// The page's picture of `target`, its unsaved one first, with buttons to choose another or
/// remove it when `editable`.
pub fn picture_editor(
    app: &mut ThroccApp,
    target: PictureFor,
    name: &str,
    seed: u64,
    saved: Option<Hash>,
    editable: bool,
    cx: &mut Context<ThroccApp>,
) -> Div {
    let picture = match &app.form(target).edit {
        Some(edit) => edit.image.clone(),
        None => app.picture(saved, cx),
    };
    let has_picture = picture.is_some();
    let saving = app.form(target).saving();
    let buttons = editable.then(|| {
        div()
            .flex()
            .gap_2()
            .child(
                secondary(("choose-picture", target as usize), "Choose Picture…")
                    .disabled(saving)
                    .on_click(cx.listener(move |app, _, _, cx| app.choose_picture(target, cx))),
            )
            .children(has_picture.then(|| {
                secondary(("remove-picture", target as usize), "Remove")
                    .disabled(saving)
                    .on_click(cx.listener(move |app, _, _, cx| app.remove_picture(target, cx)))
            }))
    });
    div()
        .flex()
        .items_center()
        .gap_4()
        .child(avatar(name, seed, picture, px(64.), false))
        .children(buttons)
}

/// A card's Save button at the bottom right, with how the last save went beside it, a
/// refusal worded by `refused`.
pub fn save_line(
    status: Option<&SaveStatus>,
    refused: impl FnOnce(&str) -> String,
    save: Button,
) -> Div {
    let status = status.map(|status| match status {
        SaveStatus::Saving => caption("Saving…"),
        SaveStatus::Saved => caption("Saved").text_color(theme::ok_text()),
        SaveStatus::UntilJoined => {
            caption("Saved here. Others see your picture once you join a room.")
        }
        SaveStatus::Failed(reason) => caption(refused(reason)).text_color(theme::danger_text()),
    });
    div()
        .flex()
        .items_center()
        .justify_end()
        .gap_3()
        .children(status)
        .child(save)
}

/// A labelled field, the label above the control.
pub fn field(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(field_label(label))
        .child(control)
}

/// A field's label, for one laid out by hand.
pub fn field_label(label: &str) -> Div {
    div()
        .typeset(theme::CALLOUT)
        .text_color(theme::text_2())
        .child(label.to_owned())
}

/// A labelled control, the label to its left.
pub fn row(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_4()
        .gap_y_1()
        .min_h(theme::TOUCH)
        .child(
            div()
                .w(px(180.))
                .text_color(theme::text_2())
                .child(label.to_owned()),
        )
        .child(div().flex_1().min_w(px(200.)).child(control))
}

/// A switch at the height of a [`row`].
pub fn switch_row(switch: Switch) -> Div {
    div()
        .flex()
        .items_center()
        .min_h(theme::TOUCH)
        .child(switch)
}

/// A row of mutually exclusive choices, the picked one raised.
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: impl Into<ElementId>,
    options: impl IntoIterator<Item = (T, impl Into<SharedString>)>,
    selected: T,
    cx: &mut Context<ThroccApp>,
    on_pick: impl Fn(&mut ThroccApp, T, &mut Context<ThroccApp>) + Clone + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .gap_1()
        .p_1()
        .rounded(theme::RADIUS_BUTTON)
        .bg(theme::surface())
        .children(options.into_iter().enumerate().map(|(i, (value, label))| {
            let on_pick = on_pick.clone();
            div()
                .id(i)
                .px_3()
                .py_1()
                .rounded(theme::RADIUS_ROW)
                .cursor_pointer()
                .when(value == selected, |s| s.bg(theme::raised()))
                .text_color(if value == selected {
                    theme::text()
                } else {
                    theme::text_3()
                })
                .child(label.into())
                .on_click(cx.listener(move |app, _, _, cx| {
                    on_pick(app, value, cx);
                    cx.notify();
                }))
        }))
}
