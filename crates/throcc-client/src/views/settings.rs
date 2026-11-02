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
use throcc_client_core::client::Command;
use throcc_client_core::media::audio::{
    DEFAULT_GATE_DBFS, NoiseSuppressionLevel, Transmit, VoiceSettings,
};

use super::common::{
    Typeset, avatar, caption, card, chip, field_error, key_cap, mono, primary, section,
    server_seed, slider_gain, slider_value, volume_slider,
};
use super::page::{field, frame, nav, picture_editor, row, save_line, segmented};
use crate::app::{PictureFor, Screen, SettingsPage, ThroccApp, typed_name};
use crate::hotkey::DEFAULT_KEY_LABEL;
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: SettingsPage,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let links = nav(
        SettingsPage::ALL.map(|p| (p, p.title())),
        page,
        Screen::Settings,
        cx,
    );
    let content = match page {
        SettingsPage::Profile => profile(app, cx),
        SettingsPage::Audio => audio(app, cx),
        SettingsPage::Appearance => appearance(app, cx),
        SettingsPage::Servers => servers(app),
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

fn set_voice(app: &mut ThroccApp, change: impl FnOnce(&mut VoiceSettings)) {
    change(&mut app.voice);
    app.command(Command::Voice(app.voice));
}

fn profile(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let seed = app.state.me.map_or(0, |me| me.0);
    let name = app.state.profile_name.clone().unwrap_or_default();
    let saved = app.state.profile_picture;
    let picture = picture_editor(app, PictureFor::Profile, &name, seed, saved, true, cx);
    let name_error = typed_name(&app.name, cx).err();
    let save = primary("save-profile", "Save")
        .disabled(app.profile_form.saving() || !app.profile_changed(cx))
        .on_click(cx.listener(|app, _, _, cx| app.save_profile(cx)));
    let save = save_line(
        app.profile_form.status.as_ref(),
        |reason| format!("Kept on this computer, but this server refused it: {reason}"),
        save,
    );
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
    pub fn new(
        voice: &VoiceSettings,
        inputs: &[String],
        outputs: &[String],
        window: &mut Window,
        cx: &mut Context<ThroccApp>,
    ) -> Self {
        let mut device_select = |names: &[String]| {
            let mut items: Devices = vec![SYSTEM_DEFAULT.into()];
            items.extend(names.iter().cloned().map(SharedString::from));
            cx.new(|cx| SelectState::new(items, Some(IndexPath::new(0)), window, cx))
        };
        let input = device_select(inputs);
        let output = device_select(outputs);
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
                  _,
                  event: &SelectEvent<Devices>,
                  cx: &mut Context<ThroccApp>| {
                let SelectEvent::Confirm(picked) = event;
                let device = picked
                    .as_ref()
                    .filter(|name| name.as_ref() != SYSTEM_DEFAULT)
                    .map(ToString::to_string);
                if is_input {
                    app.input_device = device;
                } else {
                    app.output_device = device;
                }
                app.command(Command::Devices {
                    input: app.input_device.clone(),
                    output: app.output_device.clone(),
                });
                cx.notify();
            }
        };
        let on_slide = |change: fn(&mut VoiceSettings, &SliderEvent)| {
            move |app: &mut ThroccApp, _, event: &SliderEvent, cx: &mut Context<ThroccApp>| {
                set_voice(app, |voice| change(voice, event));
                cx.notify();
            }
        };
        let subscriptions = vec![
            cx.subscribe(&input, on_device(true)),
            cx.subscribe(&output, on_device(false)),
            cx.subscribe(
                &input_volume,
                on_slide(|voice, event| voice.input_volume = slider_gain(event)),
            ),
            cx.subscribe(
                &output_volume,
                on_slide(|voice, event| voice.output_volume = slider_gain(event)),
            ),
            cx.subscribe(&cue_volume, |app, _, event: &SliderEvent, cx| {
                set_voice(app, |voice| voice.cue_volume = slider_gain(event));
                if matches!(event, SliderEvent::Release(_)) {
                    app.save_cue_volume();
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
                let meter_shown = matches!(app.voice.transmit, Transmit::VoiceActivity { .. });
                if !shown {
                    app.stop_mic_test();
                } else if meter_shown && app.client.is_some() {
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

/// The mic level as a bar, green while it is above `threshold_dbfs`, with a mark there.
fn meter(level_dbfs: Option<f32>, threshold_dbfs: f32) -> Div {
    let fraction = |dbfs: f32| (1. - dbfs / METER_FLOOR_DBFS).clamp(0., 1.);
    let level = level_dbfs.unwrap_or(f32::NEG_INFINITY);
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
                .w(relative(fraction(level)))
                .bg(if level > threshold_dbfs {
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
            mono(format!("{percent:.0}%"))
                .w(px(64.))
                .flex_none()
                .text_right(),
        )
}

fn audio(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    keep_metering(app, cx);
    let voice = app.voice;
    let vad = matches!(voice.transmit, Transmit::VoiceActivity { .. });
    let threshold = gate_dbfs(voice.transmit);
    let level = app.client.as_ref().and_then(|c| c.mic_level());
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
                            app.sync_push_to_talk(cx);
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
                                mono(format!("{threshold:.0} dB").replace('-', "−"))
                                    .w(px(64.))
                                    .flex_none()
                                    .text_right(),
                            ),
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
                        [
                            (None, "Off"),
                            (Some(NoiseSuppressionLevel::Low), "Low"),
                            (Some(NoiseSuppressionLevel::Moderate), "Moderate"),
                            (Some(NoiseSuppressionLevel::High), "High"),
                        ],
                        voice.noise_suppression,
                        cx,
                        |app, level, _| set_voice(app, |v| v.noise_suppression = level),
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
                        "Stop Test"
                    } else {
                        "Test Microphone"
                    },
                )
                .on_click(cx.listener(|app, _, _, cx| {
                    app.mic_test = !app.mic_test;
                    app.command(Command::MicTest(app.mic_test));
                    cx.notify();
                })),
            ),
        )
        .into_any_element()
}

fn servers(app: &ThroccApp) -> AnyElement {
    let current = app.connected_server().map(|s| s.address.as_str());
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .children(app.known_servers.iter().map(|(address, fingerprint)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .min_h(theme::TOUCH)
                        .child(avatar(address, server_seed(address), None, px(28.), false))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .child(mono(address.clone()))
                                .child(
                                    mono(fingerprint.short())
                                        .typeset(theme::CAPTION)
                                        .text_color(theme::text_3()),
                                ),
                        )
                        .children(
                            (current == Some(address.as_str()))
                                .then(|| chip("Connected", theme::ok_bg(), theme::ok_text())),
                        )
                })),
        )
        .into_any_element()
}

fn appearance(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let custom = app.picked_theme == theme::CUSTOM;
    let presets = theme::PRESETS
        .iter()
        .enumerate()
        .map(|(i, &(name, palette))| {
            theme_option(("preset", i), name, palette, app.picked_theme == name).on_click(
                cx.listener(move |app, _, _, cx| {
                    app.use_preset(name, palette, cx);
                    cx.notify();
                }),
            )
        });
    let custom_option = theme_option("custom", theme::CUSTOM, app.custom_colors, custom).on_click(
        cx.listener(|app, _, _, cx| {
            app.use_custom(cx);
            cx.notify();
        }),
    );
    let colors = theme::Palette::LABELS
        .iter()
        .zip(app.custom_colors.colors())
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
                .child(
                    div()
                        .w(px(112.))
                        .font_family(theme::mono_font())
                        .child(Input::new(field)),
                )
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
