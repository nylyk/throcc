//! Draws an NV12 frame through gpui's D3D11 renderer in a real window and reads it back. On a
//! machine without a GPU, DXGI hands out the WARP software adapter. Runs on its own main
//! thread, so it has no test harness.

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    windows_test::run();
}

#[cfg(target_os = "windows")]
mod windows_test {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::{
        App, AppContext, Bounds, Context, IntoElement, Nv12Frame, Nv12Planes, ParentElement,
        Render, Styled, SurfaceId, Window, WindowBounds, WindowOptions, div, point, px, size,
        surface,
    };
    use throcc_client_core::media::video::frame::VideoFrame;

    struct Planes(VideoFrame);

    impl Nv12Planes for Planes {
        fn y(&self) -> &[u8] {
            &self.0.y
        }

        fn uv(&self) -> &[u8] {
            &self.0.uv
        }
    }

    struct Picture(Nv12Frame);

    impl Render for Picture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(surface(self.0.clone()).size_full().rounded(px(16.)))
        }
    }

    const SIDE: usize = 128;
    const QUADRANTS: [(u8, u8, u8); 4] = [
        (81, 90, 240),
        (145, 54, 34),
        (41, 240, 110),
        (128, 128, 128),
    ];

    fn quadrant_frame() -> VideoFrame {
        let mut frame = VideoFrame::blank(SIDE, SIDE);
        for row in 0..SIDE {
            for col in 0..SIDE {
                let (y, u, v) = QUADRANTS[(row / (SIDE / 2)) * 2 + col / (SIDE / 2)];
                frame.y[row * SIDE + col] = y;
                let uv = (row / 2) * SIDE + (col / 2) * 2;
                frame.uv[uv] = u;
                frame.uv[uv + 1] = v;
            }
        }
        frame
    }

    /// Checks the picture is letterboxed into the window's middle with each quadrant's colour
    /// and a rounded top-left corner.
    fn check(image: &image::RgbaImage, expected: &[[u8; 3]; 4]) -> Result<(), String> {
        let (width, height) = image.dimensions();
        let side = width.min(height);
        let (left, top) = ((width - side) / 2, (height - side) / 2);
        let quarter = side / 4;
        let centres = [
            (left + quarter, top + quarter),
            (left + 3 * quarter, top + quarter),
            (left + quarter, top + 3 * quarter),
            (left + 3 * quarter, top + 3 * quarter),
        ];
        for ((x, y), expected) in centres.into_iter().zip(expected) {
            let actual = image.get_pixel(x, y).0;
            if actual[..3]
                .iter()
                .zip(expected)
                .any(|(a, e)| a.abs_diff(*e) > 2)
            {
                return Err(format!(
                    "{width}x{height}: pixel ({x}, {y}) is {actual:?}, expected {expected:?}"
                ));
            }
        }
        let corner = image.get_pixel(left + 2, top + 2).0;
        if corner[..3]
            .iter()
            .zip(expected[0])
            .all(|(a, e)| a.abs_diff(e) <= 20)
        {
            return Err(format!("the top-left corner {corner:?} is not rounded off"));
        }
        Ok(())
    }

    pub fn run() {
        let frame = quadrant_frame();
        let bgra = frame.to_bgra();
        let expected = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(qx, qy): (usize, usize)| {
            let px = ((qy * SIDE / 2 + SIDE / 4) * SIDE + qx * SIDE / 2 + SIDE / 4) * 4;
            [bgra[px + 2], bgra[px + 1], bgra[px]]
        });
        let nv12 = Nv12Frame {
            id: SurfaceId::new(),
            generation: 0,
            width: SIDE as u32,
            height: SIDE as u32,
            planes: Arc::new(Planes(frame)),
        };
        let outcome: Rc<RefCell<Result<(), String>>> = Rc::new(RefCell::new(Err(
            "the app quit before a frame was read".into(),
        )));
        let result = outcome.clone();
        gpui_platform::application().run(move |cx: &mut App| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(100.), px(100.)),
                    size: size(px(400.), px(300.)),
                })),
                ..Default::default()
            };
            let window = match cx.open_window(options, |_, cx| cx.new(|_| Picture(nv12))) {
                Ok(window) => window,
                Err(error) => {
                    *result.borrow_mut() = Err(format!("could not open a window: {error:#}"));
                    cx.quit();
                    return;
                }
            };
            cx.spawn(async move |cx| {
                let mut last = String::from("no frame was drawn");
                for _ in 0..50 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let image = window.update(cx, |_, window, _| {
                        window.refresh();
                        window.render_to_image()
                    });
                    match image {
                        Ok(Ok(image)) => match check(&image, &expected) {
                            Ok(()) => {
                                last.clear();
                                break;
                            }
                            Err(error) => last = error,
                        },
                        Ok(Err(error)) => last = format!("render_to_image failed: {error:#}"),
                        Err(error) => last = format!("the window went away: {error:#}"),
                    }
                }
                *result.borrow_mut() = if last.is_empty() { Ok(()) } else { Err(last) };
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
        match outcome.replace(Ok(())) {
            Ok(()) => println!("ok: the D3D11 renderer drew the NV12 frame"),
            Err(error) => {
                eprintln!("FAILED: {error}");
                std::process::exit(1);
            }
        }
    }
}
