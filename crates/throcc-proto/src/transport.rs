use std::time::Duration;

use crate::frame::DATAGRAM_PAYLOAD_FLOOR;

pub const KEEP_ALIVE: Duration = Duration::from_secs(5);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub const SERVER_DATAGRAM_SEND_BUFFER: usize = 64 * 1024;
/// Reordering tolerated before a packet counts as lost. Media datagrams are never
/// retransmitted, so a spurious loss only shrinks the congestion window.
pub const REORDER_PACKETS: u32 = 64;
/// The same in time, as a factor of the round-trip time.
pub const REORDER_RTT_FACTOR: f32 = 2.0;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DatagramError {
    #[error("the peer does not support QUIC datagrams, which throcc needs for media")]
    Unsupported,
    #[error("the peer allows only {0}-byte datagrams; throcc needs {floor}", floor = DATAGRAM_PAYLOAD_FLOOR)]
    TooSmall(usize),
}

/// Checks a connection's `max_datagram_size()` against the payload floor.
pub fn check_datagrams(max_datagram_size: Option<usize>) -> Result<(), DatagramError> {
    match max_datagram_size {
        None => Err(DatagramError::Unsupported),
        Some(size) if size < DATAGRAM_PAYLOAD_FLOOR => Err(DatagramError::TooSmall(size)),
        Some(_) => Ok(()),
    }
}

/// QUIC application close codes.
pub mod close {
    pub const NORMAL: u32 = 0;
    pub const DATAGRAMS_REQUIRED: u32 = 1;
    pub const PROTOCOL_ERROR: u32 = 2;
    pub const AUTH_FAILED: u32 = 3;
    pub const REMOVED: u32 = 4;
    pub const SERVER_BUSY: u32 = 5;
}
