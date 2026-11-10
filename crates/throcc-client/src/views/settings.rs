use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Div, ElementId, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Window, div, px, relative,
};
use gpui_component::input::Input;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{Disableable, IndexPath};
use throcc_client_core::media::cues::Cue;
use throcc_client_core::media::voice::MicLevel;
use throcc_client_core::settings::{
    AudioSettings, DEFAULT_GATE_DBFS, NoiseSuppression, Transmit, VoiceSettings,
};
use throcc_proto::Fingerprint;

use super::common::{
    Typeset, caption, card, field_error, ghost_icon, key_cap, mono, primary, section, slider_gain,
    slider_value, update_notice, volume_slider,
};
use super::page::{field, frame, nav, picture_editor, row, save_line, segmented, switch_row};
use crate::app::{DeviceNames, PictureFor, Screen, SettingsPage, ThroccApp, typed_name};
use crate::hotkey::DEFAULT_KEY_LABEL;
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: SettingsPage,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let links = nav(
        SettingsPage::ALL.map(|p| (p, p.title(), p.icon())),
        page,
        Screen::Settings,
        cx,
    );
    let content = match page {
        SettingsPage::Profile => profile(app, cx),
        SettingsPage::Audio => audio(app, cx),
        SettingsPage::Appearance => appearance(app, cx),
        SettingsPage::Verified => verified(app, cx),
        SettingsPage::About => about(app, cx),
    };
    let content = match app.settings_problem.clone() {
        Some(problem) => div()
            .flex()
            .flex_col()
            .gap_6()
            .child(field_error(problem))
            .child(content)
            .into_any_element(),
        None => content,
    };
    frame(
        app.sidebar_width,
        "Settings",
        links,
        page.title(),
        content,
        cx,
    )
}

/// The voice-activity gate in `transmit`, or the default one for push to talk.
fn gate_dbfs(transmit: Transmit) -> f32 {
    match transmit {
        Transmit::VoiceActivity { threshold_dbfs } => threshold_dbfs,
        Transmit::PushToTalk => DEFAULT_GATE_DBFS,
    }
}

/// Changes the voice settings and gives them to the audio thread.
fn change_voice(app: &mut ThroccApp, change: impl FnOnce(&mut VoiceSettings)) {
    change(&mut app.settings.audio.voice);
    app.audio_engine.apply(app.settings.audio.voice);
}

fn set_voice(app: &mut ThroccApp, change: impl FnOnce(&mut VoiceSettings)) {
    change_voice(app, change);
    app.save_settings();
}

/// Applies a slider's voice setting as it moves, and keeps it once let go.
fn slide_voice(app: &mut ThroccApp, event: &SliderEvent, change: impl FnOnce(&mut VoiceSettings)) {
    change_voice(app, change);
    if matches!(event, SliderEvent::Release(_)) {
        app.save_settings();
    }
}

fn profile(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let name = app
        .settings
        .profile
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let saved = app.profile_avatar;
    let picture = picture_editor(app, PictureFor::Profile, &name, 0, saved, true, cx);
    let name_error = typed_name(&app.name, cx).err();
    let save = primary("save-profile", "Save")
        .disabled(!app.profile_changed(cx))
        .on_click(cx.listener(|app, _, window, cx| app.save_profile(window, cx)));
    let save = save_line(app.profile_form.status.as_ref(), save);
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            card()
                .flex()
                .flex_col()
                .gap_4()
                .child(picture)
                .child(field("Name", Input::new(&app.name)).children(name_error.map(field_error)))
                .child(save),
        )
        .into_any_element()
}

const SYSTEM_DEFAULT: &str = "System default";
/// The quietest level the meter and the threshold slider show.
const METER_FLOOR_DBFS: f32 = -80.0;
const METER_EVERY: Duration = Duration::from_millis(100);

type Devices = Vec<SharedString>;

/// The audio page's dropdowns and sliders, which keep their state between renders.
pub struct AudioControls {
    input: Entity<SelectState<Devices>>,
    output: Entity<SelectState<Devices>>,
    input_volume: Entity<SliderState>,
    output_volume: Entity<SliderState>,
    cue_volume: Entity<SliderState>,
    threshold: Entity<SliderState>,
    /// Whether a timer redraws the level meter.
    metering: bool,
    _subscriptions: Vec<Subscription>,
}

impl AudioControls {
    /// The controls for `voice`, offering only the system default devices until
    /// [`Self::show_devices`] gives the rest.
    pub fn new(voice: &VoiceSettings, window: &mut Window, cx: &mut Context<ThroccApp>) -> Self {
        let mut device_select = || {
            let items = device_items(&[]);
            cx.new(|cx| SelectState::new(items, Some(IndexPath::new(0)), window, cx))
        };
        let input = device_select();
        let output = device_select();
        let input_volume = cx.new(|_| volume_slider(voice.input_volume));
        let output_volume = cx.new(|_| volume_slider(voice.output_volume));
        let cue_volume = cx.new(|_| volume_slider(voice.cue_volume));
        let threshold = cx.new(|_| {
            SliderState::new()
                .min(METER_FLOOR_DBFS)
                .max(0.)
                .step(1.)
                .default_value(gate_dbfs(voice.transmit))
        });
        let on_device = |is_input: bool| {
            move |app: &mut ThroccApp,
                  _: &Entity<SelectState<Devices>>,
                  event: &SelectEvent<Devices>,
                  window: &mut Window,
                  cx: &mut Context<ThroccApp>| {
                let SelectEvent::Confirm(picked) = event;
                let device = picked
                    .as_ref()
                    .filter(|name| name.as_ref() != SYSTEM_DEFAULT)
                    .map(ToString::to_string);
                app.change_settings(|s| {
                    if is_input {
                        s.audio.input_device = device;
                    } else {
                        s.audio.output_device = device;
                    }
                });
                app.change_devices(window, cx);
                cx.notify();
            }
        };
        let on_slide = |change: fn(&mut VoiceSettings, &SliderEvent)| {
            move |app: &mut ThroccApp, _, event: &SliderEvent, cx: &mut Context<ThroccApp>| {
                slide_voice(app, event, |voice| change(voice, event));
                cx.notify();
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&input, window, on_device(true)),
            cx.subscribe_in(&output, window, on_device(false)),
            cx.subscribe(
                &input_volume,
                on_slide(|voice, event| voice.input_volume = slider_gain(event)),
            ),
            cx.subscribe(
                &output_volume,
                on_slide(|voice, event| voice.output_volume = slider_gain(event)),
            ),
            cx.subscribe(&cue_volume, |app, _, event: &SliderEvent, cx| {
                slide_voice(app, event, |voice| voice.cue_volume = slider_gain(event));
                if matches!(event, SliderEvent::Release(_)) {
                    app.audio_engine.cue(Cue::Join);
                }
                cx.notify();
            }),
            cx.subscribe(
                &threshold,
                on_slide(|voice, event| {
                    if let Transmit::VoiceActivity { threshold_dbfs } = &mut voice.transmit {
                        *threshold_dbfs = slider_value(event);
                    }
                }),
            ),
        ];
        Self {
            input,
            output,
            input_volume,
            output_volume,
            cue_volume,
            threshold,
            metering: false,
            _subscriptions: subscriptions,
        }
    }

    /// Offers `names` in the device dropdowns, with the devices `audio` names picked.
    pub fn show_devices(
        &self,
        (inputs, outputs): &DeviceNames,
        audio: &AudioSettings,
        window: &mut Window,
        cx: &mut Context<ThroccApp>,
    ) {
        let pairs = [
            (&self.input, inputs, &audio.input_device),
            (&self.output, outputs, &audio.output_device),
        ];
        for (select, names, picked) in pairs {
            let row = picked
                .as_ref()
                .and_then(|picked| names.iter().position(|name| name == picked))
                .map_or(0, |i| i + 1);
            select.update(cx, |select, cx| {
                select.set_items(device_items(names), window, cx);
                select.set_selected_index(Some(IndexPath::new(row)), window, cx);
            });
        }
    }
}

/// The system default followed by `names`.
fn device_items(names: &[String]) -> Devices {
    let mut items: Devices = vec![SYSTEM_DEFAULT.into()];
    items.extend(names.iter().cloned().map(SharedString::from));
    items
}

/// Redraws the page every 100 ms while it shows the audio settings with a level meter that
/// can move, and stops the mic test once the page is left.
fn keep_metering(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) {
    if app.audio.metering {
        return;
    }
    app.audio.metering = true;
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(METER_EVERY).await;
            let shown = this.update(cx, |app, cx| {
                let shown = app.screen == Screen::Settings(SettingsPage::Audio);
                app.audio.metering = shown;
                let meter_shown = matches!(
                    app.settings.audio.voice.transmit,
                    Transmit::VoiceActivity { .. }
                );
                if !shown {
                    app.set_mic_test(false);
                } else if meter_shown {
                    cx.notify();
                }
                shown
            });
            if !matches!(shown, Ok(true)) {
                return;
            }
        }
    })
    .detach();
}

/// The mic level as a bar, green while the gate lets it through, with a mark at
/// `threshold_dbfs`.
fn meter(level: MicLevel, threshold_dbfs: f32) -> Div {
    let fraction = |dbfs: f32| (1. - dbfs / METER_FLOOR_DBFS).clamp(0., 1.);
    div()
        .relative()
        .h(px(8.))
        .rounded_full()
        .overflow_hidden()
        .bg(theme::control())
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(relative(fraction(level.dbfs)))
                .bg(if level.gate_open {
                    theme::ok_text()
                } else {
                    theme::text_3()
                }),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(px(2.))
                .left(relative(fraction(threshold_dbfs)))
                .bg(theme::text()),
        )
}

fn percent_slider(state: &Entity<SliderState>, cx: &Context<ThroccApp>) -> Div {
    let percent = state.read(cx).value().start().round();
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(div().flex_1().child(Slider::new(state)))
        .child(
            div()
                .child(format!("{percent:.0}%"))
                .w(px(64.))
                .flex_none()
                .text_right(),
        )
}

fn audio(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    keep_metering(app, cx);
    let voice = app.settings.audio.voice;
    let vad = matches!(voice.transmit, Transmit::VoiceActivity { .. });
    let threshold = gate_dbfs(voice.transmit);
    let level = app.audio_engine.mic_level();
    let controls = &app.audio;
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Devices"))
                .children(app.audio_problem.clone().map(field_error))
                .child(row("Input", Select::new(&controls.input).w_full()))
                .child(row(
                    "Input volume",
                    percent_slider(&controls.input_volume, cx),
                ))
                .child(row("Output", Select::new(&controls.output).w_full()))
                .child(row(
                    "Output volume",
                    percent_slider(&controls.output_volume, cx),
                )),
        )
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Notification sounds"))
                .child(row("Volume", percent_slider(&controls.cue_volume, cx))),
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
                        [(true, "Voice Activity"), (false, "Push to Talk")],
                        vad,
                        cx,
                        |app, voice_activity, cx| {
                            let threshold_dbfs = app.audio.threshold.read(cx).value().start();
                            set_voice(app, |v| {
                                v.transmit = if voice_activity {
                                    Transmit::VoiceActivity { threshold_dbfs }
                                } else {
                                    Transmit::PushToTalk
                                }
                            });
                            app.sync_push_to_talk();
                        },
                    ),
                ))
                .children(vad.then(|| {
                    row(
                        "Sensitivity",
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(meter(level, threshold))
                                    .child(Slider::new(&controls.threshold)),
                            )
                            .child(
                                div()
                                    .child(format!("{threshold:.0} dB").replace('-', "−"))
                                    .w(px(64.))
                                    .flex_none()
                                    .text_right(),
                            ),
                    )
                }))
                .children(vad.then(|| {
                    switch_row(
                        Switch::new("speech-only")
                            .checked(voice.speech_only)
                            .label("Speech detection")
                            .on_click(cx.listener(|app, on: &bool, _, cx| {
                                set_voice(app, |v| v.speech_only = *on);
                                cx.notify();
                            })),
                    )
                }))
                .children((!vad).then(|| {
                    let global = app
                        .has_global_push_to_talk()
                        .then(|| [key_cap(DEFAULT_KEY_LABEL), caption("anywhere, or")]);
                    row(
                        "Push-to-talk key",
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .children(global.into_iter().flatten())
                            .child(key_cap("Space"))
                            .child(caption("while this window is in front")),
                    )
                })),
        )
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("Processing"))
                .child(row(
                    "Noise suppression",
                    segmented(
                        "ns",
                        [
                            (None, "Off"),
                            (Some(NoiseSuppression::Low), "Low"),
                            (Some(NoiseSuppression::Moderate), "Moderate"),
                            (Some(NoiseSuppression::High), "High"),
                        ],
                        voice.noise_suppression,
                        cx,
                        |app, level, _| set_voice(app, |v| v.noise_suppression = level),
                    ),
                ))
                .child(switch_row(
                    Switch::new("aec")
                        .checked(voice.echo_cancellation)
                        .label("Echo cancellation")
                        .on_click(cx.listener(|app, on: &bool, _, cx| {
                            set_voice(app, |v| v.echo_cancellation = *on);
                            cx.notify();
                        })),
                ))
                .child(switch_row(
                    Switch::new("agc")
                        .checked(voice.automatic_gain)
                        .label("Automatic gain")
                        .on_click(cx.listener(|app, on: &bool, _, cx| {
                            set_voice(app, |v| v.automatic_gain = *on);
                            cx.notify();
                        })),
                )),
        )
        .child(
            div().flex().child(
                primary(
                    "mic-test",
                    if app.mic_test {
                        "Stop Test"
                    } else {
                        "Test Microphone"
                    },
                )
                .on_click(cx.listener(|app, _, _, cx| {
                    app.set_mic_test(!app.mic_test);
                    cx.notify();
                })),
            ),
        )
        .into_any_element()
}

fn about(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let updates = card()
        .flex()
        .flex_col()
        .gap_2()
        .child(section("Updates"))
        .child(switch_row(
            Switch::new("check-for-updates")
                .checked(app.checks_for_updates())
                .label("Check for a new version when throcc starts")
                .on_click(cx.listener(|app, on: &bool, _, cx| app.use_update_check(*on, cx))),
        ))
        .children(app.newer_release.as_ref().map(update_notice));
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("throcc"))
                .child(row("Version", div().child(env!("CARGO_PKG_VERSION")))),
        )
        .child(updates)
        .into_any_element()
}

/// Everyone verified on this computer, on any server, each with the servers they were seen
/// on and a way to take them off.
fn verified(app: &ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let address_of = |server: &Fingerprint| {
        app.known_servers
            .iter()
            .find(|(_, known)| known == server)
            .map_or_else(|| server.short(), |(address, _)| address.clone())
    };
    let people = app.verified.list();
    let rows = people.iter().enumerate().map(|(i, person)| {
        let key = person.key;
        let servers: Vec<String> = person.servers.iter().map(address_of).collect();
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(theme::TOUCH)
            .px_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(person.name.clone()))
                    .child(
                        div()
                            .child(servers.join(", "))
                            .truncate()
                            .typeset(theme::CAPTION)
                            .text_color(theme::text_3()),
                    ),
            )
            .child(
                mono(key.short())
                    .typeset(theme::CAPTION)
                    .text_color(theme::text_3()),
            )
            .child(
                ghost_icon(("unverify", i), "x", "Mark as Not Verified").on_click(cx.listener(
                    move |app, _, _, cx| {
                        app.unverify(key);
                        cx.notify();
                    },
                )),
            )
    });
    card()
        .flex()
        .flex_col()
        .gap_1()
        .child(section("People that you have verified"))
        .children(rows)
        .children(
            people.is_empty().then(|| {
                caption("Nobody yet. Compare safety numbers with someone to verify them.")
            }),
        )
        .children(app.verified_problem.clone().map(field_error))
        .into_any_element()
}

fn appearance(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let (picked, _) = theme::picked(&app.settings.appearance);
    let custom_colors = theme::custom(&app.settings.appearance);
    let custom = picked == theme::CUSTOM;
    let option = |id: ElementId, name: &'static str, palette, cx: &mut Context<ThroccApp>| {
        theme_option(id, name, palette, picked == name)
            .on_click(cx.listener(move |app, _, _, cx| app.use_theme(name, cx)))
    };
    let presets: Vec<_> = theme::PRESETS
        .iter()
        .enumerate()
        .map(|(i, &(name, palette))| option(("preset", i).into(), name, palette, cx))
        .collect();
    let custom_option = option("custom".into(), theme::CUSTOM, custom_colors, cx);
    let colors = theme::Palette::LABELS
        .iter()
        .zip(custom_colors.colors())
        .zip(app.palette_fields.iter())
        .map(|((label, color), field)| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_h(theme::TOUCH)
                .child(
                    div()
                        .size(px(24.))
                        .flex_none()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme::control())
                        .bg(theme::swatch(color)),
                )
                .child(div().flex_1().child(*label))
                .child(div().w(px(112.)).child(Input::new(field)))
        });
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            card()
                .flex()
                .flex_col()
                .gap_3()
                .child(section("Theme"))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_3()
                        .children(presets)
                        .child(custom_option),
                ),
        )
        .when(custom, |page| {
            page.child(
                card()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(section("Colors"))
                    .children(colors),
            )
        })
        .when(theme::SEES_DESKTOP, |page| {
            page.child(
                card()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section("Sidebar"))
                    .child(switch_row(
                        Switch::new("translucent-sidebar")
                            .checked(theme::translucent_sidebar())
                            .label("Translucent")
                            .on_click(cx.listener(|app, on: &bool, _, cx| {
                                app.use_translucent_sidebar(*on, cx)
                            })),
                    )),
            )
        })
        .into_any_element()
}

fn theme_option(
    id: impl Into<ElementId>,
    name: &'static str,
    palette: theme::Palette,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(136.))
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(theme::RADIUS_BUTTON)
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::control()
        })
        .when(selected, |s| s.bg(theme::selected()))
        .cursor_pointer()
        .hover(|s| s.border_color(theme::border_strong()))
        .child(theme_preview(palette))
        .child(div().font_weight(FontWeight::MEDIUM).child(name))
}

/// A tiny sketch of the app drawn in `palette`: sidebar, room tiles and status dots.
fn theme_preview(palette: theme::Palette) -> Div {
    let color = theme::swatch;
    let bar = |width: f32, height: f32, fill: u32| {
        div()
            .w(relative(width))
            .h(px(height))
            .rounded(px(height / 2.))
            .bg(color(fill))
    };
    let dot = |fill: u32| div().size(px(5.)).rounded_full().bg(color(fill));
    let tile = |avatar: u32| {
        div()
            .flex_1()
            .rounded(px(3.))
            .bg(color(palette.panel))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(10.)).rounded_full().bg(color(avatar)))
    };
    let screen = div()
        .flex()
        .size_full()
        .rounded(px(5.))
        .bg(color(palette.bg))
        .child(
            div()
                .w(px(36.))
                .h_full()
                .rounded_l(px(5.))
                .p(px(5.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .bg(color(palette.panel))
                .child(bar(1., 6., palette.accent))
                .child(bar(0.8, 4., palette.text))
                .child(bar(0.6, 4., palette.muted))
                .child(bar(0.7, 4., palette.muted)),
        )
        .child(
            div()
                .flex_1()
                .p(px(5.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(bar(0.4, 4., palette.text))
                        .child(
                            div()
                                .flex()
                                .gap(px(2.))
                                .child(dot(palette.good))
                                .child(dot(palette.warning))
                                .child(dot(palette.bad)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .gap(px(4.))
                        .child(tile(palette.accent))
                        .child(tile(palette.muted)),
                ),
        );
    div()
        .h(px(64.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::control())
        .child(screen)
}
