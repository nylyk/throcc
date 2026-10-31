use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use throcc_crypto::identity::{self, IdentityKey, random};
use throcc_crypto::pake::{ClientPake, PakeError, Transcript};
use throcc_proto::framing::{self, FrameError, HEADER_BYTES};
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    AuthError, AuthResult, ClientMessage, Event, Hello, PROTOCOL, PakeFinish, PakeStep, Request,
    Response, ServerHello, ServerMessage, Welcome,
};
use throcc_proto::transport::close;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::connection::Connected;
use crate::lock;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const EVENT_QUEUE: usize = 1024;
const OUTGOING_QUEUE: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error(transparent)]
    Refused(#[from] AuthError),
    #[error(transparent)]
    Pake(#[from] PakeError),
    #[error("the server speaks protocol {0}; this client speaks {PROTOCOL}")]
    Protocol(u16),
    #[error("the server closed the connection during login")]
    Closed,
    #[error("the server sent a malformed message: {0}")]
    Malformed(#[from] FrameError),
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("not connected")]
    Closed,
    #[error("request ids are exhausted; reconnect")]
    IdsExhausted,
}

/// An authenticated control stream.
pub struct Link {
    pub welcome: Welcome,
    pub connected: Connected,
    pub requester: Requester,
    pub events: mpsc::Receiver<Event>,
}

/// Logs in with a key this server already knows.
pub async fn login(
    connected: Connected,
    identity: &IdentityKey,
    want_room: Option<RoomId>,
) -> Result<Link, LoginError> {
    let (mut send, mut recv) = open(&connected).await?;
    let hello: ServerHello = read(&mut recv).await?;
    check_protocol(&connected, &hello)?;
    let client_nonce: [u8; 32] = random();
    let message = identity::auth_message(&hello.nonce, &client_nonce, &connected.exporter);
    let auth = Hello::Auth {
        pubkey: identity.public(),
        client_nonce,
        want_room,
        signature: identity.sign(&message),
    };
    write(&mut send, &auth).await?;
    finish_login(connected, send, recv).await
}

/// Enrolls with an invite code. A server that can't prove it knows the code fails here.
pub async fn enroll(
    connected: Connected,
    identity: &IdentityKey,
    code: &InviteCode,
) -> Result<Link, LoginError> {
    let (mut send, mut recv) = open(&connected).await?;
    let hello: ServerHello = read(&mut recv).await?;
    check_protocol(&connected, &hello)?;
    let client_nonce: [u8; 32] = random();
    let pubkey = identity.public();
    let (pake, pake_a) = ClientPake::start(&code.id, &code.secret);
    let enroll = Hello::Enroll {
        code_id: code.id.clone(),
        pubkey,
        client_nonce,
        pake_a: pake_a.clone(),
    };
    write(&mut send, &enroll).await?;

    let step: PakeStep = read(&mut recv).await?;
    let reply = step?;
    let transcript = Transcript {
        server_nonce: &hello.nonce,
        client_nonce: &client_nonce,
        code_id: &code.id,
        pubkey: &pubkey,
        spki_hash: &connected.fingerprint.0,
        exporter: &connected.exporter,
        pake_a: &pake_a,
        pake_b: &reply.pake_b,
    };
    let confirm_a = match pake.finish(&transcript, &reply.confirm_b) {
        Ok(confirm_a) => confirm_a,
        Err(e) => {
            connected
                .connection
                .close(close::AUTH_FAILED.into(), b"key confirmation failed");
            return Err(e.into());
        }
    };
    let finish = PakeFinish {
        confirm_a,
        signature: identity.sign(&transcript.to_bytes()),
    };
    write(&mut send, &finish).await?;
    finish_login(connected, send, recv).await
}

async fn open(connected: &Connected) -> Result<(quinn::SendStream, quinn::RecvStream), LoginError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, connected.connection.accept_bi())
        .await
        .map_err(|_| LoginError::Closed)?
        .map_err(|_| LoginError::Closed)
}

fn check_protocol(connected: &Connected, hello: &ServerHello) -> Result<(), LoginError> {
    if hello.protocol != PROTOCOL {
        connected
            .connection
            .close(close::PROTOCOL_ERROR.into(), b"protocol mismatch");
        return Err(LoginError::Protocol(hello.protocol));
    }
    Ok(())
}

async fn finish_login(
    connected: Connected,
    send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) -> Result<Link, LoginError> {
    let result: AuthResult = read(&mut recv).await?;
    let welcome = match result {
        AuthResult::Ok(welcome) => *welcome,
        AuthResult::Err(e) => {
            connected.connection.close(close::NORMAL.into(), b"");
            return Err(e.into());
        }
    };
    let (requester, events) = spawn_link(connected.connection.clone(), send, recv);
    Ok(Link {
        welcome,
        connected,
        requester,
        events,
    })
}

async fn read<T: DeserializeOwned>(recv: &mut quinn::RecvStream) -> Result<T, LoginError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(recv))
        .await
        .map_err(|_| LoginError::Closed)?
        .map_err(|e| match e {
            ReadError::Closed => LoginError::Closed,
            ReadError::Frame(e) => LoginError::Malformed(e),
        })
}

async fn write<T: Serialize>(send: &mut quinn::SendStream, message: &T) -> Result<(), LoginError> {
    write_frame(send, message)
        .await
        .map_err(|_| LoginError::Closed)
}

#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("stream closed")]
    Closed,
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// Reads one frame. The body is allocated only after its length passes the cap.
async fn read_frame<T: DeserializeOwned>(recv: &mut quinn::RecvStream) -> Result<T, ReadError> {
    let mut header = [0u8; HEADER_BYTES];
    recv.read_exact(&mut header)
        .await
        .map_err(|_| ReadError::Closed)?;
    let mut body = vec![0u8; framing::body_len(header)?];
    recv.read_exact(&mut body)
        .await
        .map_err(|_| ReadError::Closed)?;
    Ok(framing::decode(&body)?)
}

async fn write_frame<T: Serialize>(
    send: &mut quinn::SendStream,
    message: &T,
) -> Result<(), ReadError> {
    let frame = framing::encode(message)?;
    send.write_all(&frame).await.map_err(|_| ReadError::Closed)
}

#[derive(Default)]
struct Pending {
    next_id: u32,
    waiting: HashMap<u32, oneshot::Sender<Response>>,
}

/// Sends requests on the control stream. Cheap to clone; many requests may be in flight.
#[derive(Clone)]
pub struct Requester {
    outgoing: mpsc::Sender<ClientMessage>,
    pending: Arc<Mutex<Pending>>,
    connection: quinn::Connection,
}

impl Requester {
    pub async fn request(&self, req: Request) -> Result<Response, LinkError> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut pending = lock(&self.pending);
            let id = pending.next_id;
            let Some(next) = id.checked_add(1) else {
                drop(pending);
                self.connection
                    .close(close::PROTOCOL_ERROR.into(), b"request ids exhausted");
                return Err(LinkError::IdsExhausted);
            };
            pending.next_id = next;
            pending.waiting.insert(id, tx);
            id
        };
        self.outgoing
            .send(ClientMessage::Request { id, req })
            .await
            .map_err(|_| LinkError::Closed)?;
        rx.await.map_err(|_| LinkError::Closed)
    }
}

fn spawn_link(
    connection: quinn::Connection,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) -> (Requester, mpsc::Receiver<Event>) {
    let (outgoing, mut outgoing_rx) = mpsc::channel::<ClientMessage>(OUTGOING_QUEUE);
    let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
    let pending = Arc::new(Mutex::new(Pending::default()));

    tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if write_frame(&mut send, &message).await.is_err() {
                return;
            }
        }
    });

    let reader_pending = pending.clone();
    let reader_connection = connection.clone();
    tokio::spawn(async move {
        loop {
            let message = match read_frame::<ServerMessage>(&mut recv).await {
                Ok(message) => message,
                Err(ReadError::Closed) => break,
                Err(ReadError::Frame(e)) => {
                    warn!("protocol error from server: {e}");
                    reader_connection.close(close::PROTOCOL_ERROR.into(), e.to_string().as_bytes());
                    break;
                }
            };
            match message {
                ServerMessage::Response { id, res } => {
                    let waiter = lock(&reader_pending).waiting.remove(&id);
                    match waiter {
                        Some(waiter) => {
                            waiter.send(res).ok();
                        }
                        None => {
                            warn!(id, "reply to a request that was never sent");
                            reader_connection
                                .close(close::PROTOCOL_ERROR.into(), b"unknown reply id");
                            break;
                        }
                    }
                }
                ServerMessage::Event(event) => {
                    events_tx.send(event).await.ok();
                }
            }
        }
        lock(&reader_pending).waiting.clear();
    });

    (
        Requester {
            outgoing,
            pending,
            connection,
        },
        events,
    )
}
