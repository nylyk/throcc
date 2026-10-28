use serde::Serialize;
use serde::de::DeserializeOwned;

pub const MAX_FRAME_BYTES: usize = 1 << 20;
pub const HEADER_BYTES: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the 1 MiB cap")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(#[from] postcard::Error),
    #[error("malformed message: {0} trailing bytes")]
    Trailing(usize),
}

/// A big-endian `u32` length followed by the postcard body.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut out = postcard::to_extend(message, vec![0u8; HEADER_BYTES])?;
    let len = out.len() - HEADER_BYTES;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    out[..HEADER_BYTES].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(out)
}

/// Reads a frame header. Callers allocate the body only after this succeeds.
pub fn body_len(header: [u8; HEADER_BYTES]) -> Result<usize, FrameError> {
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    Ok(len)
}

pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    let (message, rest) = postcard::take_from_bytes(body)?;
    if !rest.is_empty() {
        return Err(FrameError::Trailing(rest.len()));
    }
    Ok(message)
}

pub type BodyAndRest<'a> = (&'a [u8], &'a [u8]);

/// Splits one complete frame off the front of `buf`, returning its body and the remainder.
/// Returns `None` while the frame is incomplete.
pub fn split(buf: &[u8]) -> Result<Option<BodyAndRest<'_>>, FrameError> {
    let Some((header, rest)) = buf.split_first_chunk::<HEADER_BYTES>() else {
        return Ok(None);
    };
    let len = body_len(*header)?;
    Ok(rest.split_at_checked(len))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::ids::{MediaId, RoomId};
    use crate::messages::{ClientMessage, Request};

    #[test]
    fn round_trips() {
        let message = ClientMessage::Request {
            id: 7,
            req: Request::SetRoom(Some(RoomId(3))),
        };
        let frame = encode(&message).unwrap();
        let (body, rest) = split(&frame).unwrap().unwrap();
        assert!(rest.is_empty());
        assert_eq!(decode::<ClientMessage>(body).unwrap(), message);
    }

    #[test]
    fn rejects_oversized_header_before_reading_body() {
        assert!(matches!(body_len([0xff; 4]), Err(FrameError::TooLarge(_))));
        let header = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(matches!(split(&header), Err(FrameError::TooLarge(_))));
        assert_eq!(
            body_len((MAX_FRAME_BYTES as u32).to_be_bytes()).unwrap(),
            MAX_FRAME_BYTES
        );
    }

    #[test]
    fn refuses_to_encode_oversized_frames() {
        let message = ClientMessage::Request {
            id: 1,
            req: Request::SetAvatar(vec![0; MAX_FRAME_BYTES]),
        };
        assert!(matches!(encode(&message), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut frame = encode(&ClientMessage::Request {
            id: 1,
            req: Request::ListUsers,
        })
        .unwrap();
        frame.push(0);
        let body = &frame[HEADER_BYTES..];
        assert!(matches!(
            decode::<ClientMessage>(body),
            Err(FrameError::Trailing(1))
        ));
    }

    proptest! {
        #[test]
        fn split_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Ok(Some((body, _))) = split(&buf) {
                let _ = decode::<ClientMessage>(body);
            }
        }

        #[test]
        fn watching_round_trips(id in any::<u32>(), ids in proptest::collection::vec(any::<u32>(), 0..16)) {
            let message = ClientMessage::Request {
                id,
                req: Request::SetWatching(ids.into_iter().map(MediaId).collect()),
            };
            let frame = encode(&message).unwrap();
            let (body, _) = split(&frame).unwrap().unwrap();
            prop_assert_eq!(decode::<ClientMessage>(body).unwrap(), message);
        }
    }
}
