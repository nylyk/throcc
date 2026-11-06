#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod assets;
mod avatar;
mod hotkey;
mod keep_display_awake;
mod popout;
mod theme;
mod video;
mod views;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, point, px, size};
use gpui_component::Root;
use std::sync::Arc;

use throcc_client_core::keystore;
use throcc_client_core::media::voice::AudioEngine;
use throcc_client_core::settings::Settings;
use views::common;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mock = args.iter().any(|a| a == "--mock");
    let value_of = |flag: &str| {
        let at = args.iter().position(|a| a == flag)?;
        args.get(at + 1).cloned()
    };
    let window_size = value_of("--size")
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some(size(px(w.parse().ok()?), px(h.parse().ok()?)))
        })
        .unwrap_or(size(px(1280.), px(800.)));
    let screen = value_of("--screen");
    let (mut settings, settings_problem) = if mock {
        (Settings::default(), None)
    } else {
        saved_settings()
    };
    if mock && let Some((name, _)) = value_of("--theme").and_then(|name| theme::preset(&name)) {
        settings.appearance.theme = Some(name.into());
    }
    let audio = match AudioEngine::start(settings.audio.voice) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("throcc can't start its audio: {e:#}");
            std::process::exit(1);
        }
    };
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::apply(&settings.appearance, cx);
            app::bind_keys(cx);
            let bounds = Bounds::centered(None, window_size, cx);
            let options = common::app_titlebar(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(480.))),
                    ..Default::default()
                },
                point(px(8.), (theme::HEADER_HEIGHT - px(14.)) / 2.),
                true,
            );
            let opened = cx.open_window(options, |window, cx| {
                window.set_window_title("throcc");
                let view = cx.new(|cx| {
                    app::ThroccApp::new(mock, settings, settings_problem, audio, window, cx)
                });
                if let Some(screen) = &screen {
                    view.update(cx, |app, cx| app.show_sample(screen, window, cx));
                }
                cx.new(|cx| Root::new(view, window, cx))
            });
            match opened {
                Ok(main_window) => {
                    let main_window = main_window.window_id();
                    cx.on_window_closed(move |cx, closed| {
                        if closed == main_window {
                            cx.quit();
                        }
                    })
                    .detach();
                }
                Err(e) => {
                    eprintln!("could not open a window: {e}");
                    cx.quit();
                }
            }
            cx.activate(true);
        });
}

/// This computer's settings, or the defaults with why they couldn't be read.
fn saved_settings() -> (Settings, Option<String>) {
    match keystore::config_dir().and_then(|dir| Settings::load(&dir)) {
        Ok(settings) => (settings, None),
        Err(e) => {
            tracing::warn!("no saved settings: {e:#}");
            let problem = format!("Couldn't read your settings ({e}), so changes aren't kept.");
            (Settings::default(), Some(problem))
        }
    }
}
