use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, AsyncApp, Context, Entity, IntoElement, ParentElement, Pixels,
    Render, Styled, Task, WeakEntity, Window, div, px,
};
use throcc_client_core::client::ClientApi;
use throcc_client_core::media::video::Frame;
use throcc_proto::ids::UserId;

use gpui_component::spinner::Spinner;
use gpui_component::{Sizable, Size};

use crate::theme;
use crate::views::common::icon;

/// How often a share on screen tells the core it is still drawn; under the core's one-second
/// preview linger.
const STILL_DRAWN_EVERY: Duration = Duration::from_millis(500);
/// How often a share on screen looks for its picture before the core has one.
const PICTURE_WAIT: Duration = Duration::from_millis(100);

/// The share pictures shown in one window, by sharer. The only place frames meet gpui.
#[derive(Default)]
pub struct Feeds {
    views: HashMap<UserId, Entity<ShareView>>,
    /// The sharers drawn since the last [`Self::sweep`].
    drawn: HashSet<UserId>,
}

impl Feeds {
    /// Lets go of the shares not drawn since the last sweep, so a share that left the window
    /// holds no memory. Called at the start of each render of the window.
    pub fn sweep(&mut self) {
        let drawn = std::mem::take(&mut self.drawn);
        self.views.retain(|user, _| drawn.contains(user));
    }

    /// `user`'s share, as large as fits its parent with the aspect ratio kept, its corners
    /// rounded by `radius`.
    pub fn surface(
        &mut self,
        client: Option<&Arc<dyn ClientApi>>,
        user: UserId,
        radius: Pixels,
        cx: &mut App,
    ) -> AnyElement {
        self.drawn.insert(user);
        let view = self
            .views
            .entry(user)
            .or_insert_with(|| cx.new(|_| ShareView::new(user)));
        view.update(cx, |view, cx| view.follow(client, radius, cx));
        view.clone().into_any_element()
    }
}

/// One share on screen, redrawn on its own whenever its picture changes.
struct ShareView {
    user: UserId,
    radius: Pixels,
    client: Option<Arc<dyn ClientApi>>,
    shown: Option<Shown>,
    /// Redraws this view whenever the share's picture changes.
    watcher: Option<Task<()>>,
}

impl ShareView {
    fn new(user: UserId) -> Self {
        Self {
            user,
            radius: px(0.),
            client: None,
            shown: None,
            watcher: None,
        }
    }

    /// Draws from `client` with corners rounded by `radius`, watching its pictures.
    fn follow(
        &mut self,
        client: Option<&Arc<dyn ClientApi>>,
        radius: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.radius = radius;
        let same = match (client, &self.client) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.client = client.cloned();
        self.shown = None;
        let user = self.user;
        self.watcher = client.cloned().map(|client| {
            cx.spawn(async move |view, cx| redraw_on_new_pictures(view, client, user, cx).await)
        });
    }
}

impl Render for ShareView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let user = self.user;
        if let Some(client) = &self.client {
            client.picture_drawn(user);
        }
        let frame = self
            .client
            .as_ref()
            .and_then(|c| c.picture(user))
            .and_then(|picture| picture.borrow().clone());
        let Some(frame) = frame else {
            self.shown = None;
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
        if self
            .shown
            .as_ref()
            .is_none_or(|s| s.timestamp != frame.timestamp())
        {
            self.shown = Some(Shown::new(frame, self.shown.as_ref()));
        }
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .rounded(self.radius)
            .children(self.shown.as_ref().map(|shown| shown.element(self.radius)))
            .into_any_element()
    }
}

/// Redraws `view` whenever `user`'s picture changes, until the view is gone.
async fn redraw_on_new_pictures(
    view: WeakEntity<ShareView>,
    client: Arc<dyn ClientApi>,
    user: UserId,
    cx: &mut AsyncApp,
) {
    let redraw = |cx: &mut AsyncApp| view.update(cx, |_, cx| cx.notify()).is_ok();
    loop {
        let Some(mut picture) = client.picture(user) else {
            cx.background_executor().timer(PICTURE_WAIT).await;
            continue;
        };
        picture.mark_unchanged();
        if !redraw(cx) {
            return;
        }
        loop {
            tokio::select! {
                changed = picture.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if !redraw(cx) {
                        return;
                    }
                }
                () = cx.background_executor().timer(STILL_DRAWN_EVERY) => {
                    client.picture_drawn(user);
                }
            }
        }
        cx.background_executor().timer(PICTURE_WAIT).await;
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
