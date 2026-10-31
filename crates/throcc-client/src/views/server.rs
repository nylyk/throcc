use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::Disableable;
use gpui_component::input::Input;
use throcc_client_core::client::Command;
use throcc_proto::Fingerprint;
use throcc_proto::ids::Role;

use super::common::{avatar, caption, card, chip, ghost_icon, mono, primary, secondary, section};
use super::settings::{field, frame, nav_item, nav_list, save_line, segmented};
use crate::app::{Dialog, PictureEdit, PictureFor, Screen, ServerPage, ServerStatus, ThroccApp};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: ServerPage,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let is_admin = app.state.role == Some(Role::Admin);
    let item =
        |id: &'static str, label: &'static str, target: ServerPage, cx: &mut Context<ThroccApp>| {
            nav_item(id, label, page == target).on_click(cx.listener(move |app, _, window, cx| {
                if target == ServerPage::Overview {
                    app.fill_server(window, cx);
                }
                app.screen = Screen::Server(target);
                if target == ServerPage::Members && app.state.role == Some(Role::Admin) {
                    app.command(Command::ListUsers);
                }
                cx.notify();
            }))
        };
    let links = nav_list()
        .child(item("nav-server", "Server", ServerPage::Overview, cx))
        .child(item("nav-keys", "Privacy and keys", ServerPage::Keys, cx))
        .child(item(
            "nav-members",
            if is_admin { "Members" } else { "Invites" },
            ServerPage::Members,
            cx,
        ));
    let page_title = match page {
        ServerPage::Overview => "Server",
        ServerPage::Keys => "Privacy and keys",
        ServerPage::Members if is_admin => "Members",
        ServerPage::Members => "Invites",
    };
    let content = match page {
        ServerPage::Overview => overview(app, is_admin, cx),
        ServerPage::Keys => keys(app, cx),
        ServerPage::Members => members(app, cx),
    };
    frame(
        app.sidebar_width,
        "Server settings",
        links,
        page_title,
        content,
        cx,
    )
}

fn overview(app: &mut ThroccApp, is_admin: bool, cx: &mut Context<ThroccApp>) -> AnyElement {
    let Some(server) = app.state.server.clone() else {
        return div().into_any_element();
    };
    let picture = match &app.server_picture_edit {
        Some(edit) => edit.image.clone(),
        None => app.picture(server.avatar, cx),
    };
    let has_picture = picture.is_some();
    let saving = app.server_status == Some(ServerStatus::Saving);
    let picture_row = div()
        .flex()
        .items_center()
        .gap_4()
        .child(avatar(server.title(), 0, picture, px(64.), false))
        .children(is_admin.then(|| {
            div()
                .flex()
                .gap_2()
                .child(
                    secondary("choose-server-picture", "Choose picture…")
                        .disabled(saving)
                        .on_click(
                            cx.listener(|app, _, _, cx| app.choose_picture(PictureFor::Server, cx)),
                        ),
                )
                .children(has_picture.then(|| {
                    secondary("remove-server-picture", "Remove")
                        .disabled(saving)
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.server_status = None;
                            app.server_picture_edit = Some(PictureEdit::removed());
                            cx.notify();
                        }))
                }))
        }));
    let name = if is_admin {
        Input::new(&app.server_name).into_any_element()
    } else {
        div().child(server.title().to_owned()).into_any_element()
    };
    card()
        .flex()
        .flex_col()
        .gap_4()
        .child(picture_row)
        .child(field("Name", name))
        .child(field("Address", mono(server.address.clone())))
        .children(is_admin.then(|| {
            let status = app.server_status.clone().map(|status| match status {
                ServerStatus::Saving => caption("Saving…"),
                ServerStatus::Saved => caption("Saved.").text_color(theme::ok_text()),
                ServerStatus::Failed(reason) => caption(reason).text_color(theme::danger_text()),
            });
            let save = primary("save-server", "Save")
                .disabled(saving || !app.server_changed(cx))
                .on_click(cx.listener(|app, _, _, cx| app.save_server(cx)));
            save_line(status, save)
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
    div()
        .flex()
        .flex_col()
        .gap_6()
        .children(server.map(|(address, fingerprint)| {
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("This server"))
                .child(div().child(address))
                .child(
                    mono(fingerprint.to_string())
                        .text_size(px(12.))
                        .text_color(theme::text_2()),
                )
        }))
        .child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("People"))
                .children(state.people.iter().filter(|(u, _)| Some(**u) != me).map(
                    |(user, person)| {
                        let user = *user;
                        let changed = state.key_warnings.iter().any(|w| w.user == user);
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
                            .child(div().flex_1().child(person.name.clone()))
                            .child(if changed {
                                chip("Key changed", theme::warn_bg(), theme::warn_text())
                            } else if person.verified {
                                chip("Verified", theme::ok_bg(), theme::ok_text())
                            } else {
                                chip("Not verified", theme::raised(), theme::text_3())
                            })
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.dialog = Some(Dialog::Verify(user));
                                cx.notify();
                            }))
                    },
                )),
        )
        .into_any_element()
}

fn members(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let pictures = app.pictures_of_people(cx);
    let is_admin = app.state.role == Some(Role::Admin);
    let invite = app.invite.clone();
    let users = app.users.clone();
    let name_of = |id| users.iter().find(|u| u.id == id).map(|u| u.name.clone());
    let invite_card = card()
        .flex()
        .flex_col()
        .gap_3()
        .child(section("Invite"))
        .child(match &invite {
            Some((code, _)) => mono(code.clone())
                .text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .into_any_element(),
            None => div().into_any_element(),
        })
        .child(caption(if is_admin {
            "Single use, expires in 24 h."
        } else {
            "Single use, expires in 24 h. You can make 3 a day."
        }))
        .child(
            div()
                .flex()
                .gap_2()
                .children(invite.clone().map(|(code, _)| {
                    secondary("copy-invite", "Copy").on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()));
                    }))
                }))
                .child(
                    primary(
                        "new-invite",
                        if invite.is_some() {
                            "New code"
                        } else {
                            "Make a code"
                        },
                    )
                    .on_click(cx.listener(|app, _, _, _| app.command(Command::CreateInvite))),
                ),
        );
    let mut page = div().flex().flex_col().gap_6().child(invite_card);
    if is_admin {
        let my_id = app.state.me;
        page = page.child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section(format!("{} people", users.len())))
                .children(users.iter().map(|user| {
                    let id = user.id;
                    let name = user.name.clone();
                    let editable = user.role != Role::Admin && Some(id) != my_id;
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
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
                                        .map(|by| caption(format!("invited by {by}"))),
                                ),
                        )
                        .child(
                            mono(Fingerprint(user.identity_key).short())
                                .text_size(px(12.))
                                .text_color(theme::text_3()),
                        )
                        .child(if editable {
                            segmented(
                                ("role", id.0),
                                &[(Role::Manager, "Manager"), (Role::User, "User")],
                                user.role,
                                cx,
                                move |app, role, _| app.command(Command::SetRole(id, role)),
                            )
                            .into_any_element()
                        } else {
                            chip(format!("{:?}", user.role), theme::raised(), theme::text_2())
                                .into_any_element()
                        })
                        .children(editable.then(|| {
                            ghost_icon(("remove", id.0 as usize), "x", "Remove").on_click(
                                cx.listener(move |app, _, _, cx| {
                                    app.dialog = Some(Dialog::Remove {
                                        user: id,
                                        name: name.clone(),
                                        cascade: false,
                                    });
                                    cx.notify();
                                }),
                            )
                        }))
                })),
        );
    }
    page.into_any_element()
}
