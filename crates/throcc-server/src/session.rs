use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::Connection;
use rand::RngExt as _;
use throcc_proto::{
    Auth, AuthResult, ErrorCode, PROTOCOL_VERSION, Request, RequestEnvelope, Response,
    ResponseEnvelope, Role, ServerHello, ServerMessage, User,
};

use crate::auth::Decision;
use crate::control::{ControlReader, ControlWriter};
use crate::{State, auth, permissions};

const DRAIN_GRACE: Duration = Duration::from_secs(1);

pub async fn serve(connection: Connection, state: Arc<State>) {
    tracing::info!(
        rtt = ?connection.rtt(),
        max_datagram = ?connection.max_datagram_size(),
        "connected"
    );

    if let Err(e) = control(&connection, &state).await {
        tracing::warn!(error = ?e, "control stream ended in error");
        connection.close(1u32.into(), b"protocol error");
    }

    let reason = connection.closed().await;
    tracing::info!(%reason, "disconnected");
}

async fn control(connection: &Connection, state: &State) -> Result<()> {
    let (send, recv) = connection
        .open_bi()
        .await
        .context("opening the control stream")?;
    let mut writer = ControlWriter::new(send);

    let outcome = converse(connection, state, &mut writer, ControlReader::new(recv)).await;
    let _ = tokio::time::timeout(DRAIN_GRACE, writer.drain()).await;
    outcome
}

async fn converse(
    connection: &Connection,
    state: &State,
    writer: &mut ControlWriter,
    mut reader: ControlReader,
) -> Result<()> {
    let server_nonce: [u8; 32] = rand::rng().random();
    writer
        .write(&ServerHello {
            server_nonce,
            protocol: PROTOCOL_VERSION,
        })
        .await?;

    let auth: Auth = reader
        .read()
        .await?
        .context("the client closed before authenticating")?;
    let decision = auth::decide(
        state,
        &auth,
        &server_nonce,
        &auth::tls_channel_binding(connection)?,
        connection.remote_address().ip(),
    )?;
    let actor = match decision {
        Decision::Admitted {
            user,
            initial_state,
        } => {
            writer.write(&AuthResult::Ok(*initial_state)).await?;
            user
        }
        Decision::Refused(error) => {
            writer.write(&AuthResult::Err(error)).await?;
            return Ok(());
        }
    };
    tracing::info!(user = %actor.id, role = ?actor.role, "authenticated");

    while let Some(RequestEnvelope { id, request }) = reader.read().await? {
        tracing::debug!(id, ?request, "request");
        let response = handle(request, &actor, state);
        writer
            .write(&ServerMessage::Response(ResponseEnvelope { id, response }))
            .await?;
    }
    Ok(())
}

fn handle(request: Request, actor: &User, state: &State) -> Response {
    match request {
        Request::CreateInvite => create_invite(actor, state),
        other => Response::Err {
            code: ErrorCode::Unimplemented,
            message: format!("{other:?} is not implemented"),
        },
    }
}

fn create_invite(actor: &User, state: &State) -> Response {
    if let Err(denied) = permissions::require_role(actor.role, Role::Admin) {
        return denied;
    }

    match state.database.create_invite() {
        Ok(created) => {
            tracing::info!(by = %actor.id, expires_at = created.expires_at, "minted an invite");
            Response::InviteCode {
                code: created.code,
                expires: created.expires_at,
            }
        }
        Err(e) => {
            tracing::error!(error = ?e, "could not mint an invite");
            Response::Err {
                code: ErrorCode::Invalid,
                message: "the server could not mint an invite".into(),
            }
        }
    }
}
