use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, IntoElement, ObjectFit, ParentElement, RenderImage, Styled, StyledImage,
    Window, div, img,
};
use throcc_client_core::client::ClientApi;
use throcc_proto::ids::UserId;

use crate::theme;
use crate::views::common::caption;

/// The last picture uploaded for one share, and which frame it was.
struct Shown {
    timestamp: u32,
    image: Arc<RenderImage>,
}

/// The share pictures uploaded to one window, by sharer. The only place frames meet gpui.
#[derive(Default)]
pub struct Feeds(HashMap<UserId, Shown>);

impl Feeds {
    /// `user`'s share, as large as fits its parent with the aspect ratio kept.
    pub fn surface(
        &mut self,
        client: Option<&Arc<dyn ClientApi>>,
        user: UserId,
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
        let fresh = self
            .0
            .get(&user)
            .is_none_or(|s| s.timestamp != frame.timestamp);
        if fresh {
            let bgra = frame.to_bgra();
            if let Some(buffer) =
                image::RgbaImage::from_raw(frame.width as u32, frame.height as u32, bgra)
            {
                let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                    buffer
                )]));
                let shown = Shown {
                    timestamp: frame.timestamp,
                    image,
                };
                if let Some(previous) = self.0.insert(user, shown) {
                    cx.drop_image(previous.image, Some(window));
                }
            }
        }
        window.request_animation_frame();
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(theme::stage())
            .children(self.0.get(&user).map(|shown| {
                img(shown.image.clone())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain)
            }))
            .into_any_element()
    }
}
