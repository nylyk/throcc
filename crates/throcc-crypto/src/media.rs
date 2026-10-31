use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce, Tag};
use hkdf::Hkdf;
use sha2::Sha256;
use throcc_proto::frame::{BODY_BYTES, HEADER_BYTES, Header, HeaderError, TAG_BYTES};
use throcc_proto::ids::MediaId;
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error("plaintext of {0} bytes exceeds the {BODY_BYTES}-byte body")]
    TooLong(usize),
    #[error("datagram too short for a tag")]
    Short,
    #[error("authentication failed")]
    Auth,
}

/// The AES-128-GCM key of one track in one epoch. The key schedule is zeroized on drop.
pub struct TrackKey(Aes128Gcm);

impl TrackKey {
    /// HKDF-SHA256(sender secret, `"throcc/media/1" || media_id`).
    pub fn derive(sender_secret: &[u8; 32], media_id: MediaId) -> Self {
        let mut info = b"throcc/media/1".to_vec();
        info.extend_from_slice(&media_id.0.to_be_bytes());
        let mut key = Zeroizing::new([0u8; 16]);
        Hkdf::<Sha256>::new(None, sender_secret)
            .expand(&info, key.as_mut())
            .expect("16 bytes is a valid HKDF length");
        Self(Aes128Gcm::new_from_slice(key.as_ref()).expect("AES-128 takes a 16-byte key"))
    }

    /// Writes `header || AES-GCM(parts) || tag` into `out`. Nonce = header bytes 0–11,
    /// AAD = all 13 header bytes. The caller guarantees `(media_id, epoch, seq)` is fresh.
    pub fn seal(
        &self,
        header: &Header,
        parts: &[&[u8]],
        out: &mut Vec<u8>,
    ) -> Result<(), MediaError> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len > BODY_BYTES {
            return Err(MediaError::TooLong(len));
        }
        let header_bytes = header.encode();
        out.clear();
        out.extend_from_slice(&header_bytes);
        for part in parts {
            out.extend_from_slice(part);
        }
        let tag = self
            .0
            .encrypt_inout_detached(
                &Nonce::from(header.nonce()),
                &header_bytes,
                (&mut out[HEADER_BYTES..]).into(),
            )
            .map_err(|_| MediaError::TooLong(len))?;
        out.extend_from_slice(&tag);
        Ok(())
    }

    /// Decrypts a datagram in place and returns its header and plaintext.
    pub fn open<'a>(&self, datagram: &'a mut [u8]) -> Result<(Header, &'a [u8]), MediaError> {
        let header = Header::parse_strict(datagram)?;
        let tag_at = datagram
            .len()
            .checked_sub(TAG_BYTES)
            .filter(|at| *at >= HEADER_BYTES)
            .ok_or(MediaError::Short)?;
        let (sealed, tag) = datagram.split_at_mut(tag_at);
        let tag = Tag::try_from(&*tag).map_err(|_| MediaError::Short)?;
        let (aad, body) = sealed.split_at_mut(HEADER_BYTES);
        self.0
            .decrypt_inout_detached(&Nonce::from(header.nonce()), aad, (&mut *body).into(), &tag)
            .map_err(|_| MediaError::Auth)?;
        Ok((header, body))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use throcc_proto::frame::Flags;
    use throcc_proto::ids::Epoch;

    use super::*;

    fn header(seq: u32) -> Header {
        Header {
            media_id: MediaId(7),
            epoch: Epoch(3),
            seq,
            flags: Flags::FIRST.with(Flags::LAST, true),
        }
    }

    /// The same datagram built with ring, an independent HKDF and AES-GCM implementation.
    fn seal_with_ring(secret: &[u8; 32], header: &Header, plaintext: &[u8]) -> Vec<u8> {
        use ring::{aead, hkdf};
        struct Len16;
        impl hkdf::KeyType for Len16 {
            fn len(&self) -> usize {
                16
            }
        }
        let info = [
            b"throcc/media/1".as_slice(),
            &header.media_id.0.to_be_bytes(),
        ]
        .concat();
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(secret);
        let mut key = [0u8; 16];
        prk.expand(&[&info], Len16).unwrap().fill(&mut key).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &key).unwrap());
        let header_bytes = header.encode();
        let mut body = plaintext.to_vec();
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(header.nonce()),
            aead::Aad::from(header_bytes),
            &mut body,
        )
        .unwrap();
        [header_bytes.as_slice(), &body].concat()
    }

    #[test]
    fn known_answer() {
        let key = TrackKey::derive(&[0x42; 32], MediaId(7));
        let mut out = Vec::new();
        key.seal(&header(1), &[b"throcc"], &mut out).unwrap();
        assert_eq!(out, seal_with_ring(&[0x42; 32], &header(1), b"throcc"));
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "00000007000000030000000106f8e2e7162665ddc39b31c48f1ecc0fb414e005aa3c92"
        );
    }

    #[test]
    fn round_trips() {
        let key = TrackKey::derive(&[1; 32], MediaId(7));
        let mut out = Vec::new();
        key.seal(&header(9), &[&[1, 2, 3, 4], b"payload"], &mut out)
            .unwrap();
        let (parsed, plaintext) = key.open(&mut out).unwrap();
        assert_eq!(parsed, header(9));
        assert_eq!(plaintext, b"\x01\x02\x03\x04payload");
    }

    #[test]
    fn every_header_byte_is_authenticated() {
        let key = TrackKey::derive(&[1; 32], MediaId(7));
        let mut sealed = Vec::new();
        key.seal(&header(9), &[b"payload"], &mut sealed).unwrap();
        for i in 0..HEADER_BYTES {
            for bit in 0..8 {
                let mut modified = sealed.clone();
                modified[i] ^= 1 << bit;
                assert!(key.open(&mut modified).is_err(), "byte {i} bit {bit}");
            }
        }
    }

    #[test]
    fn other_tracks_and_secrets_fail() {
        let mut sealed = Vec::new();
        TrackKey::derive(&[1; 32], MediaId(7))
            .seal(&header(9), &[b"x"], &mut sealed)
            .unwrap();
        let other_track = TrackKey::derive(&[1; 32], MediaId(8));
        let other_secret = TrackKey::derive(&[2; 32], MediaId(7));
        assert_eq!(
            other_track.open(&mut sealed.clone()).err(),
            Some(MediaError::Auth)
        );
        assert_eq!(
            other_secret.open(&mut sealed.clone()).err(),
            Some(MediaError::Auth)
        );
    }

    #[test]
    fn size_limits() {
        let key = TrackKey::derive(&[1; 32], MediaId(7));
        let mut out = Vec::new();
        key.seal(&header(1), &[&[0; BODY_BYTES]], &mut out).unwrap();
        assert_eq!(out.len(), throcc_proto::frame::DATAGRAM_PAYLOAD_FLOOR);
        assert_eq!(
            key.seal(&header(1), &[&[0; BODY_BYTES + 1]], &mut out),
            Err(MediaError::TooLong(BODY_BYTES + 1))
        );
        assert_eq!(
            key.open(&mut [0u8; HEADER_BYTES + TAG_BYTES - 1]).err(),
            Some(MediaError::Short)
        );
    }

    proptest! {
        #[test]
        fn matches_ring(secret in any::<[u8; 32]>(), media in any::<u32>(), seq in any::<u32>(),
                        plaintext in proptest::collection::vec(any::<u8>(), 0..BODY_BYTES)) {
            let header = Header { media_id: MediaId(media), epoch: Epoch(seq ^ 5), seq, flags: Flags::FIRST };
            let mut out = Vec::new();
            TrackKey::derive(&secret, MediaId(media)).seal(&header, &[&plaintext], &mut out).unwrap();
            prop_assert_eq!(out, seal_with_ring(&secret, &header, &plaintext));
        }

        #[test]
        fn open_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let key = TrackKey::derive(&[1; 32], MediaId(7));
            let mut bytes = bytes;
            let _ = key.open(&mut bytes);
        }
    }
}
