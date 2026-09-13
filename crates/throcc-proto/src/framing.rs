use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// The message as postcard bytes behind a big-endian `u32` length.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let body = postcard::to_allocvec(message)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(Error::TooLarge {
            len: body.len(),
            max: MAX_FRAME_BYTES,
        });
    }

    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// The body length a frame declares. An oversized one is rejected here, before
/// its reader allocates for it.
pub fn body_length(prefix: [u8; LENGTH_PREFIX_BYTES]) -> Result<usize> {
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(Error::TooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(len)
}

pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    Ok(postcard::from_bytes(body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ErrorCode, Response};

    fn round_trip(message: &Response) -> Response {
        let frame = encode(message).unwrap();
        let len = body_length(frame[..LENGTH_PREFIX_BYTES].try_into().unwrap()).unwrap();
        assert_eq!(len, frame.len() - LENGTH_PREFIX_BYTES);
        decode(&frame[LENGTH_PREFIX_BYTES..]).unwrap()
    }

    #[test]
    fn a_frame_survives_encoding() {
        let message = Response::Err {
            code: ErrorCode::Denied,
            message: "nope".into(),
        };
        assert_eq!(round_trip(&message), message);
    }

    #[test]
    fn an_oversized_length_is_refused_before_allocating() {
        let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(matches!(body_length(prefix), Err(Error::TooLarge { .. })));
        assert!(body_length((MAX_FRAME_BYTES as u32).to_be_bytes()).is_ok());
    }
}
