//! The limits server and client share. The server enforces them; the client refuses server
//! input that goes past them, so a hostile server can't make it hold or search more.

/// The most people in one room.
pub const MAX_ROOM_MEMBERS: usize = 30;
/// The most screen shares running in one room at once.
pub const MAX_ROOM_SHARES: usize = 5;
/// The most tracks one member may watch: a share is two tracks, its picture and its sound,
/// so this is every share in the room.
pub const MAX_WATCHED: usize = 2 * MAX_ROOM_SHARES;
/// The most rooms on one server.
pub const MAX_ROOMS: usize = 50;
/// The most people signed in to one server at once.
pub const MAX_ONLINE: usize = 250;
