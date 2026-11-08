/// The most people in one room.
pub const MAX_ROOM_MEMBERS: usize = 30;
/// The most screen shares running in one room at once.
pub const MAX_ROOM_SHARES: usize = 5;
/// The most tracks one member may watch: a share is two tracks, its picture and its sound,
/// so this is every share in the room.
pub const MAX_WATCHED: usize = 2 * MAX_ROOM_SHARES;
/// The most rooms on one server.
pub const MAX_ROOMS: usize = 50;
/// The most people signed in to one server at once. Admins with an account still sign in
/// past it, up to [`MAX_SIGNED_IN`], so a full server can be managed.
pub const MAX_ONLINE: usize = 250;
/// How many admins may sign in past [`MAX_ONLINE`].
pub const ADMIN_HEADROOM: usize = 10;
/// The most people signed in to one server, admins past [`MAX_ONLINE`] included. Nobody
/// signs in past it.
pub const MAX_SIGNED_IN: usize = MAX_ONLINE + ADMIN_HEADROOM;
