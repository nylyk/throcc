use throcc_proto::frame::TRANSFORM_OVERHEAD_BYTES;
use throcc_proto::{FrameHeader, Result};

/// The only place the payload is touched, so encryption arrives here as one impl
/// rather than as surgery on a working path.
pub trait FrameTransform: Send {
    /// After fragmenting, before the header is prepended and the datagram sent.
    fn outbound(&mut self, header: &FrameHeader, payload: &mut Vec<u8>) -> Result<()>;

    /// After the header is stripped, before reassembly.
    fn inbound(&mut self, header: &FrameHeader, payload: &mut Vec<u8>) -> Result<()>;

    /// Bytes this transform may add, which the datagram budget reserves.
    fn overhead(&self) -> usize;
}

pub struct Passthrough;

impl FrameTransform for Passthrough {
    fn outbound(&mut self, _: &FrameHeader, _: &mut Vec<u8>) -> Result<()> {
        Ok(())
    }

    fn inbound(&mut self, _: &FrameHeader, _: &mut Vec<u8>) -> Result<()> {
        Ok(())
    }

    /// The budget already reserves an AEAD tag, so enabling encryption later does
    /// not shift packet sizes.
    fn overhead(&self) -> usize {
        TRANSFORM_OVERHEAD_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use throcc_proto::{Epoch, MediaId, PAYLOAD_BUDGET};

    #[test]
    fn the_budget_already_reserves_what_the_transform_may_add() {
        assert_eq!(
            PAYLOAD_BUDGET + throcc_proto::HEADER_BYTES + Passthrough.overhead(),
            throcc_proto::MIN_DATAGRAM_BYTES
        );
    }

    #[test]
    fn passing_through_leaves_the_payload_alone() {
        let header = FrameHeader {
            media_id: MediaId(1),
            epoch: Epoch(2),
            seq: 3,
            flags: 0,
        };
        let mut payload = b"opaque".to_vec();

        Passthrough.outbound(&header, &mut payload).unwrap();
        Passthrough.inbound(&header, &mut payload).unwrap();
        assert_eq!(payload, b"opaque");
    }
}
