use crate::ids::{Epoch, MediaId};
use crate::{Error, Result};

pub const HEADER_BYTES: usize = 13;

/// What a DATAGRAM frame's payload has left after quinn's packet overhead, at
/// QUIC's 1200-byte floor. A live connection may report more, and taking that
/// would emit packets some receivers cannot accept.
pub const MIN_DATAGRAM_BYTES: usize = 1162;

/// Reserved for an AEAD tag, so enabling encryption later does not shift packet
/// sizes or turn working packets into silently-dropped oversized ones.
pub const TRANSFORM_OVERHEAD_BYTES: usize = 16;

pub const PAYLOAD_BUDGET: usize = MIN_DATAGRAM_BYTES - HEADER_BYTES - TRANSFORM_OVERHEAD_BYTES;

/// What the peer reported it can carry, against the budget every sender uses.
/// There is no TCP fallback and no smaller frame, so falling short is fatal.
pub fn check_datagram_size(reported: Option<usize>) -> Result<()> {
    match reported {
        None => Err(Error::Datagram(
            "the peer accepts no QUIC datagrams, which is how all media travels".into(),
        )),
        Some(size) if size < MIN_DATAGRAM_BYTES => Err(Error::Datagram(format!(
            "the peer accepts datagrams of {size} bytes, and media needs {MIN_DATAGRAM_BYTES}"
        ))),
        Some(_) => Ok(()),
    }
}

pub const KEYFRAME: u8 = 1 << 0;
const KNOWN_FLAGS: u8 = KEYFRAME;

/// The plaintext routing header every media datagram opens with. Fixed width and
/// big endian, and frozen: `media_id || epoch || seq` is a 96-bit AEAD nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub media_id: MediaId,
    pub epoch: Epoch,
    pub seq: u32,
    pub flags: u8,
}

impl FrameHeader {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0u8; HEADER_BYTES];
        bytes[0..4].copy_from_slice(&self.media_id.0.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.epoch.0.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.seq.to_be_bytes());
        bytes[12] = self.flags;
        bytes
    }

    pub fn decode(datagram: &[u8]) -> Result<Self> {
        let header = datagram.get(..HEADER_BYTES).ok_or(Error::Short {
            need: HEADER_BYTES,
            have: datagram.len(),
        })?;
        Ok(Self {
            media_id: MediaId(u32::from_be_bytes(header[0..4].try_into().unwrap())),
            epoch: Epoch(u32::from_be_bytes(header[4..8].try_into().unwrap())),
            seq: u32::from_be_bytes(header[8..12].try_into().unwrap()),
            flags: header[12],
        })
    }

    pub fn is_keyframe(&self) -> bool {
        self.flags & KEYFRAME != 0
    }

    /// A client refuses a frame carrying a flag it does not know, so it can never
    /// silently misread one. The server reads bit0 and ignores the rest.
    pub fn has_unknown_flags(&self) -> bool {
        self.flags & !KNOWN_FLAGS != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_thirteen_bytes() {
        let header = FrameHeader {
            media_id: MediaId(1),
            epoch: Epoch(2),
            seq: 3,
            flags: KEYFRAME,
        };
        assert_eq!(header.encode().len(), HEADER_BYTES);
        assert_eq!(HEADER_BYTES, 13, "the header width is frozen");
    }

    #[test]
    fn a_header_round_trips() {
        for header in [
            FrameHeader {
                media_id: MediaId(0),
                epoch: Epoch(0),
                seq: 0,
                flags: 0,
            },
            FrameHeader {
                media_id: MediaId(u32::MAX),
                epoch: Epoch(u32::MAX),
                seq: u32::MAX,
                flags: u8::MAX,
            },
            FrameHeader {
                media_id: MediaId(7),
                epoch: Epoch(9),
                seq: 4_000_000_000,
                flags: KEYFRAME,
            },
        ] {
            let encoded = header.encode();
            assert_eq!(FrameHeader::decode(&encoded).unwrap(), header);
        }
    }

    #[test]
    fn a_payload_after_the_header_is_left_alone() {
        let header = FrameHeader {
            media_id: MediaId(7),
            epoch: Epoch(9),
            seq: 11,
            flags: 0,
        };
        let mut datagram = header.encode().to_vec();
        datagram.extend_from_slice(b"opaque");
        assert_eq!(FrameHeader::decode(&datagram).unwrap(), header);
    }

    #[test]
    fn the_fields_are_big_endian_in_the_documented_order() {
        let encoded = FrameHeader {
            media_id: MediaId(0x0102_0304),
            epoch: Epoch(0x0506_0708),
            seq: 0x090a_0b0c,
            flags: 0x0d,
        }
        .encode();
        assert_eq!(
            encoded,
            [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d
            ]
        );
    }

    #[test]
    fn a_short_datagram_is_refused() {
        for length in 0..HEADER_BYTES {
            assert!(FrameHeader::decode(&vec![0u8; length]).is_err());
        }
    }

    #[test]
    fn flags_beyond_the_known_ones_are_recognised_as_unknown() {
        let mut header = FrameHeader {
            media_id: MediaId(1),
            epoch: Epoch(1),
            seq: 1,
            flags: KEYFRAME,
        };
        assert!(header.is_keyframe());
        assert!(!header.has_unknown_flags());

        header.flags |= 1 << 3;
        assert!(header.is_keyframe());
        assert!(header.has_unknown_flags());
    }

    #[test]
    fn a_peer_that_cannot_carry_the_budget_is_refused() {
        assert!(check_datagram_size(None).is_err());
        assert!(check_datagram_size(Some(0)).is_err());
        assert!(check_datagram_size(Some(MIN_DATAGRAM_BYTES - 1)).is_err());
        assert!(check_datagram_size(Some(MIN_DATAGRAM_BYTES)).is_ok());
        assert!(check_datagram_size(Some(1500)).is_ok());
    }

    #[test]
    fn the_budget_leaves_room_for_the_header_and_the_tag() {
        assert_eq!(PAYLOAD_BUDGET, 1133);
        assert_eq!(
            PAYLOAD_BUDGET + HEADER_BYTES + TRANSFORM_OVERHEAD_BYTES,
            MIN_DATAGRAM_BYTES
        );
    }
}
