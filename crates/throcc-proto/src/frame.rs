use crate::ids::{Epoch, MediaId};

/// Smallest DATAGRAM payload quinn offers on a 1200-byte UDP path with the pinned quinn
/// version and its default connection id length.
pub const DATAGRAM_PAYLOAD_FLOOR: usize = 1162;
pub const HEADER_BYTES: usize = 13;
pub const NONCE_BYTES: usize = 12;
pub const TAG_BYTES: usize = 16;
/// Ciphertext body per datagram, before the tag.
pub const BODY_BYTES: usize = DATAGRAM_PAYLOAD_FLOOR - HEADER_BYTES - TAG_BYTES;
/// The media timestamp at the start of every access unit's first fragment.
pub const TIMESTAMP_BYTES: usize = 4;
pub const AUDIO_CLOCK_HZ: u32 = 48_000;
pub const VIDEO_CLOCK_HZ: u32 = 90_000;

const _: () = assert!(BODY_BYTES == 1133);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(pub u8);

impl Flags {
    pub const KEYFRAME: Self = Self(1);
    pub const FIRST: Self = Self(1 << 1);
    pub const LAST: Self = Self(1 << 2);
    /// Nothing later references this frame.
    pub const DISCARDABLE: Self = Self(1 << 3);
    const DEFINED: u8 = 0x0f;

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn with(self, other: Self, on: bool) -> Self {
        if on {
            Self(self.0 | other.0)
        } else {
            Self(self.0 & !other.0)
        }
    }

    /// The four defined bits; reserved bits cleared.
    pub fn defined(self) -> Self {
        Self(self.0 & Self::DEFINED)
    }

    pub fn has_reserved(self) -> bool {
        self.0 & !Self::DEFINED != 0
    }
}

/// The plaintext header of every media datagram. Fixed width, big endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Header {
    pub media_id: MediaId,
    pub epoch: Epoch,
    pub seq: u32,
    pub flags: Flags,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("datagram shorter than the header")]
    Short,
    #[error("reserved flag bits are set")]
    ReservedFlags,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut out = [0u8; HEADER_BYTES];
        out[0..4].copy_from_slice(&self.media_id.0.to_be_bytes());
        out[4..8].copy_from_slice(&self.epoch.0.to_be_bytes());
        out[8..12].copy_from_slice(&self.seq.to_be_bytes());
        out[12] = self.flags.0;
        out
    }

    /// Parses the header the way the server does: reserved flag bits are kept but ignored.
    pub fn parse(datagram: &[u8]) -> Result<Self, HeaderError> {
        let bytes: &[u8; HEADER_BYTES] = datagram.first_chunk().ok_or(HeaderError::Short)?;
        let word =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        Ok(Self {
            media_id: MediaId(word(0)),
            epoch: Epoch(word(4)),
            seq: word(8),
            flags: Flags(bytes[12]),
        })
    }

    /// Parses the header the way clients do: unknown flag bits are an error.
    pub fn parse_strict(datagram: &[u8]) -> Result<Self, HeaderError> {
        let header = Self::parse(datagram)?;
        if header.flags.has_reserved() {
            return Err(HeaderError::ReservedFlags);
        }
        Ok(header)
    }

    /// The AEAD nonce: header bytes 0–11.
    pub fn nonce(&self) -> [u8; NONCE_BYTES] {
        let [nonce @ .., _flags] = self.encode();
        nonce
    }
}

/// Serial-number comparison for media timestamps (RFC 1982, 32-bit).
pub fn timestamp_before(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < 1 << 31
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn layout_is_fixed() {
        let header = Header {
            media_id: MediaId(0x0102_0304),
            epoch: Epoch(0x0506_0708),
            seq: 0x090a_0b0c,
            flags: Flags::FIRST.with(Flags::LAST, true),
        };
        assert_eq!(header.encode(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 6]);
        assert_eq!(header.nonce(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn reserved_bits_are_ignored_by_the_server_and_rejected_by_clients() {
        let mut bytes = [0u8; HEADER_BYTES];
        bytes[12] = 0x80 | Flags::KEYFRAME.0;
        let header = Header::parse(&bytes).unwrap();
        assert_eq!(header.flags.defined(), Flags::KEYFRAME);
        assert_eq!(
            Header::parse_strict(&bytes),
            Err(HeaderError::ReservedFlags)
        );
        assert_eq!(Header::parse(&bytes[..12]), Err(HeaderError::Short));
    }

    #[test]
    fn timestamps_compare_across_wrap() {
        assert!(timestamp_before(u32::MAX - 10, 5));
        assert!(!timestamp_before(5, u32::MAX - 10));
        assert!(timestamp_before(1, 2));
        assert!(!timestamp_before(2, 2));
    }

    proptest! {
        #[test]
        fn header_round_trips(media in any::<u32>(), epoch in any::<u32>(), seq in any::<u32>(), flags in 0u8..16) {
            let header = Header { media_id: MediaId(media), epoch: Epoch(epoch), seq, flags: Flags(flags) };
            prop_assert_eq!(Header::parse_strict(&header.encode()).unwrap(), header);
        }

        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..32)) {
            let _ = Header::parse(&bytes);
            let _ = Header::parse_strict(&bytes);
        }
    }
}
