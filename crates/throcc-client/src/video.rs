use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AsyncApp, EntityId, IntoElement, ParentElement, Pixels, Styled, Task, Window,
    div,
};
use throcc_client_core::client::ClientApi;
use throcc_client_core::media::video::Frame;
use throcc_proto::ids::UserId;

use gpui_component::spinner::Spinner;
use gpui_component::{Sizable, Size};

use crate::theme;
use crate::views::common::icon;

/// How often a share on screen tells the core it is still drawn, which keeps our own
/// share's preview coming while the screen holds still. Under the core's one-second linger.
const STILL_DRAWN_EVERY: Duration = Duration::from_millis(500);
/// How often a share on screen looks for its picture before the core has one.
const PICTURE_WAIT: Duration = Duration::from_millis(100);

/// The share pictures shown in one window, by sharer. The only place frames meet gpui.
#[derive(Default)]
pub struct Feeds {
    shown: HashMap<UserId, Shown>,
    /// One task per share on screen, redrawing the window when its picture changes.
    watchers: HashMap<UserId, Task<()>>,
    /// The sharers drawn since the last [`Self::sweep`].
    drawn: HashSet<UserId>,
}

impl Feeds {
    /// Lets go of the last frames of shares not drawn since the last sweep, so a share that
    /// left the window holds no memory. Called at the start of each render of the window.
    pub fn sweep(&mut self) {
        let drawn = std::mem::take(&mut self.drawn);
        self.shown.retain(|user, _| drawn.contains(user));
        self.watchers.retain(|user, _| drawn.contains(user));
    }

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
        self.drawn.insert(user);
        if let Some(client) = client {
            client.picture_drawn(user);
            let view = window.current_view();
            self.watchers
                .entry(user)
                .or_insert_with(|| cx.spawn(redraw_on_new_pictures(client.clone(), user, view)));
        }
        let frame = client
            .and_then(|c| c.picture(user))
            .and_then(|picture| picture.borrow().clone());
        let Some(frame) = frame else {
            self.shown.remove(&user);
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Spinner::new()
                        .icon(icon("loader-circle"))
                        .with_size(Size::Large)
                        .color(theme::text_3()),
                )
                .into_any_element();
        };
        let previous = self.shown.get(&user);
        if previous.is_none_or(|s| s.timestamp != frame.timestamp()) {
            let shown = Shown::new(frame, previous);
            self.shown.insert(user, shown);
        }
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .rounded(radius)
            .children(self.shown.get(&user).map(|shown| shown.element(radius)))
            .into_any_element()
    }
}

/// Redraws `view` whenever `user`'s picture changes, until the returned future is dropped.
fn redraw_on_new_pictures(
    client: Arc<dyn ClientApi>,
    user: UserId,
    view: EntityId,
) -> impl AsyncFnOnce(&mut AsyncApp) {
    async move |cx: &mut AsyncApp| {
        loop {
            let Some(mut picture) = client.picture(user) else {
                cx.background_executor().timer(PICTURE_WAIT).await;
                continue;
            };
            picture.mark_unchanged();
            cx.update(|cx| cx.notify(view));
            loop {
                tokio::select! {
                    changed = picture.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        cx.update(|cx| cx.notify(view));
                    }
                    () = cx.background_executor().timer(STILL_DRAWN_EVERY) => {
                        client.picture_drawn(user);
                    }
                }
            }
            cx.background_executor().timer(PICTURE_WAIT).await;
        }
    }
}

/// The last picture handed to the GPU for one share: NV12 planes the renderer uploads at
/// 1.5 bytes a pixel, or on macOS a pixel buffer it draws from where it is.
struct Shown {
    timestamp: u32,
    source: gpui::SurfaceSource,
}

impl Shown {
    fn new(frame: Arc<Frame>, previous: Option<&Shown>) -> Self {
        let timestamp = frame.timestamp();
        #[cfg(target_os = "macos")]
        if let Frame::PixelBuffer(buffer) = &*frame
            && let Some(pointer) = std::ptr::NonNull::new(buffer.as_ptr().cast())
        {
            // SAFETY: `buffer` holds a live CVPixelBufferRef, and the renderer gets a
            // reference of its own.
            let retained = unsafe { objc2_core_foundation::CFRetained::retain(pointer) };
            return Self {
                timestamp,
                source: gpui::SurfaceSource::Surface(retained),
            };
        }
        let (id, generation) = match previous.map(|shown| &shown.source) {
            Some(gpui::SurfaceSource::Nv12(previous)) => (previous.id, previous.generation + 1),
            _ => (gpui::SurfaceId::new(), 0),
        };
        let (width, height) = frame.size();
        Self {
            timestamp,
            source: gpui::SurfaceSource::Nv12(gpui::Nv12Frame {
                id,
                generation,
                width: width as u32,
                height: height as u32,
                planes: Arc::new(Planes(frame)),
            }),
        }
    }

    fn element(&self, radius: Pixels) -> AnyElement {
        gpui::surface(self.source.clone())
            .absolute()
            .inset_0()
            .size_full()
            .rounded(radius)
            .into_any_element()
    }
}

struct Planes(Arc<Frame>);

impl gpui::Nv12Planes for Planes {
    fn y(&self) -> &[u8] {
        self.0.planes().map_or(&[], |frame| &frame.y)
    }

    fn uv(&self) -> &[u8] {
        self.0.planes().map_or(&[], |frame| &frame.uv)
    }
}
