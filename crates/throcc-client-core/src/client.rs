use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::Connection;
use throcc_proto::{
    Epoch, MediaId, PeerState, Placed, Request, RequestEnvelope, Response, ResponseEnvelope, Role,
    Room, RoomId, ServerMessage, UserId,
};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::auth::{self, Welcome};
use crate::control::{ControlReader, ControlWriter};
use crate::media::receive;
use crate::media::send::{self, MediaSender};
use crate::{Connector, Error, Keystore, Result};

const QUEUE_DEPTH: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
/// This must stay under `SHUTDOWN_GRACE`, or the runtime tears the drain down
/// mid-flush.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    SetRoom(Option<RoomId>),
    CreateRoom { name: String },
    RenameRoom { room: RoomId, name: String },
    DeleteRoom(RoomId),
    CreateInvite { role: Role, ttl_secs: u32 },
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Placed(Placed),
    Media {
        media_id: MediaId,
        timestamp: Option<u32>,
        bytes: Vec<u8>,
    },
    UserEntered {
        room: RoomId,
        epoch: Epoch,
        peer: PeerState,
    },
    UserExited {
        room: RoomId,
        epoch: Epoch,
        user: UserId,
    },
    RoomCreated(Room),
    RoomRenamed {
        room: RoomId,
        name: String,
    },
    RoomDeleted(RoomId),
    Invited {
        code: String,
        expires: u64,
    },
    Failed {
        message: String,
    },
    Disconnected {
        reason: String,
    },
}

pub struct Client {
    welcome: Welcome,
    media: Arc<MediaSender>,
    connector: Connector,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
    disconnect_reason: Arc<Mutex<Option<String>>>,
    control: JoinHandle<()>,
    runtime: Runtime,
}

impl Client {
    /// This blocks until the client is authenticated, and the session then runs on
    /// the client's own runtime.
    pub fn connect(
        address: SocketAddr,
        server: &str,
        keystore: Keystore,
        invite_code: Option<String>,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let (connector, connection, writer, reader, welcome) = runtime.block_on(async {
            let mut connector = Connector::new(keystore)?;
            let connection = connector.connect(address, server).await?;
            let (mut writer, mut reader) = open_control(&connection).await?;
            let welcome = auth::handshake(
                &connection,
                &mut writer,
                &mut reader,
                connector.keystore().identity(),
                invite_code,
            )
            .await?;
            Ok::<_, Error>((connector, connection, writer, reader, welcome))
        })?;

        let (commands, command_queue) = mpsc::channel(QUEUE_DEPTH);
        let (events, _) = broadcast::channel(QUEUE_DEPTH);
        let (datagrams, datagram_queue) = mpsc::channel(send::QUEUE_DEPTH);
        let media = Arc::new(MediaSender::new(datagrams, welcome.placed.epoch));
        let disconnect_reason = Arc::new(Mutex::new(None));

        runtime.spawn(send::drain(connection.clone(), datagram_queue));
        runtime.spawn(receive::receive(connection.clone(), events.clone()));
        let control = runtime.spawn(control(
            connection,
            writer,
            reader,
            command_queue,
            events.clone(),
            media.clone(),
            disconnect_reason.clone(),
        ));

        Ok(Self {
            welcome,
            media,
            connector,
            commands,
            events,
            disconnect_reason,
            control,
            runtime,
        })
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn keystore(&self) -> &Keystore {
        self.connector.keystore()
    }

    pub fn command(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|e| Error::CommandDropped(e.to_string()))
    }

    /// The handle media is sent through, cloneable so a capture thread can hold
    /// one of its own.
    pub fn media(&self) -> Arc<MediaSender> {
        self.media.clone()
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        let ended = self
            .disconnect_reason
            .lock()
            .expect("disconnect reason mutex poisoned");
        match ended.clone() {
            None => self.events.subscribe(),
            Some(reason) => {
                let (replay, receiver) = broadcast::channel(1);
                let _ = replay.send(Event::Disconnected { reason });
                receiver
            }
        }
    }

    /// This blocks until the server has been told the session is over, so the server
    /// sees a close rather than an idle timeout.
    pub fn shutdown(self) {
        let Self {
            connector,
            commands,
            control,
            runtime,
            ..
        } = self;

        let _ = commands.try_send(Command::Disconnect);
        drop(commands);

        runtime.block_on(async {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
                let _ = control.await;
                connector.wait_idle().await;
            })
            .await;
        });
        runtime.shutdown_timeout(SHUTDOWN_GRACE);
    }
}

async fn open_control(connection: &Connection) -> Result<(ControlWriter, ControlReader)> {
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|e| Error::Protocol(format!("accepting the control stream: {e}")))?;
    Ok((ControlWriter::new(send), ControlReader::new(recv)))
}

async fn control(
    connection: Connection,
    mut writer: ControlWriter,
    reader: ControlReader,
    mut commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    media: Arc<MediaSender>,
    disconnect_reason: Arc<Mutex<Option<String>>>,
) {
    let outcome = run(&mut writer, reader, &mut commands, &events, &media).await;
    let _ = tokio::time::timeout(DRAIN_GRACE, writer.drain()).await;

    let reason = match outcome {
        Ok(()) => connection
            .close_reason()
            .map_or_else(|| "closed".to_string(), |reason| reason.to_string()),
        Err(e) => e.to_string(),
    };

    connection.close(0u32.into(), b"bye");
    *disconnect_reason
        .lock()
        .expect("disconnect reason mutex poisoned") = Some(reason.clone());
    let _ = events.send(Event::Disconnected { reason });
}

async fn run(
    writer: &mut ControlWriter,
    mut reader: ControlReader,
    commands: &mut mpsc::Receiver<Command>,
    events: &broadcast::Sender<Event>,
    media: &MediaSender,
) -> Result<()> {
    let (inbound, mut server_messages) = mpsc::channel(QUEUE_DEPTH);
    tokio::spawn(async move {
        loop {
            match reader.read::<ServerMessage>().await {
                Ok(Some(message)) => {
                    if inbound.send(Ok(message)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = inbound.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    let mut pending: HashMap<u32, oneshot::Sender<Response>> = HashMap::new();
    let mut next_request_id: u32 = 0;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let request = match command {
                    None | Some(Command::Disconnect) => return Ok(()),
                    Some(Command::SetRoom(room)) => Request::SetRoom(room),
                    Some(Command::CreateRoom { name }) => Request::CreateRoom { name },
                    Some(Command::RenameRoom { room, name }) => {
                        Request::RenameRoom { room, name }
                    }
                    Some(Command::DeleteRoom(room)) => Request::DeleteRoom(room),
                    Some(Command::CreateInvite { role, ttl_secs }) => {
                        Request::CreateInvite { role, ttl_secs }
                    }
                };

                let id = next_request_id;
                next_request_id = next_request_id
                    .checked_add(1)
                    .ok_or_else(|| Error::Protocol("request ids exhausted".into()))?;

                let (reply, wait_for_reply) = oneshot::channel();
                pending.insert(id, reply);
                writer.write(&RequestEnvelope { id, request }).await?;

                let events = events.clone();
                tokio::spawn(async move {
                    if let Ok(response) = wait_for_reply.await
                        && let Some(event) = event_for(response)
                    {
                        let _ = events.send(event);
                    }
                });
            }

            message = server_messages.recv() => {
                match message {
                    None => return Ok(()),
                    Some(Err(e)) => return Err(e),
                    Some(Ok(ServerMessage::Event(event))) => {
                        if let Some(event) = translate(event) {
                            observe_epoch(media, &event);
                            let _ = events.send(event);
                        }
                    }
                    Some(Ok(ServerMessage::Response(ResponseEnvelope { id, response }))) => {
                        if let Response::Placed(placed) = &response {
                            media.observe(placed.epoch);
                        }
                        let Some(reply) = pending.remove(&id) else {
                            return Err(Error::Protocol(format!(
                                "response {id} answers no pending request"
                            )));
                        };
                        let _ = reply.send(response);
                    }
                }
            }
        }
    }
}

fn observe_epoch(media: &MediaSender, event: &Event) {
    match event {
        Event::Placed(placed) => media.observe(placed.epoch),
        Event::UserEntered { epoch, .. } | Event::UserExited { epoch, .. } => media.observe(*epoch),
        _ => {}
    }
}

fn translate(event: throcc_proto::Event) -> Option<Event> {
    match event {
        throcc_proto::Event::UserEntered { room, epoch, peer } => {
            Some(Event::UserEntered { room, epoch, peer })
        }
        throcc_proto::Event::UserExited { room, epoch, user } => {
            Some(Event::UserExited { room, epoch, user })
        }
        throcc_proto::Event::RoomCreated(room) => Some(Event::RoomCreated(room)),
        throcc_proto::Event::RoomRenamed { room, name } => Some(Event::RoomRenamed { room, name }),
        throcc_proto::Event::RoomDeleted(room) => Some(Event::RoomDeleted(room)),
        other => {
            tracing::debug!(?other, "event");
            None
        }
    }
}

fn event_for(response: Response) -> Option<Event> {
    match response {
        Response::Placed(placed) => Some(Event::Placed(placed)),
        Response::InviteCode { code, expires } => Some(Event::Invited { code, expires }),
        Response::Err { code, message } => Some(Event::Failed {
            message: format!("{code:?}: {message}"),
        }),
        other => {
            tracing::debug!(?other, "response");
            None
        }
    }
}
