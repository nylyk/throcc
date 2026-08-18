use quinn::{RecvStream, SendStream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use throcc_proto::framing::{self, LENGTH_PREFIX_BYTES};

use crate::{Error, Result};

pub struct ControlWriter(SendStream);

impl ControlWriter {
    pub fn new(send: SendStream) -> Self {
        Self(send)
    }

    pub async fn write<T: Serialize>(&mut self, message: &T) -> Result<()> {
        let frame = framing::encode(message)?;
        self.0
            .write_all(&frame)
            .await
            .map_err(|e| Error::Protocol(format!("writing to the control stream: {e}")))
    }
}

pub struct ControlReader(RecvStream);

impl ControlReader {
    pub fn new(recv: RecvStream) -> Self {
        Self(recv)
    }

    /// `None` once the peer has closed the stream cleanly.
    pub async fn read<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
        match self.0.read_exact(&mut prefix).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(e) => {
                return Err(Error::Protocol(format!(
                    "reading a control frame length: {e}"
                )));
            }
        }

        let mut body = vec![0u8; framing::body_len(prefix)?];
        self.0
            .read_exact(&mut body)
            .await
            .map_err(|e| Error::Protocol(format!("reading a control frame body: {e}")))?;
        Ok(Some(framing::decode(&body)?))
    }
}
