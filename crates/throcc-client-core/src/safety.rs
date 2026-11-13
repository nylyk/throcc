//! Comparing a safety number with someone, as their profile shows it. The number stays on
//! the key the user is comparing: when the server presents another key while the profile is
//! open, the new number shows with a warning, and nothing is marked or dismissed until the
//! user chooses to compare again. Only the key whose number was on screen is ever sent, and
//! the core refuses it again if that person presents another by then.

use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::messages::PublicKey;

use crate::client::UiCommand;
use crate::state::State;

/// One person's profile as open on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyCheck {
    user: UserId,
    /// The key whose number is on screen, with that number.
    shown: Option<(Fingerprint, String)>,
    /// Whether the key changed while the profile was open, until the user compares again.
    key_changed: bool,
}

impl SafetyCheck {
    pub fn new(user: UserId) -> Self {
        Self {
            user,
            shown: None,
            key_changed: false,
        }
    }

    pub fn user(&self) -> UserId {
        self.user
    }

    /// Keeps the number on the key the person presents in `state`, with `number` giving
    /// the safety number of a key. Their key arrives with a room state and the server may
    /// present another at any time, so the app calls this on every state. True if what
    /// shows changed.
    pub fn follow(&mut self, state: &State, number: impl FnOnce(&PublicKey) -> String) -> bool {
        let Some(key) = state.person(self.user).and_then(|p| p.identity_key) else {
            return false;
        };
        if self.shown_key() == Some(Fingerprint(key)) {
            return false;
        }
        self.key_changed |= self.shown.is_some();
        self.shown = Some((Fingerprint(key), number(&key)));
        true
    }

    /// Shows the check as if the key had changed while open, for sample screens.
    pub fn mark_changed_for_sample(&mut self) {
        self.key_changed = true;
    }

    /// Lets the user act on the number that replaced the one they were comparing.
    pub fn compare_again(&mut self) {
        self.key_changed = false;
    }

    /// Whether the key changed while the profile was open, so the number on screen is a new
    /// one the user hasn't chosen to compare yet.
    pub fn key_changed_while_open(&self) -> bool {
        self.key_changed
    }

    /// The key whose number is on screen.
    pub fn shown_key(&self) -> Option<Fingerprint> {
        self.shown.as_ref().map(|(key, _)| *key)
    }

    /// The 60-digit number on screen, until the person presents a key in a room with us.
    pub fn number(&self) -> Option<&str> {
        self.shown.as_ref().map(|(_, number)| number.as_str())
    }

    /// The number in twelve groups of five digits, as it is read out.
    pub fn groups(&self) -> Vec<String> {
        let number = self.number().unwrap_or_default();
        let groups = number.as_bytes().chunks(5);
        groups
            .filter_map(|group| std::str::from_utf8(group).ok())
            .map(str::to_owned)
            .collect()
    }

    /// Whether the number on screen is the one of the key the person presents in `state`,
    /// and the user chose to compare it.
    fn current(&self, state: &State) -> bool {
        let presented = state.person(self.user).and_then(|p| p.identity_key);
        !self.key_changed && self.shown_key() == presented.map(Fingerprint)
    }

    /// Marks the key on screen verified, or not verified for `verify` false, if the user may
    /// now. The caller says which, as its button showed it, so a second tap before the screen
    /// catches up repeats the choice rather than undoing it.
    pub fn mark(&self, state: &State, verify: bool) -> Option<UiCommand> {
        let key = self.shown_key().filter(|_| self.current(state))?;
        Some(UiCommand::SetVerified {
            user: self.user,
            key,
            verified: verify,
        })
    }

    /// Dismisses the warning that the person's verified key changed, for the key on screen,
    /// if there is one and the user may now. Dismissing unties the verified key from this
    /// person, so it is never sent without a warning standing.
    pub fn dismiss_warning(&self, state: &State) -> Option<UiCommand> {
        (state.key_changed(self.user) && self.current(state)).then(|| UiCommand::DismissKeyChange {
            user: self.user,
            shown: self.shown_key(),
        })
    }

    /// Whether the number on screen is that of a key that replaced one the user verified.
    pub fn new_key_shown(&self, state: &State) -> bool {
        let warning = state.key_warnings.iter().find(|w| w.user == self.user);
        warning.is_some_and(|w| Some(w.now) == self.shown_key())
    }

    /// What to do with the number on screen, for someone called `name`, once one shows.
    pub fn explanation(&self, state: &State, name: &str) -> Option<String> {
        self.shown.as_ref()?;
        Some(if self.new_key_shown(state) {
            format!(
                "This number belongs to {name}’s new key. Compare it with theirs in person or through another app before you trust it."
            )
        } else {
            format!(
                "To check that your calls with {name} are end-to-end encrypted, compare these numbers with theirs in person or through another app."
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{KeyWarning, Person, Playback};
    use throcc_proto::ids::Role;

    const MARA: UserId = UserId(7);

    fn state_with(key: Option<PublicKey>, verified: bool) -> State {
        let mut state = State::default();
        state.people.insert(
            MARA,
            Person {
                name: "mara".into(),
                avatar: None,
                room: None,
                mic: false,
                deafened: false,
                sharing: None,
                audience: 0,
                speaking: false,
                playback: Playback::default(),
                verified,
                verified_name: None,
                identity_key: key,
                role: Role::User,
                blocked: false,
            },
        );
        state
    }

    fn number(key: &PublicKey) -> String {
        format!("{:0>60}", key[0])
    }

    #[test]
    fn nothing_can_be_marked_before_a_key_is_presented() {
        let mut check = SafetyCheck::new(MARA);
        let state = state_with(None, false);
        assert!(!check.follow(&state, number));
        assert_eq!(check.number(), None);
        assert!(check.groups().is_empty());
        assert_eq!(check.mark(&state, true), None);
        assert_eq!(check.dismiss_warning(&state), None);
        assert_eq!(check.explanation(&state, "mara"), None);
    }

    #[test]
    fn the_key_on_screen_is_the_one_marked() {
        let mut check = SafetyCheck::new(MARA);
        let state = state_with(Some([1; 32]), false);
        assert!(check.follow(&state, number));
        assert_eq!(check.groups().len(), 12);
        assert_eq!(check.dismiss_warning(&state), None, "no warning stands");
        assert_eq!(
            check.mark(&state, true),
            Some(UiCommand::SetVerified {
                user: MARA,
                key: Fingerprint([1; 32]),
                verified: true,
            })
        );
    }

    #[test]
    fn a_key_changed_while_open_shows_its_number_but_marks_nothing_until_compared_again() {
        let mut check = SafetyCheck::new(MARA);
        check.follow(&state_with(Some([1; 32]), false), number);
        let changed = state_with(Some([2; 32]), false);
        assert!(check.follow(&changed, number));
        assert!(check.key_changed_while_open());
        assert_eq!(check.shown_key(), Some(Fingerprint([2; 32])));
        assert_eq!(check.mark(&changed, true), None);
        check.compare_again();
        let marked = check.mark(&changed, true).unwrap();
        assert!(
            matches!(marked, UiCommand::SetVerified { key, .. } if key == Fingerprint([2; 32]))
        );
    }

    #[test]
    fn a_number_that_isnt_the_presented_keys_is_never_acted_on() {
        let mut check = SafetyCheck::new(MARA);
        check.follow(&state_with(Some([1; 32]), true), number);
        let moved_on = state_with(Some([2; 32]), true);
        assert_eq!(check.mark(&moved_on, false), None);
    }

    #[test]
    fn a_new_key_after_a_verified_one_says_so() {
        let mut check = SafetyCheck::new(MARA);
        let mut state = state_with(Some([2; 32]), false);
        state.key_warnings.push(KeyWarning {
            user: MARA,
            verified: Fingerprint([1; 32]),
            now: Fingerprint([2; 32]),
        });
        check.follow(&state, number);
        let mut moved_on = state_with(Some([3; 32]), false);
        moved_on.key_warnings = state.key_warnings.clone();
        assert_eq!(check.dismiss_warning(&moved_on), None);
        assert!(check.new_key_shown(&state));
        let explanation = check.explanation(&state, "mara").unwrap();
        assert!(explanation.starts_with("This number belongs to mara’s new key."));
        assert_eq!(
            check.dismiss_warning(&state),
            Some(UiCommand::DismissKeyChange {
                user: MARA,
                shown: Some(Fingerprint([2; 32])),
            })
        );
    }
}
