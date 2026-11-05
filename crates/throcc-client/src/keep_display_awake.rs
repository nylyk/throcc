/// Keeps the display from sleeping while the user is idle, until dropped.
pub struct KeepDisplayAwake {
    _hold: platform::Hold,
}

impl KeepDisplayAwake {
    pub fn new(cx: &gpui::App) -> Self {
        Self {
            _hold: platform::Hold::new(cx),
        }
    }
}

#[cfg(not(windows))]
const REASON: &str = "Watching a screen share";

#[cfg(target_os = "macos")]
mod platform {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    pub struct Hold(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    impl Hold {
        pub fn new(_: &gpui::App) -> Self {
            let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::IdleDisplaySleepDisabled,
                &NSString::from_str(super::REASON),
            );
            Self(activity)
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            // SAFETY: the activity is the one `beginActivityWithOptions_reason` returned.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState,
    };

    // The execution state belongs to the thread that set it, so a hold is made and dropped
    // on gpui's main thread.
    pub struct Hold;

    impl Hold {
        pub fn new(_: &gpui::App) -> Self {
            // SAFETY: plain flags, no pointers.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) };
            Self
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            // SAFETY: plain flags, no pointers.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
    use gpui::AppContext;
    use tokio::sync::oneshot;

    pub struct Hold {
        _release: oneshot::Sender<()>,
    }

    impl Hold {
        pub fn new(cx: &gpui::App) -> Self {
            let (release, released) = oneshot::channel();
            cx.background_spawn(async move {
                if let Err(e) = inhibit_until(released).await {
                    tracing::warn!("cannot keep the display awake: {e}");
                }
            })
            .detach();
            Self { _release: release }
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
