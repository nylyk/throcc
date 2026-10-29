use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::input::Input;
use gpui_component::switch::Switch;
use throcc_client_core::client::Command;
use throcc_client_core::media::audio::{
    DEFAULT_VAD_THRESHOLD, NoiseSuppressionLevel, Transmit, VoiceSettings,
};
use throcc_proto::ids::Role;

use super::common::{
    avatar, caption, card, chip, ghost_icon, mono, primary, secondary, section, short_key, title,
};
use crate::app::{Dialog, Screen, SettingsPage, ThroccApp};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: SettingsPage,
    _window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let is_admin = app.state.role == Some(Role::Admin);
    let nav_item = |id: &'static str,
                    label: &'static str,
                    target: SettingsPage,
                    cx: &mut Context<ThroccApp>| {
        div()
            .id(id)
            .px_3()
            .min_h(px(36.))
            .flex()
            .items_center()
            .rounded(theme::RADIUS_ROW)
            .cursor_pointer()
            .when_page(page == target)
            .hover(|s| s.bg(theme::raised()))
            .child(label)
            .on_click(cx.listener(move |app, _, window, cx| {
                app.screen = Screen::Settings(target);
                match target {
                    SettingsPage::Members => app.command(Command::ListUsers),
                    SettingsPage::Profile => app.fill_profile(window, cx),
                    _ => {}
                }
                cx.notify();
            }))
    };
    let nav = div()
        .w(theme::SIDEBAR_WIDTH)
        .h_full()
        .flex_none()
        .bg(theme::sidebar())
        .border_r_1()
        .border_color(theme::control())
        .p_3()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            secondary("settings-back", "Back")
                .w_full()
                .on_click(cx.listener(|app, _, _, cx| {
                    app.screen = Screen::Main;
                    cx.notify();
                })),
        )
        .child(div().h_2())
        .child(nav_item(
            "nav-profile",
            "Profile",
            SettingsPage::Profile,
            cx,
        ))
        .child(nav_item("nav-audio", "Audio", SettingsPage::Audio, cx))
        .child(nav_item(
            "nav-keys",
            "Privacy and keys",
            SettingsPage::Keys,
            cx,
        ))
        .child(nav_item(
            "nav-members",
            if is_admin { "Members" } else { "Invites" },
            SettingsPage::Members,
            cx,
        ))
        .child(nav_item(
            "nav-servers",
            "Servers",
            SettingsPage::Servers,
            cx,
        ));

    let content = match page {
        SettingsPage::Profile => profile(app, cx),
        SettingsPage::Audio => audio(app, cx),
        SettingsPage::Keys => keys(app, cx),
        SettingsPage::Members => members(app, cx),
        SettingsPage::Servers => servers(app, cx),
    };
    div().size_full().flex().child(nav).child(
        div()
            .id("settings-content")
            .flex_1()
            .overflow_y_scroll()
            .p_8()
            .child(div().max_w(px(640.)).child(content)),
    )
}

trait PageHighlight {
    fn when_page(self, selected: bool) -> Self;
}

impl<T: Styled> PageHighlight for T {
    fn when_page(self, selected: bool) -> Self {
        if selected {
            self.bg(theme::raised())
        } else {
            self
        }
    }
}

fn row(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .min_h(theme::TOUCH)
        .child(
            div()
                .w(px(180.))
                .text_color(theme::text_2())
                .child(label.to_owned()),
        )
        .child(div().flex_1().child(control))
}

/// A row of mutually exclusive choices.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    selected: T,
    cx: &mut Context<ThroccApp>,
    on_pick: impl Fn(&mut ThroccApp, T) + Clone + 'static,
) -> Div {
    div()
        .flex()
        .gap_1()
        .p_1()
        .rounded(theme::RADIUS_BUTTON)
        .bg(theme::surface())
        .children(options.iter().enumerate().map(|(i, (value, label))| {
            let value = *value;
            let on_pick = on_pick.clone();
            div()
                .id((id, i))
                .px_3()
                .py_1()
                .rounded(theme::RADIUS_ROW)
                .cursor_pointer()
                .when_page(value == selected)
                .text_color(if value == selected {
                    theme::text()
                } else {
                    theme::text_3()
                })
                .child(*label)
                .on_click(cx.listener(move |app, _, _, cx| {
                    on_pick(app, value);
                    app.sync_push_to_talk(cx);
                    cx.notify();
                }))
        }))
}

fn set_voice(app: &mut ThroccApp, change: impl FnOnce(&mut VoiceSettings)) {
    change(&mut app.voice);
    app.command(Command::Voice(app.voice));
}

fn audio(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let voice = app.voice;
    let vad = matches!(voice.transmit, Transmit::VoiceActivity { .. });
    let device_list = |id: &'static str,
                       names: Vec<String>,
                       selected: Option<String>,
                       input: bool,
                       cx: &mut Context<ThroccApp>| {
        let mut choices: Vec<(Option<String>, String)> = vec![(None, "System default".into())];
        choices.extend(names.into_iter().map(|n| (Some(n.clone()), n)));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(choices.into_iter().enumerate().map(|(i, (value, label))| {
                let picked = value == selected;
                div()
                    .id((id, i))
                    .px_3()
                    .py_1()
                    .rounded(theme::RADIUS_ROW)
                    .cursor_pointer()
                    .when_page(picked)
                    .hover(|s| s.bg(theme::raised()))
                    .text_color(if picked {
                        theme::text()
                    } else {
                        theme::text_2()
                    })
                    .child(label)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        if input {
                            app.input_device = value.clone();
                        } else {
                            app.output_device = value.clone();
                        }
                        app.command(Command::Devices {
                            input: app.input_device.clone(),
                            output: app.output_device.clone(),
                        });
                        cx.notify();
                    }))
            }))
    };
    let threshold = match voice.transmit {
        Transmit::VoiceActivity { threshold } => threshold,
        Transmit::PushToTalk => DEFAULT_VAD_THRESHOLD,
    };
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(title("Audio"))
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Devices"))
                .child(row(
                    "Input",
                    device_list(
                        "input",
                        app.inputs.clone(),
                        app.input_device.clone(),
                        true,
                        cx,
                    ),
                ))
                .child(row(
                    "Output",
                    device_list(
                        "output",
                        app.outputs.clone(),
                        app.output_device.clone(),
                        false,
                        cx,
                    ),
                )),
        )
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Transmit"))
                .child(row(
                    "Mode",
                    segmented(
                        "mode",
                        &[(true, "Voice activity"), (false, "Push to talk")],
                        vad,
                        cx,
                        move |app, voice_activity| {
                            set_voice(app, |v| {
                                v.transmit = if voice_activity {
                                    Transmit::VoiceActivity { threshold }
                                } else {
                                    Transmit::PushToTalk
                                }
                            })
                        },
                    ),
                ))
                .children(vad.then(|| {
                    row(
                        "Sensitivity",
                        segmented(
                            "sensitivity",
                            &[(0.3f32, "High"), (0.5, "Normal"), (0.7, "Low")],
                            threshold,
                            cx,
                            |app, threshold| {
                                set_voice(app, |v| {
                                    v.transmit = Transmit::VoiceActivity { threshold }
                                })
                            },
                        ),
                    )
                }))
                .children((!vad).then(|| {
                    row(
                        "Push-to-talk key",
                        mono("Alt + Space anywhere, or Space in this window"),
                    )
                })),
        )
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Processing"))
                .child(
                    Switch::new("aec")
                        .checked(voice.echo_cancellation)
                        .label("Echo cancellation")
                        .on_click(cx.listener(|app, on: &bool, _, cx| {
                            set_voice(app, |v| v.echo_cancellation = *on);
                            cx.notify();
                        })),
                )
                .child(row(
                    "Noise suppression",
                    segmented(
                        "ns",
                        &[
                            (None, "Off"),
                            (Some(NoiseSuppressionLevel::Low), "Low"),
                            (Some(NoiseSuppressionLevel::Moderate), "Moderate"),
                            (Some(NoiseSuppressionLevel::High), "High"),
                        ],
                        voice.noise_suppression,
                        cx,
                        |app, level| set_voice(app, |v| v.noise_suppression = level),
                    ),
                ))
                .child(
                    Switch::new("agc")
                        .checked(voice.automatic_gain)
                        .label("Automatic gain")
                        .on_click(cx.listener(|app, on: &bool, _, cx| {
                            set_voice(app, |v| v.automatic_gain = *on);
                            cx.notify();
                        })),
                ),
        )
        .child(
            div().flex().child(
                primary(
                    "mic-test",
                    if app.mic_test {
                        "Stop test"
                    } else {
                        "Test microphone"
                    },
                )
                .on_click(cx.listener(|app, _, _, cx| {
                    app.mic_test = !app.mic_test;
                    app.command(Command::MicTest(app.mic_test));
                    cx.notify();
                })),
            ),
        )
        .child(caption(
            "The test plays your processed microphone back to you, with echo cancellation applied.",
        ))
        .into_any_element()
}

fn profile(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let me = app
        .state
        .me
        .and_then(|me| Some((me, app.state.person(me)?.clone())));
    let (seed, name, avatar_hash) = me.map_or((0, String::new(), None), |(me, person)| {
        (me.0, person.name, person.avatar)
    });
    let picture = app.picture(avatar_hash);
    let picture_buttons = div()
        .flex()
        .gap_2()
        .child(
            secondary("choose-picture", "Choose picture…").on_click(cx.listener(
                |app, _, _, cx| {
                    app.profile_error = None;
                    app.choose_picture(cx);
                },
            )),
        )
        .children(avatar_hash.map(|_| {
            secondary("remove-picture", "Remove").on_click(cx.listener(|app, _, _, cx| {
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
                .children(
                    app.profile_error
                        .clone()
                        .map(|error| caption(error).text_color(theme::danger_text())),
                )
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

fn servers(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let current = app.state.server.as_ref().map(|s| s.address.clone());
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(title("Servers"))
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .children(app.known_servers.iter().enumerate().map(
                    |(i, (address, fingerprint))| {
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .min_h(theme::TOUCH)
                            .child(avatar(address, i as u64, None, px(28.), false))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(div().child(address.clone()))
                                    .child(
                                        mono(fingerprint.short())
                                            .text_size(px(12.))
                                            .text_color(theme::text_3()),
                                    ),
                            )
                            .children(
                                (current.as_deref() == Some(address.as_str()))
                                    .then(|| chip("Connected", theme::ok_bg(), theme::ok_text())),
                            )
                    },
                )),
        )
        .child(
            secondary("disconnect", "Leave this server")
                .on_click(cx.listener(|app, _, _, cx| app.leave_server(cx))),
        )
        .into_any_element()
}
