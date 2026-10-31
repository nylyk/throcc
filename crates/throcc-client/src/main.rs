mod app;
mod avatar;
mod hotkey;
mod popout;
mod theme;
mod video;
mod views;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
use views::common;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mock = args.iter().any(|a| a == "--mock");
    let theme = args
        .iter()
        .position(|a| a == "--theme")
        .and_then(|i| args.get(i + 1))
        .and_then(|name| {
            theme::PRESETS
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
        })
        .map(|(_, palette)| *palette);
    let window_size = args
        .iter()
        .position(|a| a == "--size")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some(size(px(w.parse().ok()?), px(h.parse().ok()?))))
        .unwrap_or(size(px(1280.), px(800.)));
    let screen = args
        .iter()
        .position(|a| a == "--screen")
        .and_then(|i| args.get(i + 1))
        .cloned();
    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let saved = if mock { None } else { theme::saved() };
            theme::set(theme.or(saved).unwrap_or(theme::PRESETS[0].1), cx);
            app::bind_keys(cx);
            let bounds = Bounds::centered(None, window_size, cx);
            let options = common::app_titlebar(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(900.), px(600.))),
                    ..Default::default()
                },
                (theme::HEADER_HEIGHT - px(14.)) / 2.,
                true,
            );
            let opened = cx.open_window(options, |window, cx| {
                window.set_window_title("throcc");
                let view = cx.new(|cx| app::ThroccApp::new(mock, window, cx));
                if let Some(screen) = &screen {
                    view.update(cx, |app, cx| app.show_sample(screen, window, cx));
                }
                cx.new(|cx| Root::new(view, window, cx))
            });
            if let Err(e) = opened {
                eprintln!("could not open a window: {e}");
                cx.quit();
            }
            cx.activate(true);
        });
}
