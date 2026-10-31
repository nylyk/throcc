use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AnyElement, App, IntoElement, ParentElement, Pixels, Styled, Window, div};
use throcc_client_core::client::ClientApi;
use throcc_client_core::media::video::frame::VideoFrame;
use throcc_proto::ids::UserId;

use crate::views::common::caption;

/// The share pictures shown in one window, by sharer. The only place frames meet gpui.
#[derive(Default)]
pub struct Feeds(HashMap<UserId, Shown>);

impl Feeds {
    /// `user`'s share, as large as fits its parent with the aspect ratio kept, its corners
    /// rounded by `radius`.
    pub fn surface(
        &mut self,
        client: Option<&Arc<dyn ClientApi>>,
        user: UserId,
        radius: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let frame = client
            .and_then(|c| c.picture(user))
            .and_then(|picture| picture.borrow().clone());
        let Some(frame) = frame else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(caption("Waiting for the first picture…"))
                .into_any_element();
        };
        let previous = self.0.get(&user);
        if previous.is_none_or(|s| s.timestamp != frame.timestamp)
            && let Some(shown) = Shown::new(frame, previous)
            && let Some(previous) = self.0.insert(user, shown)
        {
            previous.release(window, cx);
        }
        window.request_animation_frame();
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .rounded(radius)
            .children(self.0.get(&user).map(|shown| shown.element(radius)))
            .into_any_element()
    }
}

/// The last frame handed to the GPU for one share, as an NV12 surface the renderer uploads
/// two planes of, at 1.5 bytes a pixel.
#[cfg(not(target_os = "macos"))]
struct Shown {
    timestamp: u32,
    frame: gpui::Nv12Frame,
}

#[cfg(not(target_os = "macos"))]
impl Shown {
    fn new(frame: Arc<VideoFrame>, previous: Option<&Shown>) -> Option<Self> {
        let (id, generation) = previous.map_or((gpui::SurfaceId::new(), 0), |shown| {
            (shown.frame.id, shown.frame.generation + 1)
        });
        Some(Self {
            timestamp: frame.timestamp,
            frame: gpui::Nv12Frame {
                id,
                generation,
                width: frame.width as u32,
                height: frame.height as u32,
                planes: Arc::new(Planes(frame)),
            },
        })
    }

    fn element(&self, radius: Pixels) -> AnyElement {
        gpui::surface(self.frame.clone())
            .absolute()
            .inset_0()
            .size_full()
            .rounded(radius)
            .into_any_element()
    }

    fn release(self, _window: &mut Window, _cx: &mut App) {}
}

#[cfg(not(target_os = "macos"))]
struct Planes(Arc<VideoFrame>);

#[cfg(not(target_os = "macos"))]
impl gpui::Nv12Planes for Planes {
    fn y(&self) -> &[u8] {
        &self.0.y
    }

    fn uv(&self) -> &[u8] {
        &self.0.uv
    }
}

/// The last frame handed to the GPU for one share, as a BGRA image in the sprite atlas.
#[cfg(target_os = "macos")]
struct Shown {
    timestamp: u32,
    image: Arc<gpui::RenderImage>,
}

#[cfg(target_os = "macos")]
impl Shown {
    fn new(frame: Arc<VideoFrame>, _previous: Option<&Shown>) -> Option<Self> {
        let buffer =
            image::RgbaImage::from_raw(frame.width as u32, frame.height as u32, frame.to_bgra())?;
        let image = gpui::RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]);
        Some(Self {
            timestamp: frame.timestamp,
            image: Arc::new(image),
        })
    }

    fn element(&self, radius: Pixels) -> AnyElement {
        use gpui::StyledImage;

        gpui::img(self.image.clone())
            .absolute()
            .inset_0()
            .size_full()
            .rounded(radius)
            .object_fit(gpui::ObjectFit::Contain)
            .into_any_element()
    }

    fn release(self, window: &mut Window, cx: &mut App) {
        cx.drop_image(self.image, Some(window));
    }
}
