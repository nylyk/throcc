use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use throcc_crypto::identity::random;
use throcc_proto::framing::{self, FrameError, HEADER_BYTES};
use throcc_proto::messages::{
    AuthError, AuthResult, ClientMessage, Event, Hello, PROTOCOL, PakeStep, Response, ServerHello,
    ServerMessage,
};
use throcc_proto::transport::close;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};

use crate::Shared;
use crate::auth::{self, Refused, SourceKey};
use crate::egress;
use crate::limits::Pass;
use crate::rooms::{Link, Outgoing};

/// How long a connection may wait for a handshake slot, and then how long it has to send
/// its `Hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest a sign-in or enrollment may take after Hello, PAKE included.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(40);
pub const HANDSHAKE_FRAME_BYTES: usize = 4096;
const EVENT_QUEUE: usize = 1024;
/// Picture bytes a connection may have waiting for its client to read.
const QUEUED_PICTURE_BYTES: usize = 256 * 1024;
/// How long a closing session waits for the client to read its last message.
pub(crate) const LINGER: Duration = Duration::from_secs(2);

pub struct FrameStream {
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("stream closed")]
    Closed,
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// Reads one frame. The body is allocated only after its length passes the cap.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut quinn::RecvStream) -> Result<T, ReadError> {
    read_capped(recv, framing::MAX_FRAME_BYTES).await
}

/// Reads one handshake frame, which is never larger than [`HANDSHAKE_FRAME_BYTES`].
pub async fn read_handshake<T: DeserializeOwned>(
    recv: &mut quinn::RecvStream,
) -> Result<T, ReadError> {
    read_capped(recv, HANDSHAKE_FRAME_BYTES).await
}

async fn read_capped<T: DeserializeOwned>(
    recv: &mut quinn::RecvStream,
    cap: usize,
) -> Result<T, ReadError> {
    let mut header = [0u8; HEADER_BYTES];
    recv.read_exact(&mut header)
        .await
        .map_err(|_| ReadError::Closed)?;
    let len = framing::body_len(header)?;
    if len > cap {
        return Err(FrameError::TooLarge(len).into());
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|_| ReadError::Closed)?;
    Ok(framing::decode(&body)?)
}

pub async fn write_frame<T: Serialize>(
    send: &mut quinn::SendStream,
    message: &T,
) -> anyhow::Result<()> {
    send.write_all(&framing::encode(message)?).await?;
    Ok(())
}

pub async fn run(shared: Arc<Shared>, connection: quinn::Connection, unauthenticated: Pass) {
    let peer = connection.remote_address();
    match serve(&shared, &connection, unauthenticated).await {
        Ok(()) => info!(%peer, "session ended"),
        Err(ReadError::Closed) => info!(%peer, "session closed"),
        Err(ReadError::Frame(e)) => {
            warn!(%peer, "protocol error: {e}");
            connection.close(close::PROTOCOL_ERROR.into(), e.to_string().as_bytes());
        }
    }
}

async fn serve(
    shared: &Shared,
    connection: &quinn::Connection,
    unauthenticated: Pass,
) -> Result<(), ReadError> {
    let source = SourceKey::of(connection.remote_address().ip());
    let Ok(permit) = tokio::time::timeout(HELLO_TIMEOUT, shared.handshakes.enter(source)).await
    else {
        connection.close(
            close::SERVER_BUSY.into(),
            b"too many connections are signing in",
        );
        return Ok(());
    };
    let Ok(greeted) = tokio::time::timeout(HELLO_TIMEOUT, greet(connection)).await else {
        connection.close(close::PROTOCOL_ERROR.into(), b"no hello in time");
        return Ok(());
    };
    let (mut stream, server_nonce, hello) = greeted?;
    let Some(_signed_in) = shared.connections.try_enter(source) else {
        warn!(peer = %connection.remote_address(), "refusing sign-in: its network is full");
        connection.close(
            close::SERVER_BUSY.into(),
            b"too many connections from your network",
        );
        return Ok(());
    };

    let signing_in = auth::authenticate(shared, connection, &mut stream, &server_nonce, hello);
    let authenticated = match tokio::time::timeout(SIGN_IN_TIMEOUT, signing_in).await {
        Ok(Ok(authenticated)) => authenticated,
        Ok(Err(refused)) => {
            info!(peer = %connection.remote_address(), "authentication failed: {refused:?}");
            refuse(connection, stream, refused).await;
            return Ok(());
        }
        Err(_) => {
            connection.close(close::PROTOCOL_ERROR.into(), b"signing in took too long");
            return Ok(());
        }
    };

    let user = authenticated.row.id;
    let (outgoing, outgoing_rx) = mpsc::channel(EVENT_QUEUE);
    let link = Link {
        outgoing: outgoing.clone(),
        connection: connection.clone(),
        egress: egress::spawn(connection.clone(), outgoing.clone()),
    };
    let connected = shared
        .hub()
        .connect(authenticated.row, link, authenticated.want_room);
    let (conn_id, welcome) = match connected {
        Ok(Some(connected)) => connected,
        Ok(None) => {
            refuse(connection, stream, Refused::Hello(AuthError::UnknownKey)).await;
            return Ok(());
        }
        Err(e) => {
            warn!("registering a connection: {e:#}");
            refuse(connection, stream, Refused::Internal).await;
            return Ok(());
        }
    };
    drop(permit);
    drop(unauthenticated);
    let ingress = tokio::spawn(shared.sfu.clone().ingress(conn_id, connection.clone()));
    info!(user = user.0, "authenticated");

    let FrameStream { mut send, mut recv } = stream;
    let sent = write_frame(&mut send, &AuthResult::Ok(Box::new(welcome))).await;
    let writer = tokio::spawn(write_loop(send, outgoing_rx, connection.clone()));
    let result = match sent {
        Ok(()) => read_loop(shared, user, conn_id, &mut recv, &outgoing).await,
        Err(_) => Err(ReadError::Closed),
    };
    shared.hub().disconnect(user, conn_id);
    writer.abort();
    ingress.abort();
    result
}

/// Opens the control stream, sends `ServerHello` and reads the client's `Hello`.
async fn greet(
    connection: &quinn::Connection,
) -> Result<(FrameStream, [u8; 32], Hello), ReadError> {
    let (send, recv) = connection.open_bi().await.map_err(|_| ReadError::Closed)?;
    let mut stream = FrameStream { send, recv };
    let server_nonce: [u8; 32] = random();
    write_frame(
        &mut stream.send,
        &ServerHello {
            nonce: server_nonce,
            protocol: PROTOCOL,
        },
    )
    .await
    .map_err(|_| ReadError::Closed)?;
    let hello = read_handshake(&mut stream.recv).await?;
    Ok((stream, server_nonce, hello))
}

async fn read_loop(
    shared: &Shared,
    user: throcc_proto::ids::UserId,
    conn_id: u64,
    recv: &mut quinn::RecvStream,
    outgoing: &mpsc::Sender<Outgoing>,
) -> Result<(), ReadError> {
    let pictures = Arc::new(Semaphore::new(QUEUED_PICTURE_BYTES));
    loop {
        let ClientMessage::Request { id, req } = read_frame(recv).await?;
        debug!(user = user.0, id, ?req, "request");
        let res = shared.hub().handle(user, conn_id, req);
        let response = match &res {
            Response::Avatar(bytes) => {
                let cost = bytes.len().min(QUEUED_PICTURE_BYTES) as u32;
                let Ok(queued) = pictures.clone().acquire_many_owned(cost).await else {
                    return Err(ReadError::Closed);
                };
                Outgoing::Budgeted(ServerMessage::Response { id, res }, queued)
            }
            _ => Outgoing::Message(ServerMessage::Response { id, res }),
        };
        if outgoing.send(response).await.is_err() {
            return Err(ReadError::Closed);
        }
    }
}

async fn write_loop(
    mut send: quinn::SendStream,
    mut outgoing: mpsc::Receiver<Outgoing>,
    connection: quinn::Connection,
) {
    while let Some(next) = outgoing.recv().await {
        let (message, budget) = match next {
            Outgoing::Message(message) => (message, None),
            Outgoing::Budgeted(message, budget) => (message, Some(budget)),
            Outgoing::Kick { reason, code } => {
                let kicked = ServerMessage::Event(Event::Kicked {
                    reason: reason.clone(),
                });
                if write_frame(&mut send, &kicked).await.is_ok() && send.finish().is_ok() {
                    tokio::time::timeout(LINGER, send.stopped()).await.ok();
                }
                connection.close(code.into(), reason.as_bytes());
                return;
            }
        };
        if write_frame(&mut send, &message).await.is_err() {
            return;
        }
        drop(budget);
    }
}

/// Sends a refusal and lets the client close first, so the refusal isn't lost to our close.
async fn refuse(connection: &quinn::Connection, mut stream: FrameStream, refused: Refused) {
    let written = match refused {
        Refused::Hello(e) => {
            let result = AuthResult::Err(e);
            tokio::time::timeout(HELLO_TIMEOUT, write_frame(&mut stream.send, &result)).await
        }
        Refused::Enroll(e) => {
            let step = PakeStep::Err(e);
            tokio::time::timeout(HELLO_TIMEOUT, write_frame(&mut stream.send, &step)).await
        }
        Refused::Internal => {
            connection.close(close::SERVER_BUSY.into(), b"internal server error");
            return;
        }
    };
    if matches!(written, Ok(Ok(_))) {
        stream.send.finish().ok();
        tokio::time::timeout(LINGER, connection.closed()).await.ok();
    }
    connection.close(close::AUTH_FAILED.into(), b"authentication failed");
}
