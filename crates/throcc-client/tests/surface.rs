#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::sync::Arc;

use gpui::{
    Bounds, ContentMask, Corners, DevicePixels, Nv12Frame, Nv12Planes, PaintSurface,
    PlatformHeadlessRenderer, ScaledPixels, Scene, Size, SurfaceId, point, size,
};
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
        #[cfg(target_os = "linux")]
        frame,
        #[cfg(target_os = "macos")]
        source: gpui::SurfaceSource::Nv12(frame),
    }
}

fn flat(width: usize, height: usize, y: u8, u: u8, v: u8) -> VideoFrame {
    let mut frame = VideoFrame::blank(width, height);
    frame.y.fill(y);
    frame.uv.as_chunks_mut::<2>().0.fill([u, v]);
    frame
}

#[cfg(target_os = "linux")]
fn renderer() -> gpui_wgpu::WgpuHeadlessRenderer {
    gpui_wgpu::WgpuHeadlessRenderer::new()
        .expect("no Vulkan or GL adapter; install mesa's lavapipe")
}

#[cfg(target_os = "macos")]
fn renderer() -> gpui_apple::metal_renderer::MetalHeadlessRenderer {
    gpui_apple::metal_renderer::MetalHeadlessRenderer::new()
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

/// A frame whose planes are shorter than its size says, or one with no pixels at all.
#[test]
fn short_or_empty_frames_draw_nothing() {
    let mut short = flat(32, 32, 200, 128, 128);
    short.uv.truncate(10);
    for frame in [short, flat(0, 0, 200, 128, 128)] {
        let mut renderer = renderer();
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
}

#[cfg(all(target_os = "macos", feature = "ffmpeg"))]
#[test]
fn decoded_pixel_buffers_draw_uncopied() {
    use throcc_client_core::media::video::Frame;
    use throcc_client_core::media::video::decode::{Decoder, MaxSize};
    use throcc_client_core::media::video::encode::{Encoder, EncoderConfig};

    let config = EncoderConfig {
        width: 1280,
        height: 720,
        fps: 30,
        kbps: 4000,
    };
    let mut encoder = Encoder::open(config).expect("a video encoder");
    let mut decoder = Decoder::new(encoder.codec(), MaxSize::new(1280, 720)).unwrap();
    let mut last = None;
    for n in 0..10 {
        let mut input = flat(1280, 720, 200, 128, 128);
        input.timestamp = n * 3000;
        for unit in encoder.encode(&input).unwrap() {
            if let Some(frame) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                last = Some(frame);
            }
        }
    }
    let Some(Frame::PixelBuffer(buffer)) = last else {
        eprintln!("skipped: {} decoded into memory", decoder.engine());
        return;
    };
    let pointer = std::ptr::NonNull::new(buffer.as_ptr().cast()).unwrap();
    // SAFETY: `buffer` holds a live CVPixelBufferRef, and the surface takes its own reference.
    let retained = unsafe { objc2_core_foundation::CFRetained::retain(pointer) };
    let at = bounds(0.0, 0.0, 64.0, 36.0);
    let scene = scene_of([PaintSurface {
        order: 0,
        bounds: at,
        content_mask: ContentMask { bounds: at },
        corner_radii: Corners::all(ScaledPixels(0.0)),
        source: gpui::SurfaceSource::Surface(retained),
    }]);
    let image = renderer()
        .render_scene_to_image(&scene, device_size(64, 36))
        .unwrap();
    let pixel = image.get_pixel(32, 18).0;
    assert!(
        pixel[..3].iter().all(|channel| channel.abs_diff(200) <= 6),
        "the decoded picture drew as {pixel:?}"
    );
}
