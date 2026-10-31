use std::time::Duration;

use gpui::{
    AnyElement, AppContext, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription,
    Window, div, px, relative,
};
use gpui_component::IndexPath;
use gpui_component::input::Input;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use throcc_client_core::client::Command;
use throcc_client_core::media::audio::{
    DEFAULT_GATE_DBFS, NoiseSuppressionLevel, Transmit, VoiceSettings,
};

use super::common::{avatar, caption, card, chip, ghost_icon, mono, primary, secondary, section};
use crate::app::{ProfileStatus, Screen, SettingsPage, ThroccApp};
use crate::theme;

pub fn render(
    app: &mut ThroccApp,
    page: SettingsPage,
    _window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> impl IntoElement {
    let item = |id: &'static str,
                label: &'static str,
                target: SettingsPage,
                cx: &mut Context<ThroccApp>| {
        nav_item(id, label, page == target).on_click(cx.listener(move |app, _, window, cx| {
            if target == SettingsPage::Profile {
                app.fill_profile(window, cx);
            }
            app.screen = Screen::Settings(target);
            cx.notify();
        }))
    };
    let links = nav_list()
        .child(item("nav-profile", "Profile", SettingsPage::Profile, cx))
        .child(item("nav-audio", "Audio", SettingsPage::Audio, cx))
        .child(item(
            "nav-appearance",
            "Appearance",
            SettingsPage::Appearance,
            cx,
        ))
        .child(item("nav-servers", "Servers", SettingsPage::Servers, cx));
    let content = match page {
        SettingsPage::Profile => profile(app, cx),
        SettingsPage::Audio => audio(app, cx),
        SettingsPage::Appearance => appearance(app, cx),
        SettingsPage::Servers => servers(app),
    };
    let page_title = match page {
        SettingsPage::Profile => "Profile",
        SettingsPage::Audio => "Audio",
        SettingsPage::Appearance => "Appearance",
        SettingsPage::Servers => "Servers",
    };
    frame("Settings", links, page_title, content, cx)
}

/// The column of page links in the settings sidebar.
pub(super) fn nav_list() -> Div {
    div().flex_1().p_3().flex().flex_col().gap_1()
}

pub(super) fn nav_item(id: &'static str, label: &'static str, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .min_h(px(36.))
        .flex()
        .items_center()
        .rounded(theme::RADIUS_ROW)
        .cursor_pointer()
        .when_page(selected)
        .hover(|s| s.bg(theme::raised()))
        .child(label)
}

/// A settings screen laid out like the main one: the sidebar under a bar with a back
/// arrow and `heading`, the page under a bar with its `page_title`.
pub(super) fn frame(
    heading: &'static str,
    links: Div,
    page_title: &'static str,
    content: AnyElement,
    cx: &mut Context<ThroccApp>,
) -> Div {
    let top_bar = || {
        div()
            .flex()
            .items_center()
            .h(theme::HEADER_HEIGHT)
            .flex_none()
            .border_b_1()
            .border_color(theme::control())
    };
    let sidebar = div()
        .w(theme::SIDEBAR_WIDTH)
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(theme::sidebar())
        .border_r_1()
        .border_color(theme::control())
        .child(
            top_bar()
                .gap_2()
                .px_3()
                .child(
                    ghost_icon("settings-back", "arrow-left", "Back").on_click(cx.listener(
                        |app, _, _, cx| {
                            app.screen = Screen::Main;
                            cx.notify();
                        },
                    )),
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child(heading)),
        )
        .child(links);
    let page = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            top_bar().px_6().child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(16.))
                    .child(page_title),
            ),
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

pub(super) trait PageHighlight {
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

pub(super) fn row(label: &str, control: impl IntoElement) -> Div {
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
pub(super) fn segmented<T: Copy + PartialEq + 'static>(
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

fn profile(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let seed = app.state.me.map_or(0, |me| me.0);
    let name = app.state.profile_name.clone().unwrap_or_default();
    let picture = match app.profile_preview.clone() {
        Some(preview) => preview,
        None => app.picture(app.state.profile_picture),
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
                .children(app.profile_status.clone().map(|status| {
                    match status {
                        ProfileStatus::Saving => caption("Saving…"),
                        ProfileStatus::Saved => caption("Saved.").text_color(theme::ok_text()),
                        ProfileStatus::UntilJoined => {
                            caption("Saved here. Others see your picture once you join a room.")
                        }
                        ProfileStatus::Failed(reason) => caption(format!(
                            "Kept on this computer, but this server refused it: {reason}"
                        ))
                        .text_color(theme::danger_text()),
                    }
                })),
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
        let mut slider = |min: f32, max: f32, value: f32| {
            cx.new(|_| {
                SliderState::new()
                    .min(min)
                    .max(max)
                    .step(1.)
                    .default_value(value)
            })
        };
        let input_volume = slider(0., 200., voice.input_volume * 100.);
        let output_volume = slider(0., 200., voice.output_volume * 100.);
        let threshold = slider(
            METER_FLOOR_DBFS,
            0.,
            match voice.transmit {
                Transmit::VoiceActivity { threshold_dbfs } => threshold_dbfs,
                Transmit::PushToTalk => DEFAULT_GATE_DBFS,
            },
        );
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
        let on_slide = |change: fn(&mut VoiceSettings, f32)| {
            move |app: &mut ThroccApp, _, event: &SliderEvent, cx: &mut Context<ThroccApp>| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                set_voice(app, |voice| change(voice, value.start()));
                cx.notify();
            }
        };
        let subscriptions = vec![
            cx.subscribe(&input, on_device(true)),
            cx.subscribe(&output, on_device(false)),
            cx.subscribe(
                &input_volume,
                on_slide(|voice, percent| voice.input_volume = percent / 100.),
            ),
            cx.subscribe(
                &output_volume,
                on_slide(|voice, percent| voice.output_volume = percent / 100.),
            ),
            cx.subscribe(
                &threshold,
                on_slide(|voice, dbfs| {
                    if let Transmit::VoiceActivity { threshold_dbfs } = &mut voice.transmit {
                        *threshold_dbfs = dbfs;
                    }
                }),
            ),
        ];
        Self {
            input,
            output,
            input_volume,
            output_volume,
            threshold,
            metering: false,
            _subscriptions: subscriptions,
        }
    }
}

/// Redraws the page every 100 ms while it shows the audio settings, for the level meter.
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
                if shown {
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
    let threshold = match voice.transmit {
        Transmit::VoiceActivity { threshold_dbfs } => threshold_dbfs,
        Transmit::PushToTalk => DEFAULT_GATE_DBFS,
    };
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
                                    Transmit::VoiceActivity {
                                        threshold_dbfs: threshold,
                                    }
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
                                mono(format!("{threshold:.0} dB"))
                                    .w(px(64.))
                                    .flex_none()
                                    .text_right(),
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
        .into_any_element()
}

fn servers(app: &mut ThroccApp) -> AnyElement {
    let current = app.state.server.as_ref().map(|s| s.address.clone());
    div()
        .flex()
        .flex_col()
        .gap_6()
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
        .into_any_element()
}

fn appearance(app: &mut ThroccApp, cx: &mut Context<ThroccApp>) -> AnyElement {
    let current = theme::palette();
    let presets =
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(
                theme::PRESETS
                    .iter()
                    .enumerate()
                    .map(|(i, (name, palette))| {
                        let palette = *palette;
                        let selected = palette == current;
                        div()
                            .id(("preset", i))
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
                            .when_some_bg(selected.then(theme::selected))
                            .cursor_pointer()
                            .hover(|s| s.border_color(theme::border_strong()))
                            .child(
                                div()
                                    .flex()
                                    .h(px(24.))
                                    .rounded(px(6.))
                                    .overflow_hidden()
                                    .children(palette.colors().map(|color| {
                                        div().flex_1().h_full().bg(theme::swatch(color))
                                    })),
                            )
                            .child(div().font_weight(FontWeight::MEDIUM).child(*name))
                            .on_click(cx.listener(move |app, _, window, cx| {
                                app.use_palette(palette, window, cx);
                                cx.notify();
                            }))
                    }),
            );
    let colors = theme::Palette::LABELS
        .iter()
        .zip(current.colors())
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
                .child(presets),
        )
        .child(
            card()
                .flex()
                .flex_col()
                .gap_1()
                .child(section("Colors"))
                .children(colors),
        )
        .into_any_element()
}

trait OptionalBg {
    fn when_some_bg(self, bg: Option<gpui::Hsla>) -> Self;
}

impl<T: Styled> OptionalBg for T {
    fn when_some_bg(self, bg: Option<gpui::Hsla>) -> Self {
        match bg {
            Some(bg) => self.bg(bg),
            None => self,
        }
    }
}
