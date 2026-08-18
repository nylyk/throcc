use core::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident, $inner:ty) => {
        #[derive(
            Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
        )]
        pub struct $name(pub $inner);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id!(UserId, u64);
id!(RoomId, u64);
id!(MediaId, u32);
id!(Epoch, u32);
