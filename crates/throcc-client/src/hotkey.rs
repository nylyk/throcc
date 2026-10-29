use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

pub const DEFAULT_KEY: &str = "alt+Space";

/// A push-to-talk key that works while another app has focus. Unregistered on drop.
pub struct PushToTalkKey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

impl PushToTalkKey {
    /// Registers `spec`, such as `alt+Space`. Fails on Wayland, where only the
    /// GlobalShortcuts portal could provide one.
    pub fn register(spec: &str) -> anyhow::Result<Self> {
        let hotkey: HotKey = spec.parse()?;
        let manager = GlobalHotKeyManager::new()?;
        manager.register(hotkey)?;
        Ok(Self { manager, hotkey })
    }

    /// The key's latest state change since the last call: `Some(true)` pressed.
    pub fn poll(&self) -> Option<bool> {
        let mut latest = None;
        while let Ok(GlobalHotKeyEvent { id, state }) = GlobalHotKeyEvent::receiver().try_recv() {
            if id == self.hotkey.id() {
                latest = Some(state == HotKeyState::Pressed);
            }
        }
        latest
    }
}

impl Drop for PushToTalkKey {
    fn drop(&mut self) {
        self.manager.unregister(self.hotkey).ok();
    }
}
