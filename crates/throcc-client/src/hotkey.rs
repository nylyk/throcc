use std::sync::{Mutex, MutexGuard, Once};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

pub const DEFAULT_KEY: &str = "alt+Space";
/// [`DEFAULT_KEY`] as the keyboard labels it.
pub const DEFAULT_KEY_LABEL: &str = if cfg!(target_os = "macos") {
    "⌥ Space"
} else {
    "Alt+Space"
};

type Handler = Box<dyn Fn(GlobalHotKeyEvent) + Send>;

/// The registered key's handler. `global_hotkey` takes one handler for the whole process.
static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static ROUTE_EVENTS: Once = Once::new();

fn handler_slot() -> MutexGuard<'static, Option<Handler>> {
    HANDLER.lock().unwrap_or_else(|e| e.into_inner())
}

/// A push-to-talk key that works while another app has focus. Unregistered on drop.
pub struct PushToTalkKey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

impl PushToTalkKey {
    /// Registers `spec`, such as `alt+Space`, calling `changed` with whether it is pressed.
    /// Fails on Wayland, where only the GlobalShortcuts portal could provide one.
    pub fn register(spec: &str, changed: impl Fn(bool) + Send + 'static) -> anyhow::Result<Self> {
        let hotkey: HotKey = spec.parse()?;
        let manager = GlobalHotKeyManager::new()?;
        ROUTE_EVENTS.call_once(|| {
            GlobalHotKeyEvent::set_event_handler(Some(|event| {
                if let Some(handler) = handler_slot().as_ref() {
                    handler(event);
                }
            }));
        });
        let id = hotkey.id();
        let handler: Handler = Box::new(move |event: GlobalHotKeyEvent| {
            if event.id == id {
                changed(event.state == HotKeyState::Pressed);
            }
        });
        *handler_slot() = Some(handler);
        if let Err(e) = manager.register(hotkey) {
            handler_slot().take();
            return Err(e.into());
        }
        Ok(Self { manager, hotkey })
    }
}

impl Drop for PushToTalkKey {
    fn drop(&mut self) {
        self.manager.unregister(self.hotkey).ok();
        handler_slot().take();
    }
}
