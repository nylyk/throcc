use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::Disableable;
use gpui_component::input::Input;
use throcc_client_core::client::Command;
use throcc_proto::ids::Role;
use throcc_proto::{Fingerprint, permissions};

use super::common::{
    Trust, Typeset, avatar, caption, card, chip, field_error, ghost_icon, mono, primary, secondary,
    section, server_seed, trust, verified_as,
};
use super::page::{field, frame, nav, picture_editor, save_line, segmented};
use crate::app::{Dialog, PictureFor, Screen, ServerPage, ThroccApp, typed_name};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: ServerPage,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let links = nav(
        ServerPage::ALL.map(|p| (p, p.title(), p.icon())),
        page,
        Screen::Server,
        cx,
    );
    let content = match page {
        ServerPage::Overview => overview(app, cx),
        ServerPage::Keys => keys(app, cx),
        ServerPage::Members => members(app, cx),
    };
    frame(
        app.sidebar_width,
        "Server Settings",
        links,
        page.title(),
        content,
        cx,
    )
}

fn overview(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let Some(server) = app.state.server.clone() else {
        return div().into_any_element();
    };
    let editable = app.state.role.is_some_and(permissions::can_edit_server);
    let picture = picture_editor(
        app,
        PictureFor::Server,
        server.title(),
        server_seed(&server.address),
        server.avatar,
        editable,
        cx,
    );
    let name = if editable {
        let error = typed_name(&app.server_name, cx).err();
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(Input::new(&app.server_name))
            .children(error.map(field_error))
            .into_any_element()
    } else {
        div().child(server.title().to_owned()).into_any_element()
    };
    card()
        .flex()
        .flex_col()
        .gap_4()
        .child(picture)
        .child(field("Name", name))
        .child(field("Address", mono(server.address.clone())))
        .children(editable.then(|| {
            let save = primary("save-server", "Save")
                .disabled(app.server_form.saving() || !app.server_changed(cx))
                .on_click(cx.listener(|app, _, _, cx| app.save_server(cx)));
            save_line(app.server_form.status.as_ref(), str::to_owned, save)
        }))
        .into_any_element()
}

fn keys(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let pictures = app.pictures_of_people(cx);
    let state = app.state.clone();
    let server = state
        .server
        .as_ref()
        .map(|s| (s.address.clone(), s.fingerprint));
    let me = state.me;
    let verified = app.verified.list();
    div()
        .flex()
        .flex_col()
        .gap_6()
        .children(server.map(|(address, fingerprint)| {
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("This Server"))
                .child(mono(address))
                .child(
                    mono(fingerprint.to_string())
                        .typeset(theme::CAPTION)
                        .text_color(theme::text_2()),
                )
        }))
        .child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("People Here"))
                .children(state.people.iter().filter(|(u, _)| Some(**u) != me).map(
                    |(user, person)| {
                        let user = *user;
                        let verified_as = verified_as(&verified, person);
                        div()
                            .id(("contact", user.0 as usize))
                            .flex()
                            .items_center()
                            .gap_3()
                            .min_h(theme::TOUCH)
                            .px_2()
                            .rounded(theme::RADIUS_ROW)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::raised()))
                            .child(avatar(
                                &person.name,
                                user.0,
                                pictures.get(&user).cloned(),
                                px(28.),
                                false,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().truncate().child(person.name.clone()))
                                    .children(verified_as.map(|name| {
                                        caption(format!("Verified as {name}")).truncate()
                                    })),
                            )
                            .child(match trust(&state, user) {
                                Trust::KeyChanged => {
                                    chip("Key changed", theme::danger_bg(), theme::danger_text())
                                }
                                Trust::Verified => {
                                    chip("Verified", theme::ok_bg(), theme::ok_text())
                                }
                                Trust::Unverified => {
                                    chip("Not verified", theme::raised(), theme::text_3())
                                }
                            })
                            .on_click(cx.listener(move |app, _, _, cx| app.open_profile(user, cx)))
                    },
                )),
        )
        .into_any_element()
}

fn members(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let pictures = app.pictures_of_people(cx);
    let role = app.state.role;
    let invite = app.invite.clone();
    let users = app.users.clone();
    let name_of = |id| users.iter().find(|u| u.id == id).map(|u| u.name.clone());
    let invite_card = card()
        .flex()
        .flex_col()
        .gap_3()
        .child(section("Invite"))
        .children(
            invite
                .as_ref()
                .map(|code| mono(code.clone()).typeset(theme::TITLE_1)),
        )
        .children(invite.is_some().then(|| {
            caption(match role.and_then(permissions::invite_cap) {
                Some(cap) => format!("Single use, expires in 24 hours. You can make {cap} a day."),
                None => "Single use, expires in 24 hours.".to_owned(),
            })
        }))
        .child(
            div()
                .flex()
                .gap_2()
                .children(invite.clone().map(|code| {
                    secondary("copy-invite", "Copy").on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()));
                    }))
                }))
                .child(
                    primary(
                        "new-invite",
                        if invite.is_some() {
                            "New Code"
                        } else {
                            "Create Code"
                        },
                    )
                    .on_click(cx.listener(|app, _, _, _| app.command(Command::CreateInvite))),
                ),
        );
    let mut page = div().flex().flex_col().gap_6().child(invite_card);
    if let Some(me) = role.filter(|r| permissions::can_list_users(*r)) {
        let my_id = app.state.me;
        page = page.child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section(format!("{} People", users.len())))
                .children(users.iter().map(|user| {
                    let id = user.id;
                    let name = user.name.clone();
                    let editable = Some(id) != my_id && permissions::can_remove(me, user.role);
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x_3()
                        .gap_y_1()
                        .min_h(theme::TOUCH)
                        .child(avatar(
                            &user.name,
                            id.0,
                            pictures.get(&id).cloned(),
                            px(28.),
                            false,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .child(div().child(format!(
                                    "{}{}",
                                    user.name,
                                    if Some(id) == my_id { " (you)" } else { "" }
                                )))
                                .children(
                                    user.invited_by
                                        .and_then(name_of)
                                        .map(|by| caption(format!("Invited by {by}"))),
                                ),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    mono(Fingerprint(user.identity_key).short())
                                        .typeset(theme::CAPTION)
                                        .text_color(theme::text_3()),
                                )
                                .child(if editable {
                                    segmented(
                                        ("role", id.0),
                                        [(Role::Manager, "Manager"), (Role::User, "User")]
                                            .into_iter()
                                            .filter(|(role, _)| {
                                                permissions::can_set_role(me, user.role, *role)
                                            }),
                                        user.role,
                                        cx,
                                        move |app, role, _| app.command(Command::SetRole(id, role)),
                                    )
                                    .into_any_element()
                                } else {
                                    chip(
                                        format!("{:?}", user.role),
                                        theme::raised(),
                                        theme::text_2(),
                                    )
                                    .into_any_element()
                                })
                                .children(editable.then(|| {
                                    ghost_icon(("remove", id.0 as usize), "x", "Remove").on_click(
                                        cx.listener(move |app, _, _, cx| {
                                            let dialog = Dialog::Remove {
                                                user: id,
                                                name: name.clone(),
                                                cascade: false,
                                            };
                                            app.set_dialog(Some(dialog), cx);
                                        }),
                                    )
                                })),
                        )
                })),
        );
    }
    page.into_any_element()
}
