use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::input::Input;
use throcc_client_core::client::Command;
use throcc_proto::ids::Role;

use super::common::{
    avatar, caption, card, chip, danger, ghost_icon, mono, primary, secondary, section, short_key,
    title,
};
use super::settings::{frame, nav, nav_item, row, segmented};
use crate::app::{Dialog, ProfileStatus, Screen, ServerPage, ThroccApp};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: ServerPage,
    _window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let is_admin = app.state.role == Some(Role::Admin);
    let address = app
        .state
        .server
        .as_ref()
        .map_or("This server".to_owned(), |s| s.address.clone());
    let item =
        |id: &'static str, label: &'static str, target: ServerPage, cx: &mut Context<ThroccApp>| {
            nav_item(id, label, page == target).on_click(cx.listener(move |app, _, window, cx| {
                app.screen = Screen::Server(target);
                match target {
                    ServerPage::Members => app.command(Command::ListUsers),
                    ServerPage::Profile => app.fill_profile(window, cx),
                    ServerPage::Keys => {}
                }
                cx.notify();
            }))
        };
    let nav = nav(cx)
        .child(
            div()
                .px_3()
                .pb_1()
                .flex()
                .items_center()
                .gap_2()
                .child(avatar(&address, 0, None, px(20.), false))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(address.clone()),
                ),
        )
        .child(item("nav-profile", "Profile", ServerPage::Profile, cx))
        .child(item("nav-keys", "Privacy and keys", ServerPage::Keys, cx))
        .child(item(
            "nav-members",
            if is_admin { "Members" } else { "Invites" },
            ServerPage::Members,
            cx,
        ))
        .child(div().flex_1())
        .child(
            danger("leave-this-server", "Leave this server")
                .w_full()
                .on_click(cx.listener(|app, _, _, cx| app.leave_server(cx))),
        );
    let content = match page {
        ServerPage::Profile => profile(app, cx),
        ServerPage::Keys => keys(app, cx),
        ServerPage::Members => members(app, cx),
    };
    frame(nav, content)
}

fn profile(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let me = app
        .state
        .me
        .and_then(|me| Some((me, app.state.person(me)?.clone())));
    let (seed, name, avatar_hash) = me.map_or((0, String::new(), None), |(me, person)| {
        (me.0, person.name, person.avatar)
    });
    let picture = match app.profile_preview.clone() {
        Some(preview) => preview,
        None => app.picture(avatar_hash),
    };
    let has_picture = picture.is_some();
    let picture_buttons = div()
        .flex()
        .gap_2()
        .child(
            secondary("choose-picture", "Choose picture…").on_click(cx.listener(
                |app, _, _, cx| {
                    app.profile_status = None;
                    app.choose_picture(cx);
                },
            )),
        )
        .children(has_picture.then(|| {
            secondary("remove-picture", "Remove").on_click(cx.listener(|app, _, _, cx| {
                app.profile_preview = Some(None);
                app.profile_status = Some(app.saving_status());
                app.command(Command::SetAvatar(None));
                cx.notify();
            }))
        }));
    let name_field = div()
        .flex()
        .gap_2()
        .child(div().flex_1().child(Input::new(&app.name)))
        .child(
            primary("save-name", "Save").on_click(cx.listener(|app, _, _, cx| {
                let name = app.name.read(cx).value().trim().to_owned();
                if !name.is_empty() {
                    app.profile_status = Some(ProfileStatus::Saving);
                    app.command(Command::SetName(name));
                }
                cx.notify();
            })),
        );
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(title("Profile"))
        .child(
            card()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(avatar(&name, seed, picture, px(64.), false))
                        .child(picture_buttons),
                )
                .child(row("Name", name_field))
                .children(app.profile_status.clone().map(|status| match status {
                    ProfileStatus::Saving => caption("Saving…"),
                    ProfileStatus::Saved => caption("Saved.").text_color(theme::ok_text()),
                    ProfileStatus::UntilJoined => {
                        caption("Saved here. Others see your picture once you join a room.")
                    }
                    ProfileStatus::Failed(reason) => {
                        caption(format!("Not saved: {reason}")).text_color(theme::danger_text())
                    }
                }))
                .child(caption(
                    "Everyone on this server sees your name and picture. You can change them once every 10 seconds.",
                )),
        )
        .into_any_element()
}

fn keys(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let pictures = app.pictures_of_people();
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
        .child(title("Privacy and keys"))
        .children(server.map(|(address, fingerprint)| {
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("This server"))
                .child(div().child(address))
                .child(mono(fingerprint.to_string()).text_size(px(12.)).text_color(theme::text_2()))
                .child(caption("Media is end-to-end encrypted. The server sees who is in which room, profiles and room names, not what anyone says or shows."))
        }))
        .child(
            card().flex().flex_col().gap_1().child(section("People")).children(
                state.people.iter().filter(|(u, _)| Some(**u) != me).map(|(user, person)| {
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
                        .child(avatar(&person.name, user.0, pictures.get(&user).cloned(), px(28.), false))
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
                }),
            ),
        )
        .into_any_element()
}

fn members(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let pictures = app.pictures_of_people();
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
            None => caption("Make a code and send it to the person you want to invite.")
                .into_any_element(),
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
    let mut page = div()
        .flex()
        .flex_col()
        .gap_6()
        .child(title(if is_admin { "Members" } else { "Invites" }))
        .child(invite_card);
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
                            mono(short_key(&user.identity_key))
                                .text_size(px(12.))
                                .text_color(theme::text_3()),
                        )
                        .child(if editable {
                            segmented(
                                "role",
                                &[(Role::Manager, "Manager"), (Role::User, "User")],
                                user.role,
                                cx,
                                move |app, role| app.command(Command::SetRole(id, role)),
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
        page = page.child(caption(
            "Removing someone can also remove everyone they invited.",
        ));
    }
    page.into_any_element()
}
