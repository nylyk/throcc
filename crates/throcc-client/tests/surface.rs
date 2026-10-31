#![cfg(target_os = "linux")]

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AtlasKey, Bounds, ContentMask, Corners, DevicePixels, Nv12Frame, Nv12Planes, PaintSurface,
    PlatformHeadlessRenderer, PolychromeSprite, RenderImage, RenderImageParams, ScaledPixels,
    Scene, Size, SurfaceId, point, size,
};
use gpui_wgpu::WgpuHeadlessRenderer;
use throcc_client_core::media::video::frame::VideoFrame;

struct Planes(Arc<VideoFrame>);

impl Nv12Planes for Planes {
    fn y(&self) -> &[u8] {
        &self.0.y
    }

    fn uv(&self) -> &[u8] {
        &self.0.uv
    }
}

fn nv12(id: SurfaceId, generation: u64, frame: VideoFrame) -> Nv12Frame {
    Nv12Frame {
        id,
        generation,
        width: frame.width as u32,
        height: frame.height as u32,
        planes: Arc::new(Planes(Arc::new(frame))),
    }
}

fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn device_size(width: i32, height: i32) -> Size<DevicePixels> {
    size(DevicePixels(width), DevicePixels(height))
}

fn scene_of(surfaces: impl IntoIterator<Item = PaintSurface>) -> Scene {
    let mut scene = Scene::default();
    for surface in surfaces {
        scene.insert_primitive(surface);
    }
    scene.finish();
    scene
}

fn surface(frame: Nv12Frame, at: Bounds<ScaledPixels>, radius: f32) -> PaintSurface {
    PaintSurface {
        order: 0,
        bounds: at,
        content_mask: ContentMask { bounds: at },
        corner_radii: Corners::all(ScaledPixels(radius)),
        frame,
    }
}

fn flat(width: usize, height: usize, y: u8, u: u8, v: u8) -> VideoFrame {
    let mut frame = VideoFrame::blank(width, height);
    frame.y.fill(y);
    frame.uv.as_chunks_mut::<2>().0.fill([u, v]);
    frame
}

fn renderer() -> WgpuHeadlessRenderer {
    WgpuHeadlessRenderer::new().expect("no Vulkan or GL adapter; install mesa's lavapipe")
}

fn assert_rgb(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 3]) {
    let actual = image.get_pixel(x, y).0;
    assert!(
        actual[..3]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 2),
        "pixel ({x}, {y}) was {actual:?}, expected {expected:?}"
    );
}

#[test]
fn colours_match_the_cpu_conversion() {
    let mut frame = VideoFrame::blank(64, 64);
    let quadrants = [
        (81, 90, 240),
        (145, 54, 34),
        (41, 240, 110),
        (128, 128, 128),
    ];
    for row in 0..64 {
        for col in 0..64 {
            let (y, u, v) = quadrants[(row / 32) * 2 + col / 32];
            frame.y[row * 64 + col] = y;
            let uv = (row / 2) * 64 + (col / 2) * 2;
            frame.uv[uv] = u;
            frame.uv[uv + 1] = v;
        }
    }
    let bgra = frame.to_bgra();
    let mut renderer = renderer();
    let scene = scene_of([surface(
        nv12(SurfaceId::new(), 0, frame),
        bounds(0.0, 0.0, 64.0, 64.0),
        0.0,
    )]);
    let image = renderer
        .render_scene_to_image(&scene, device_size(64, 64))
        .unwrap();
    for (x, y) in [(16, 16), (48, 16), (16, 48), (48, 48)] {
        let px = (y * 64 + x) * 4;
        let expected = [bgra[px + 2], bgra[px + 1], bgra[px]];
        assert_rgb(&image, x as u32, y as u32, expected);
    }
}

#[test]
fn corners_are_rounded_and_the_content_mask_clips() {
    let mut renderer = renderer();
    let at = bounds(0.0, 0.0, 64.0, 64.0);
    let mut masked = surface(
        nv12(SurfaceId::new(), 0, flat(64, 64, 200, 128, 128)),
        at,
        16.0,
    );
    masked.content_mask.bounds = bounds(0.0, 0.0, 48.0, 64.0);
    let image = renderer
        .render_scene_to_image(&scene_of([masked]), device_size(64, 64))
        .unwrap();
    assert_rgb(&image, 0, 0, [0, 0, 0]);
    assert_rgb(&image, 3, 3, [0, 0, 0]);
    assert_rgb(&image, 0, 63, [0, 0, 0]);
    assert_rgb(&image, 8, 8, [200, 200, 200]);
    assert_rgb(&image, 32, 1, [200, 200, 200]);
    assert_rgb(&image, 1, 32, [200, 200, 200]);
    assert_rgb(&image, 32, 32, [200, 200, 200]);
    assert_rgb(&image, 56, 32, [0, 0, 0]);
}

#[test]
fn uploads_only_when_the_generation_changes() {
    let mut renderer = renderer();
    let id = SurfaceId::new();
    let at = bounds(0.0, 0.0, 32.0, 32.0);
    let mut draw = |generation, frame| {
        let scene = scene_of([surface(nv12(id, generation, frame), at, 0.0)]);
        let image = renderer
            .render_scene_to_image(&scene, device_size(32, 32))
            .unwrap();
        image.get_pixel(16, 16).0[0]
    };
    assert_eq!(draw(1, flat(32, 32, 50, 128, 128)), 50);
    assert_eq!(draw(1, flat(32, 32, 220, 128, 128)), 50);
    assert_eq!(draw(2, flat(32, 32, 220, 128, 128)), 220);
    assert_eq!(draw(3, flat(16, 18, 100, 128, 128)), 100);
}

#[test]
fn short_planes_draw_nothing() {
    let mut renderer = renderer();
    let mut frame = flat(32, 32, 200, 128, 128);
    frame.uv.truncate(10);
    let scene = scene_of([surface(
        nv12(SurfaceId::new(), 0, frame),
        bounds(0.0, 0.0, 32.0, 32.0),
        0.0,
    )]);
    let image = renderer
        .render_scene_to_image(&scene, device_size(32, 32))
        .unwrap();
    assert_rgb(&image, 16, 16, [0, 0, 0]);
}

fn cpu_time() -> (Duration, Duration) {
    let read = |clock| {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid timespec for clock_gettime to write.
        unsafe { libc::clock_gettime(clock, &mut now) };
        Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
    };
    (
        read(libc::CLOCK_THREAD_CPUTIME_ID),
        read(libc::CLOCK_PROCESS_CPUTIME_ID),
    )
}

/// CPU time per frame for a moving 2560×1440 share, through the old BGRA image path and
/// through the NV12 surface. `thread` is the calling (UI) thread; `process` adds every other
/// thread, which under lavapipe includes the software rasteriser. Run with
/// `cargo test --release -p throcc-client --test surface -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_bgra_image_against_nv12_surface() {
    const FRAMES: u32 = 60;
    let (width, height) = (2560, 1440);
    let frames: Vec<_> = (0..4)
        .map(|n| Arc::new(VideoFrame::test_pattern(width, height, n, 0)))
        .collect();
    let target = device_size(width as i32, height as i32);
    let at = bounds(0.0, 0.0, width as f32, height as f32);
    let mut renderer = renderer();

    let mut measure = |name: &str, show: &mut dyn FnMut(&mut WgpuHeadlessRenderer, u32)| {
        renderer
            .render_scene_to_image(&Scene::default(), target)
            .unwrap();
        let (thread, process) = cpu_time();
        for n in 0..FRAMES {
            show(&mut renderer, n);
        }
        renderer
            .render_scene_to_image(&Scene::default(), target)
            .unwrap();
        let (thread_end, process_end) = cpu_time();
        println!(
            "{name}: {:.1} ms on the calling thread, {:.1} ms in the process, per frame",
            ((thread_end - thread) / FRAMES).as_secs_f64() * 1e3,
            ((process_end - process) / FRAMES).as_secs_f64() * 1e3,
        );
    };

    let mut previous: Option<AtlasKey> = None;
    measure("bgra image", &mut |renderer, n| {
        let frame = &frames[n as usize % frames.len()];
        let buffer =
            image::RgbaImage::from_raw(width as u32, height as u32, frame.to_bgra()).unwrap();
        let image = RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]);
        let key = AtlasKey::Image(RenderImageParams {
            image_id: image.id,
            frame_index: 0,
        });
        let atlas = renderer.sprite_atlas();
        let tile = atlas
            .get_or_insert_with(key.clone(), &mut || {
                Ok(Some((
                    image.size(0),
                    Cow::Borrowed(image.as_bytes(0).unwrap()),
                )))
            })
            .unwrap()
            .unwrap();
        let mut scene = Scene::default();
        scene.insert_primitive(PolychromeSprite {
            order: 0,
            pad: 0,
            tile,
            grayscale: false.into(),
            opacity: 1.0,
            bounds: at,
            content_mask: ContentMask { bounds: at },
            corner_radii: Corners::default(),
        });
        scene.finish();
        renderer.render_scene(&scene, target).unwrap();
        if let Some(previous) = previous.replace(key) {
            atlas.remove(&previous);
        }
    });

    let id = SurfaceId::new();
    measure("nv12 surface", &mut |renderer, n| {
        let frame = Nv12Frame {
            id,
            generation: u64::from(n),
            width: width as u32,
            height: height as u32,
            planes: Arc::new(Planes(frames[n as usize % frames.len()].clone())),
        };
        let scene = scene_of([surface(frame, at, 0.0)]);
        renderer.render_scene(&scene, target).unwrap();
    });
}
