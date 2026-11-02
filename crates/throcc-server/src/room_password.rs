use std::collections::HashMap;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use throcc_crypto::identity::random;
use throcc_proto::ids::{RoomId, UserId};

use crate::limits::Buckets;

/// Wrong guesses a user gets at one room before each next one has to wait.
const FREE_FAILURES: u32 = 5;
/// The wait after the last free failure, doubling with each failure after it.
const FIRST_WAIT: Duration = Duration::from_secs(10);
const LONGEST_WAIT: Duration = Duration::from_secs(60 * 60);
/// How long a user's wrong guesses at a room are remembered.
const FAILURE_MEMORY: Duration = Duration::from_secs(24 * 60 * 60);
const FAILURE_SWEEP: Duration = Duration::from_secs(60);
/// Wrong guesses at one room, from everyone together, per hour. Once they are used up,
/// only users without wrong guesses there may try, so guessers can't lock members out.
const ROOM_FAILURES: f64 = 100.0;
const ROOM_FAILURES_PER_SECOND: f64 = ROOM_FAILURES / 3600.0;

/// Hashes `password` with Argon2id and a fresh salt, as a PHC string.
pub fn hash(password: &str) -> anyhow::Result<String> {
    let salt: [u8; 16] = random();
    let hash = Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing a room password: {e}"))?;
    Ok(hash.to_string())
}

pub fn matches(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// Why a guess at a room's password has to wait.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    /// This user guessed wrong too often; they may try again after this long.
    User(Duration),
    /// The room has had too many wrong guesses, and this user made some of them.
    Room,
}

#[derive(Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
}

impl Failures {
    fn next_try(self) -> Instant {
        let Some(extra) = self.count.checked_sub(FREE_FAILURES) else {
            return self.last;
        };
        let wait = FIRST_WAIT.saturating_mul(1 << extra.min(16));
        self.last + wait.min(LONGEST_WAIT)
    }
}

/// Wrong room-password guesses: an exponential backoff per user and room, and an allowance
/// per room.
pub struct Guesses {
    users: HashMap<(UserId, RoomId), Failures>,
    rooms: Buckets<RoomId>,
    next_sweep: Option<Instant>,
}

impl Default for Guesses {
    fn default() -> Self {
        Self {
            users: HashMap::new(),
            rooms: Buckets::new(ROOM_FAILURES, ROOM_FAILURES_PER_SECOND),
            next_sweep: None,
        }
    }
}

impl Guesses {
    pub fn check(&mut self, user: UserId, room: RoomId, now: Instant) -> Result<(), Wait> {
        let Some(failures) = self.users.get(&(user, room)).copied() else {
            return Ok(());
        };
        let next_try = failures.next_try();
        if now < next_try {
            return Err(Wait::User(next_try - now));
        }
        if !self.rooms.holds(room, 1.0, now) {
            return Err(Wait::Room);
        }
        Ok(())
    }

    /// Records a wrong guess. Returns the user's wrong guesses at the room so far, and
    /// whether the room's allowance is used up.
    pub fn failed(&mut self, user: UserId, room: RoomId, now: Instant) -> (u32, bool) {
        if self.next_sweep.is_none_or(|t| now >= t) {
            self.users
                .retain(|_, f| now.saturating_duration_since(f.last) < FAILURE_MEMORY);
            self.next_sweep = Some(now + FAILURE_SWEEP);
        }
        let exhausted = !self.rooms.take(room, 1.0, now);
        let failures = self.users.entry((user, room)).or_insert(Failures {
            count: 0,
            last: now,
        });
        failures.count += 1;
        failures.last = now;
        (failures.count, exhausted)
    }

    pub fn succeeded(&mut self, user: UserId, room: RoomId) {
        self.users.remove(&(user, room));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: RoomId = RoomId(1);

    #[test]
    fn a_user_waits_longer_after_each_wrong_guess_past_the_free_ones() {
        let start = Instant::now();
        let mut guesses = Guesses::default();
        let user = UserId(1);
        for _ in 0..FREE_FAILURES {
            assert_eq!(guesses.check(user, ROOM, start), Ok(()));
            guesses.failed(user, ROOM, start);
        }
        assert_eq!(
            guesses.check(user, ROOM, start),
            Err(Wait::User(FIRST_WAIT))
        );
        let later = start + FIRST_WAIT;
        assert_eq!(guesses.check(user, ROOM, later), Ok(()));
        guesses.failed(user, ROOM, later);
        assert_eq!(
            guesses.check(user, ROOM, later),
            Err(Wait::User(FIRST_WAIT * 2))
        );
        assert_eq!(guesses.check(user, RoomId(2), later), Ok(()));
        assert_eq!(guesses.check(UserId(2), ROOM, later), Ok(()));
        for _ in 0..40 {
            guesses.failed(user, ROOM, later);
        }
        assert_eq!(
            guesses.check(user, ROOM, later),
            Err(Wait::User(LONGEST_WAIT))
        );
        guesses.succeeded(user, ROOM);
        assert_eq!(guesses.check(user, ROOM, later), Ok(()));
    }

    #[test]
    fn a_room_with_too_many_wrong_guesses_still_lets_new_guessers_try() {
        let start = Instant::now();
        let mut guesses = Guesses::default();
        let mut exhausted = false;
        for user in 0..ROOM_FAILURES as u64 + 1 {
            exhausted = guesses.failed(UserId(user), ROOM, start).1;
        }
        assert!(exhausted);
        assert_eq!(guesses.check(UserId(0), ROOM, start), Err(Wait::Room));
        assert_eq!(guesses.check(UserId(999), ROOM, start), Ok(()));
        let refilled = start + Duration::from_secs(60);
        assert_eq!(guesses.check(UserId(0), ROOM, refilled), Ok(()));
    }

    #[test]
    fn old_failures_are_forgotten() {
        let start = Instant::now();
        let mut guesses = Guesses::default();
        guesses.failed(UserId(1), ROOM, start);
        guesses.failed(
            UserId(2),
            ROOM,
            start + FAILURE_MEMORY + Duration::from_secs(1),
        );
        assert_eq!(guesses.users.len(), 1);
    }

    #[test]
    fn only_the_same_password_matches_its_hash() {
        let hashed = hash("correct horse").unwrap();
        assert!(matches("correct horse", &hashed));
        assert!(!matches("correct horsE", &hashed));
        assert_ne!(hashed, hash("correct horse").unwrap());
    }
}
