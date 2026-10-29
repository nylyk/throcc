use std::sync::Arc;

use gpui::{
    AnyElement, Context, IntoElement, ObjectFit, ParentElement, RenderImage, Styled, StyledImage,
    Window, div, img,
};
use throcc_proto::ids::UserId;

use crate::app::ThroccApp;
use crate::theme;
use crate::views::common::caption;

/// The last picture uploaded for one share, and which frame it was.
pub struct Shown {
    timestamp: u32,
    image: Arc<RenderImage>,
}

/// The watched share's picture. The only place frames meet gpui.
pub fn surface(
    app: &mut ThroccApp,
    user: UserId,
    window: &mut Window,
    cx: &mut Context<ThroccApp>,
) -> AnyElement {
    let frame = app
        .client
        .as_ref()
        .and_then(|c| c.picture(user))
        .and_then(|picture| picture.borrow().clone());
    let Some(frame) = frame else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text_3())
            .child(caption("Waiting for the first picture…"))
            .into_any_element();
    };
    let fresh = app
        .shown
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
            let previous = app.shown.insert(
                user,
                Shown {
                    timestamp: frame.timestamp,
                    image,
                },
            );
            if let Some(previous) = previous {
                cx.drop_image(previous.image, Some(window));
            }
        }
    }
    window.request_animation_frame();
    match app.shown.get(&user) {
        Some(shown) => img(shown.image.clone())
            .size_full()
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => div().into_any_element(),
    }
}
