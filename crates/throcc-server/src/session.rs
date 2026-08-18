use std::time::Duration;

use anyhow::{Context, Result};
use quinn::Connection;
use rand::RngExt as _;
use throcc_proto::{
    ErrCode, PROTO_VERSION, Req, ReqEnvelope, Resp, RespEnvelope, ServerHello, ServerMessage,
};

use crate::control::{ControlReader, ControlWriter};

const DRAIN_GRACE: Duration = Duration::from_secs(1);

pub async fn serve(connection: Connection) {
    tracing::info!(
        rtt = ?connection.rtt(),
        max_datagram = ?connection.max_datagram_size(),
        "connected"
    );

    if let Err(e) = control(&connection).await {
        tracing::warn!(error = ?e, "control stream ended in error");
        connection.close(1u32.into(), b"protocol error");
    }

    let reason = connection.closed().await;
    tracing::info!(%reason, "disconnected");
}

async fn control(connection: &Connection) -> Result<()> {
    let (send, recv) = connection
        .open_bi()
        .await
        .context("opening the control stream")?;
    let mut writer = ControlWriter::new(send);

    let outcome = answer_requests(&mut writer, ControlReader::new(recv)).await;
    if outcome.is_err() {
        let _ = tokio::time::timeout(DRAIN_GRACE, writer.drain()).await;
    }
    outcome
}

async fn answer_requests(writer: &mut ControlWriter, mut reader: ControlReader) -> Result<()> {
    let server_nonce: [u8; 32] = rand::rng().random();
    writer
        .write(&ServerHello {
            server_nonce,
            proto: PROTO_VERSION,
        })
        .await?;

    while let Some(ReqEnvelope { id, req }) = reader.read().await? {
        tracing::debug!(id, ?req, "request");
        let resp = handle(req);
        writer
            .write(&ServerMessage::Resp(RespEnvelope { id, resp }))
            .await?;
    }
    Ok(())
}

fn handle(req: Req) -> Resp {
    Resp::Err {
        code: ErrCode::Unimplemented,
        msg: format!("{req:?} is not implemented"),
    }
}
