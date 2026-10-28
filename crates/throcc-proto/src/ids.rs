use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UserId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RoomId(pub u64);

/// One track of one membership session. Never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MediaId(pub u32);

/// A room's generation. Bumped on every membership change; selects E2EE keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Epoch(pub u32);

/// SHA-256 of a blob such as an avatar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    User,
    Manager,
    Admin,
}

impl Role {
    pub fn rank(self) -> u8 {
        match self {
            Role::User => 0,
            Role::Manager => 1,
            Role::Admin => 2,
        }
    }

    pub fn from_rank(rank: u8) -> Option<Self> {
        match rank {
            0 => Some(Role::User),
            1 => Some(Role::Manager),
            2 => Some(Role::Admin),
            _ => None,
        }
    }
}
