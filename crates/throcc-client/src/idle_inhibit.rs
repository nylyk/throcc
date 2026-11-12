//! Keeps a Wayland session from going idle while a window plays a share, through the
//! compositor's idle-inhibit protocol. The compositor honours an inhibitor while its surface is
//! visible, and idle daemons such as hypridle and swayidle follow the compositor, whatever the
//! desktop portal can do: Hyprland's portal, for one, has no Inhibit backend of its own.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1;

/// Inhibits idle while `window`'s surface is visible, until dropped.
pub struct IdleInhibitor {
    inhibitor: ZwpIdleInhibitorV1,
    connection: Connection,
}

impl IdleInhibitor {
    /// `None` outside Wayland, or on a compositor without the protocol.
    pub fn new(window: &gpui::Window) -> Option<Self> {
        let display = window.display_handle().ok()?.as_raw();
        let surface = HasWindowHandle::window_handle(window).ok()?.as_raw();
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) =
            (display, surface)
        else {
            return None;
        };
        let manager = MANAGER
            .get_or_init(|| Manager::bind(display.display))
            .as_ref()?;
        // SAFETY: the pointer is gpui's live wl_surface proxy for this window, and the id
        // is used only to create the inhibitor here.
        let id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) };
        let surface = WlSurface::from_id(&manager.connection, id.ok()?).ok()?;
        let mut queue = manager.queue.lock().unwrap_or_else(|e| e.into_inner());
        // Drains the registry's events, which nothing else reads.
        queue.dispatch_pending(&mut Ignore).ok()?;
        let inhibitor = manager
            .manager
            .create_inhibitor(&surface, &queue.handle(), ());
        manager.connection.flush().ok()?;
        Some(Self {
            inhibitor,
            connection: manager.connection.clone(),
        })
    }
}

impl Drop for IdleInhibitor {
    fn drop(&mut self) {
        self.inhibitor.destroy();
        self.connection.flush().ok();
    }
}

/// The inhibit manager, bound once on gpui's display through a queue of our own.
struct Manager {
    connection: Connection,
    queue: Mutex<EventQueue<Ignore>>,
    manager: ZwpIdleInhibitManagerV1,
}

static MANAGER: OnceLock<Option<Manager>> = OnceLock::new();

impl Manager {
    fn bind(display: NonNull<c_void>) -> Option<Self> {
        // SAFETY: gpui keeps its Wayland display connected for the whole life of the app.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<Ignore>(&connection).ok()?;
        let manager = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
        Some(Self {
            connection,
            queue: Mutex::new(queue),
            manager,
        })
    }
}

/// None of the objects on our queue has events we need.
struct Ignore;

impl Dispatch<WlRegistry, GlobalListContents> for Ignore {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for Ignore {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitManagerV1,
        _: <ZwpIdleInhibitManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for Ignore {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitorV1,
        _: <ZwpIdleInhibitorV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
