use std::collections::{HashMap, HashSet};

use anyhow::Result;
use throcc_proto::Fingerprint;
use throcc_proto::ids::UserId;
use throcc_proto::limits::MAX_ONLINE;
use throcc_proto::messages::PublicKey;
use tracing::warn;

use crate::state::KeyWarning;
use crate::verified::VerifiedStore;

/// Who is verified on one server. Only a key presented this session counts: someone
/// verified before shows as verified only once they present that key again, since nothing
/// else ties the account now behind their user id to it.
pub(crate) struct Trust {
    server: Fingerprint,
    /// The identity key each user last presented this session, with an encryption key it
    /// validly signed for them here.
    presented: HashMap<UserId, PublicKey>,
    verified: VerifiedStore,
}

impl Trust {
    pub fn new(server: Fingerprint, verified: VerifiedStore) -> Self {
        Self {
            server,
            presented: HashMap::new(),
            verified,
        }
    }

    /// Forgets every presented key, for a new connection, where each must be presented again.
    pub fn forget_presented(&mut self) {
        self.presented.clear();
    }

    /// Notes the identity keys `peers` present. True if any is new. Once [`MAX_ONLINE`] are
    /// noted, only those of `peers` and of people with a changed key stay.
    pub fn note(&mut self, peers: &[(UserId, PublicKey)]) -> bool {
        let new: Vec<(UserId, PublicKey)> = peers
            .iter()
            .filter(|(user, key)| self.presented.get(user) != Some(key))
            .copied()
            .collect();
        if new.is_empty() {
            return false;
        }
        let mut verified = self.verified.lock();
        if self.presented.len() + new.len() > MAX_ONLINE {
            let here: HashSet<UserId> = peers.iter().map(|(user, _)| *user).collect();
            let server = &self.server;
            self.presented.retain(|user, _| {
                here.contains(user) || verified.changed_at(server, *user).is_some()
            });
        }
        for (user, key) in new {
            self.presented.insert(user, key);
            if let Err(e) = verified.presented(&self.server, user, &key) {
                warn!("noting where a verified key was seen: {e:#}");
            }
        }
        true
    }

    /// Whether `user` is verified, and the identity key they presented this session.
    pub fn of(&self, user: UserId) -> (bool, Option<PublicKey>) {
        match self.presented.get(&user) {
            Some(key) => (self.verified.lock().is_verified(key), Some(*key)),
            None => (false, None),
        }
    }

    /// The warning that the key shown as `user` replaced one the user verified.
    pub fn warning(&self, user: UserId) -> Option<KeyWarning> {
        let verified = self.verified.lock();
        let now = verified.changed_at(&self.server, user)?;
        let before = verified.verified_at(&self.server, user)?;
        Some(KeyWarning {
            user,
            verified: Fingerprint(before),
            now: Fingerprint(now),
        })
    }

    /// Verifies `key` as `name`, or takes it off the verified ones, if `user` still presents
    /// it: the user compared the safety number of that key, not of one presented since.
    /// False, changing nothing, when they don't.
    pub fn set_verified(
        &mut self,
        user: UserId,
        key: &PublicKey,
        verified: bool,
        name: &str,
    ) -> Result<bool> {
        if self.presented.get(&user) != Some(key) {
            return Ok(false);
        }
        if verified {
            self.verified.lock().verify(&self.server, user, key, name)?;
        } else {
            self.verified.lock().unverify(key)?;
        }
        Ok(true)
    }

    /// Stops warning that `user`'s verified key changed. Where the verified key is the one
    /// they present again, it is expected there anew, so a later swap is still warned of.
    /// Dismisses the warning for `user` if they still present `shown`, the key the user
    /// looked at. Returns whether it was dismissed.
    pub fn dismiss_change(&mut self, user: UserId, shown: Option<&PublicKey>) -> Result<bool> {
        let presented = self.presented.get(&user).copied();
        if presented.as_ref() != shown {
            return Ok(false);
        }
        self.verified
            .lock()
            .dismiss_change(&self.server, user, presented.as_ref())?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Fingerprint = Fingerprint([1; 32]);
    const MARA: UserId = UserId(4);
    const REAL: PublicKey = [4; 32];
    const IMPOSTOR: PublicKey = [9; 32];

    fn trust_in(dir: &std::path::Path) -> Trust {
        Trust::new(SERVER, VerifiedStore::load(dir).unwrap())
    }

    /// Mara verified with her real key, then shown with the impostor's.
    fn verified_then_changed(dir: &std::path::Path) -> Trust {
        let mut trust = trust_in(dir);
        trust.note(&[(MARA, REAL)]);
        trust.set_verified(MARA, &REAL, true, "Mara").unwrap();
        trust.note(&[(MARA, IMPOSTOR)]);
        trust
    }

    #[test]
    fn only_the_key_whose_number_was_compared_is_verified() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = trust_in(dir.path());
        trust.note(&[(MARA, REAL)]);
        let compared = trust.of(MARA).1.unwrap();
        trust.note(&[(MARA, IMPOSTOR)]);
        assert!(!trust.set_verified(MARA, &compared, true, "Mara").unwrap());
        assert_eq!(trust.of(MARA), (false, Some(IMPOSTOR)));
        assert!(trust.verified.list().is_empty());

        trust.note(&[(MARA, REAL)]);
        assert!(trust.set_verified(MARA, &compared, true, "Mara").unwrap());
        assert_eq!(trust.of(MARA), (true, Some(REAL)));
        trust.note(&[(MARA, IMPOSTOR)]);
        assert_eq!(trust.of(MARA), (false, Some(IMPOSTOR)));
        let warning = trust.warning(MARA).unwrap();
        assert_eq!(
            (warning.verified, warning.now),
            (Fingerprint(REAL), Fingerprint(IMPOSTOR))
        );

        assert!(!trust.set_verified(MARA, &REAL, false, "").unwrap());
        assert_eq!(
            trust.verified.list().len(),
            1,
            "unverified while another key was shown"
        );
        assert!(!trust.set_verified(MARA, &REAL, true, "Mara").unwrap());
        trust.note(&[(MARA, REAL)]);
        assert!(trust.set_verified(MARA, &REAL, false, "").unwrap());
        assert!(trust.verified.list().is_empty());
        assert!(trust.warning(MARA).is_none());
    }

    #[test]
    fn a_key_verified_before_counts_only_once_presented_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = trust_in(dir.path());
        trust.note(&[(MARA, REAL)]);
        trust.set_verified(MARA, &REAL, true, "Mara").unwrap();

        let mut next = trust_in(dir.path());
        assert_eq!(next.of(MARA), (false, None));
        next.note(&[(MARA, REAL)]);
        assert_eq!(next.of(MARA), (true, Some(REAL)));
        next.forget_presented();
        assert_eq!(next.of(MARA), (false, None));
    }

    #[test]
    fn a_changed_key_is_warned_of_after_a_restart_and_once_the_old_key_is_back() {
        let dir = tempfile::tempdir().unwrap();
        let _changed = verified_then_changed(dir.path());

        let mut next = trust_in(dir.path());
        assert!(next.warning(MARA).is_some(), "lost on a restart");
        next.note(&[(MARA, REAL)]);
        assert!(next.warning(MARA).is_some(), "hidden by the old key");
        assert!(next.dismiss_change(MARA, Some(&REAL)).unwrap());
        assert!(next.warning(MARA).is_none());
    }

    #[test]
    fn a_swap_after_dismissing_while_the_verified_key_is_back_is_warned_of() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = verified_then_changed(dir.path());
        trust.note(&[(MARA, REAL)]);
        assert!(trust.dismiss_change(MARA, Some(&REAL)).unwrap());
        assert!(trust.warning(MARA).is_none());
        assert_eq!(trust.of(MARA), (true, Some(REAL)));

        trust.note(&[(MARA, [10; 32])]);
        let warning = trust.warning(MARA).expect("the swap went unwarned");
        assert_eq!(warning.now, Fingerprint([10; 32]));
    }

    #[test]
    fn dismissing_while_a_new_key_is_shown_accepts_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = verified_then_changed(dir.path());
        assert!(trust.dismiss_change(MARA, Some(&IMPOSTOR)).unwrap());
        trust.note(&[(MARA, [10; 32])]);
        assert!(trust.warning(MARA).is_none());
    }

    #[test]
    fn a_dismiss_for_a_key_no_longer_shown_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = verified_then_changed(dir.path());
        trust.note(&[(MARA, REAL)]);
        trust.note(&[(MARA, [10; 32])]);
        assert!(!trust.dismiss_change(MARA, Some(&REAL)).unwrap());
        assert!(trust.warning(MARA).is_some());
    }

    #[test]
    fn people_with_a_changed_key_stay_noted_when_many_are() {
        let dir = tempfile::tempdir().unwrap();
        let mut trust = verified_then_changed(dir.path());
        let room = throcc_proto::limits::MAX_ROOM_MEMBERS as u64;
        for batch in 0..(MAX_ONLINE as u64 / room + 1) {
            let peers: Vec<(UserId, PublicKey)> = (0..room)
                .map(|i| (UserId(1000 + batch * room + i), [7; 32]))
                .collect();
            trust.note(&peers);
        }
        assert_eq!(trust.of(MARA), (false, Some(IMPOSTOR)));
        assert!(trust.presented.len() <= MAX_ONLINE + room as usize);
    }
}
