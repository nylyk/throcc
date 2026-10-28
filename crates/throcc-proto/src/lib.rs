pub mod fingerprint;
pub mod frame;
pub mod transport;

pub use fingerprint::Fingerprint;

pub const ALPN: &[u8] = b"throcc/1";
pub const DEFAULT_PORT: u16 = 8476;
/// Sent as SNI. Clients pin the server's key, so the name carries no meaning.
pub const SERVER_NAME: &str = "throcc.invalid";
