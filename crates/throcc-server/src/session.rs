use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use throcc_crypto::identity::random;
use throcc_proto::framing::{self, FrameError, LEN_BYTES};
use throcc_proto::messages::{
    AuthError, AuthResult, ClientHello, ClientMessage, ErrorCode, Event, PROTOCOL, PakeStep,
    Request, Response, ServerHello, ServerMessage,
};
use throcc_proto::transport::close;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};

use crate::ServerState;
use crate::auth::{self, AuthRefusal};
use crate::egress;
use crate::limits::{Pass, SourceKey};
use crate::rooms::{ConnectionHandle, Handled, Outbound, PasswordDone, PasswordWork};

/// How long a connection may wait for a handshake slot, and then how long it has to send
/// its `ClientHello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest a sign-in or enrollment may take after ClientHello, PAKE included.
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
    let mut header = [0u8; LEN_BYTES];
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

/// Serves one connection. Everything before sign-in is logged as counts, since one source
/// can fail sign-ins as fast as it can connect.
pub async fn run(shared: Arc<ServerState>, connection: quinn::Connection, unauthenticated: Pass) {
    let peer = connection.remote_address();
    match serve(&shared, &connection, unauthenticated).await {
        Ok(()) => debug!(%peer, "session ended"),
        Err(ReadError::Closed) => {
            debug!(%peer, "closed before signing in");
            shared.refusals.count("closed before signing in");
        }
        Err(ReadError::Frame(e)) => {
            debug!(%peer, "protocol error before signing in: {e}");
            shared.refusals.count("protocol error before signing in");
            connection.close(close::PROTOCOL_ERROR.into(), e.to_string().as_bytes());
        }
    }
}

/// Signs a connection in and serves it. Errors are returned only before sign-in.
async fn serve(
    shared: &ServerState,
    connection: &quinn::Connection,
    unauthenticated: Pass,
) -> Result<(), ReadError> {
    let peer = connection.remote_address();
    // Nothing reads datagrams until sign-in, and quinn would otherwise keep them, together
    // with the receive buffers they arrived in.
    let discarding = AbortOnDrop(tokio::spawn(discard_datagrams(connection.clone())));
    let source = SourceKey::of(peer.ip());
    let Ok(permit) = tokio::time::timeout(HELLO_TIMEOUT, shared.handshakes.enter(source)).await
    else {
        debug!(%peer, "no handshake slot in time");
        shared.refusals.count("refused: no sign-in slot in time");
        connection.close(
            close::SERVER_BUSY.into(),
            b"too many connections are signing in",
        );
        return Ok(());
    };
    let Ok(greeted) = tokio::time::timeout(HELLO_TIMEOUT, greet(connection)).await else {
        debug!(%peer, "no hello in time");
        shared.refusals.count("no hello in time");
        connection.close(close::PROTOCOL_ERROR.into(), b"no hello in time");
        return Ok(());
    };
    let (mut stream, server_nonce, hello) = greeted?;
    let Some(_signed_in) = shared.connections.try_enter(source) else {
        debug!(%peer, "refusing sign-in: its network is full");
        shared
            .refusals
            .count("refused sign-in: too many signed in from its network");
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
            debug!(%peer, "authentication failed: {refused:?}");
            shared.refusals.count("authentication failed");
            refuse(connection, stream, refused).await;
            return Ok(());
        }
        Err(_) => {
            debug!(%peer, "signing in took too long");
            shared.refusals.count("signing in took too long");
            connection.close(close::PROTOCOL_ERROR.into(), b"signing in took too long");
            return Ok(());
        }
    };

    let user = authenticated.row.id;
    let (outgoing, outgoing_rx) = mpsc::channel(EVENT_QUEUE);
    let handle = ConnectionHandle {
        outgoing: outgoing.clone(),
        connection: connection.clone(),
        egress: egress::spawn(connection.clone(), outgoing.clone()),
    };
    let connected = shared
        .hub()
        .connect(authenticated.row, handle, authenticated.want_room);
    let (conn_id, welcome) = match connected {
        Ok(Some(connected)) => connected,
        Ok(None) => {
            refuse(connection, stream, AuthRefusal::Auth(AuthError::UnknownKey)).await;
            return Ok(());
        }
        Err(e) => {
            warn!("registering a connection: {e:#}");
            refuse(connection, stream, AuthRefusal::Internal).await;
            return Ok(());
        }
    };
    drop(permit);
    drop(unauthenticated);
    drop(discarding);
    let ingress = tokio::spawn(shared.sfu.clone().ingress(conn_id, connection.clone()));
    info!(user = user.0, %peer, "authenticated");

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
    match result {
        Err(ReadError::Frame(e)) => {
            warn!(user = user.0, "protocol error: {e}");
            connection.close(close::PROTOCOL_ERROR.into(), e.to_string().as_bytes());
        }
        Ok(()) | Err(ReadError::Closed) => info!(user = user.0, "signed out"),
    }
    Ok(())
}

async fn discard_datagrams(connection: quinn::Connection) {
    while connection.read_datagram().await.is_ok() {}
}

/// Aborts its task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Opens the control stream, sends `ServerHello` and reads the client's `ClientHello`.
async fn greet(
    connection: &quinn::Connection,
) -> Result<(FrameStream, [u8; 32], ClientHello), ReadError> {
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
    shared: &ServerState,
    user: throcc_proto::ids::UserId,
    conn_id: u64,
    recv: &mut quinn::RecvStream,
    outgoing: &mpsc::Sender<Outbound>,
) -> Result<(), ReadError> {
    let pictures = Arc::new(Semaphore::new(QUEUED_PICTURE_BYTES));
    loop {
        let ClientMessage::Request { id, request: req } = read_frame(recv).await?;
        debug!(user = user.0, id, kind = request_kind(&req), "request");
        let handled = shared.hub().handle(user, conn_id, req);
        let res = match handled {
            Handled::Done(res) => res,
            Handled::Pending(work) => match run_password_work(shared, work).await {
                Some(done) => shared.hub().resume(user, conn_id, done),
                None => Response::Err {
                    code: ErrorCode::Internal,
                    message: "internal server error".into(),
                },
            },
        };
        let response = match &res {
            Response::Avatar(bytes) => {
                let cost = bytes.len().min(QUEUED_PICTURE_BYTES) as u32;
                let Ok(queued) = pictures.clone().acquire_many_owned(cost).await else {
                    return Err(ReadError::Closed);
                };
                Outbound::Budgeted(ServerMessage::Response { id, response: res }, queued)
            }
            _ => Outbound::Message(ServerMessage::Response { id, response: res }),
        };
        if outgoing.send(response).await.is_err() {
            return Err(ReadError::Closed);
        }
    }
}

/// Runs Argon2 on the blocking pool, so neither the hub lock nor an async worker waits
/// on it, and only a few at a time, since each run takes 19 MiB.
async fn run_password_work(shared: &ServerState, work: PasswordWork) -> Option<PasswordDone> {
    let _turn = shared.password_work.acquire().await.ok()?;
    match tokio::task::spawn_blocking(move || work.run()).await {
        Ok(done) => Some(done),
        Err(e) => {
            warn!("room password work failed: {e}");
            None
        }
    }
}

/// A request's name for the log. Requests carry passwords and pictures, so the log never
/// gets the request itself.
fn request_kind(req: &Request) -> &'static str {
    match req {
        Request::JoinRoom { .. } => "JoinRoom",
        Request::LeaveRoom => "LeaveRoom",
        Request::SetProfile { .. } => "SetProfile",
        Request::UploadAvatar(_) => "UploadAvatar",
        Request::FetchAvatar(_) => "FetchAvatar",
        Request::SetEncryptionKey(_) => "SetEncryptionKey",
        Request::SetMedia(_) => "SetMedia",
        Request::CreateRoom { .. } => "CreateRoom",
        Request::RenameRoom { .. } => "RenameRoom",
        Request::DeleteRoom(_) => "DeleteRoom",
        Request::SetRoomPassword { .. } => "SetRoomPassword",
        Request::CreateInvite => "CreateInvite",
        Request::ListUsers => "ListUsers",
        Request::SetRole { .. } => "SetRole",
        Request::RemoveUser { .. } => "RemoveUser",
        Request::SetWatched(_) => "SetWatched",
        Request::RequestKeyframe { .. } => "RequestKeyframe",
        Request::PublishSenderKey { .. } => "PublishSenderKey",
        Request::SetServerName(_) => "SetServerName",
        Request::SetServerAvatar(_) => "SetServerAvatar",
        Request::KickFromRoom { .. } => "KickFromRoom",
    }
}

async fn write_loop(
    mut send: quinn::SendStream,
    mut outgoing: mpsc::Receiver<Outbound>,
    connection: quinn::Connection,
) {
    while let Some(next) = outgoing.recv().await {
        let (message, budget) = match next {
            Outbound::Message(message) => (message, None),
            Outbound::Budgeted(message, budget) => (message, Some(budget)),
            Outbound::Kick { reason, code } => {
                let kicked = ServerMessage::Event(Event::RemovedFromServer {
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
async fn refuse(connection: &quinn::Connection, mut stream: FrameStream, refused: AuthRefusal) {
    let written = match refused {
        AuthRefusal::Auth(e) => send_within(&mut stream.send, &AuthResult::Err(e)).await,
        AuthRefusal::Enroll(e) => send_within(&mut stream.send, &PakeStep::Err(e)).await,
        AuthRefusal::Internal => {
            connection.close(close::SERVER_BUSY.into(), b"internal server error");
            return;
        }
    };
    if written {
        stream.send.finish().ok();
        tokio::time::timeout(LINGER, connection.closed()).await.ok();
    }
    connection.close(close::AUTH_FAILED.into(), b"authentication failed");
}

async fn send_within<T: Serialize>(send: &mut quinn::SendStream, message: &T) -> bool {
    matches!(
        tokio::time::timeout(HELLO_TIMEOUT, write_frame(send, message)).await,
        Ok(Ok(()))
    )
}
