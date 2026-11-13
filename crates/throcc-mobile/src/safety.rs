//! A safety-number comparison as a profile on a phone shows it, over the core's
//! [`SafetyCheck`](core::SafetyCheck), so every client keeps the number on the key being
//! compared the same way.

use std::sync::{Arc, Mutex};

use throcc_client_core::safety as core;

use crate::{Core, lock};

/// One person's profile as open on screen. The app calls `follow` on every state.
#[derive(uniffi::Object)]
pub struct SafetyCheck {
    pub(crate) core: Arc<Core>,
    pub(crate) check: Mutex<core::SafetyCheck>,
}

#[uniffi::export]
impl SafetyCheck {
    /// Keeps the number on the key the person presents now. True if what shows changed.
    pub fn follow(&self) -> bool {
        let connected = lock(&self.core.connected);
        let Some(client) = connected.as_ref().map(|c| c.client.clone()) else {
            return false;
        };
        drop(connected);
        let state = client.state();
        lock(&self.check).follow(&state, |key| client.safety_number(key))
    }

    /// Lets the user act on the number that replaced the one they were comparing.
    pub fn compare_again(&self) {
        lock(&self.check).compare_again();
    }

    /// Whether the key changed while the profile was open, so the number on screen is a new
    /// one the user hasn't chosen to compare yet.
    pub fn key_changed_while_open(&self) -> bool {
        lock(&self.check).key_changed_while_open()
    }

    /// The number on screen in twelve groups of five digits, or none until the person
    /// presents a key in a room with us.
    pub fn groups(&self) -> Vec<String> {
        lock(&self.check).groups()
    }

    /// Whether Mark as Verified, or Not Verified, may be used now.
    pub fn can_mark(&self) -> bool {
        self.with_state(|check, state| check.mark(state, true))
            .is_some()
    }

    /// Marks the key whose number is on screen verified, or not verified for `verify` false,
    /// as the button the user tapped said.
    pub fn mark(&self, verify: bool) {
        if let Some(command) = self.with_state(|check, state| check.mark(state, verify)) {
            self.core.tell_client(command);
        }
    }

    /// Whether Dismiss Warning may be used now.
    pub fn can_dismiss_warning(&self) -> bool {
        self.with_state(|check, state| check.dismiss_warning(state))
            .is_some()
    }

    /// Dismisses the warning that the person's verified key changed, for the key on screen.
    pub fn dismiss_warning(&self) {
        if let Some(command) = self.with_state(|check, state| check.dismiss_warning(state)) {
            self.core.tell_client(command);
        }
    }

    /// What to do with the number on screen, for someone called `name`, once one shows.
    pub fn explanation(&self, name: String) -> Option<String> {
        self.with_state(|check, state| check.explanation(state, &name))
    }
}

impl SafetyCheck {
    fn with_state<T>(
        &self,
        f: impl FnOnce(&core::SafetyCheck, &throcc_client_core::state::State) -> Option<T>,
    ) -> Option<T> {
        let state = self.core.client_state()?;
        f(&lock(&self.check), &state)
    }
}
