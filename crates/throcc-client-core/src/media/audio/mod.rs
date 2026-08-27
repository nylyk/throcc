pub mod capture;
pub mod cleanup;
pub mod devices;
pub mod jitter;
pub mod playout;

pub const SAMPLE_RATE: u32 = 48_000;

/// The processing chain's frame is fixed at 10 ms, and Opus encodes 20 ms, so
/// capture buffers two blocks per packet.
pub const SAMPLES_PER_BLOCK: usize = (SAMPLE_RATE / 100) as usize;
pub const BLOCKS_PER_PACKET: usize = 2;
pub const SAMPLES_PER_PACKET: usize = SAMPLES_PER_BLOCK * BLOCKS_PER_PACKET;
