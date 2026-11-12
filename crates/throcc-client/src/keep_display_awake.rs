/// Keeps the display from sleeping while the user is idle, until dropped.
pub use platform::KeepDisplayAwake;
/// Keeps the session awake while one more window showing a share is visible, where the
/// compositor needs that from each window: Wayland. Elsewhere [`KeepDisplayAwake`] does it all.
pub use platform::KeepWindowAwake;

#[cfg(not(windows))]
const REASON: &str = "Watching a screen share";

/// Where the whole session is kept awake at once, a window needs nothing of its own.
#[cfg(not(target_os = "linux"))]
pub enum NoWindowHold {}

#[cfg(not(target_os = "linux"))]
impl NoWindowHold {
    pub fn new(_: &gpui::Window) -> Option<Self> {
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    pub struct KeepDisplayAwake(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    impl KeepDisplayAwake {
        pub fn new(_: &gpui::Window, _: &gpui::App) -> Self {
            let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::IdleDisplaySleepDisabled,
                &NSString::from_str(super::REASON),
            );
            Self(activity)
        }
    }

    impl Drop for KeepDisplayAwake {
        fn drop(&mut self) {
            // SAFETY: the activity is the one `beginActivityWithOptions_reason` returned.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }

    pub use super::NoWindowHold as KeepWindowAwake;
}

#[cfg(windows)]
mod platform {
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState,
    };

    // The execution state belongs to the thread that set it, so a hold is made and dropped
    // on gpui's main thread.
    pub struct KeepDisplayAwake;

    impl KeepDisplayAwake {
        pub fn new(_: &gpui::Window, _: &gpui::App) -> Self {
            // SAFETY: plain flags, no pointers.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) };
            Self
        }
    }

    impl Drop for KeepDisplayAwake {
        fn drop(&mut self) {
            // SAFETY: plain flags, no pointers.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }

    pub use super::NoWindowHold as KeepWindowAwake;
}

#[cfg(target_os = "linux")]
mod platform {
    use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
    use gpui::AppContext;
    use tokio::sync::oneshot;

    pub use crate::idle_inhibit::IdleInhibitor as KeepWindowAwake;

    /// Asks the desktop portal, which GNOME, KDE and X11 sessions answer, and inhibits idle on
    /// the main window for Wayland compositors that follow only their own inhibitors.
    pub struct KeepDisplayAwake {
        _release: oneshot::Sender<()>,
        _window: Option<KeepWindowAwake>,
    }

    impl KeepDisplayAwake {
        pub fn new(window: &gpui::Window, cx: &gpui::App) -> Self {
            let (release, released) = oneshot::channel();
            cx.background_spawn(async move {
                if let Err(e) = inhibit_until(released).await {
                    tracing::warn!("cannot keep the display awake: {e}");
                }
            })
            .detach();
            Self {
                _release: release,
                _window: KeepWindowAwake::new(window),
            }
        }
    }

    async fn inhibit_until(released: oneshot::Receiver<()>) -> ashpd::Result<()> {
        let proxy = InhibitProxy::new().await?;
        let options = InhibitOptions::default().set_reason(super::REASON);
        let inhibition = proxy
            .inhibit(None, InhibitFlags::Idle.into(), options)
            .await?;
        released.await.ok();
        inhibition.close().await
    }
}
