mod app;
mod hotkey;
mod theme;
mod video;
mod views;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mock = args.iter().any(|a| a == "--mock");
    let screen = args
        .iter()
        .position(|a| a == "--screen")
        .and_then(|i| args.get(i + 1))
        .cloned();
    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::install(cx);
            app::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(600.))),
                ..Default::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                window.set_window_title("throcc");
                let view = cx.new(|cx| {
                    let mut app = app::ThroccApp::new(mock, window, cx);
                    if let Some(screen) = &screen {
                        app.show_sample(screen);
                    }
                    app
                });
                cx.new(|cx| Root::new(view, window, cx))
            });
            if let Err(e) = opened {
                eprintln!("could not open a window: {e}");
                cx.quit();
            }
            cx.activate(true);
        });
}
