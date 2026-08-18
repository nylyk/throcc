use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use quinn::Connection;
use throcc_proto::{
    PROTO_VERSION, Req, ReqEnvelope, Resp, RespEnvelope, RoomId, ServerHello, ServerMessage,
};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::control::{ControlReader, ControlWriter};
use crate::{Connector, Error, Keystore, Result};

const QUEUE_DEPTH: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    SetRoom(Option<RoomId>),
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Failed { message: String },
    Disconnected { reason: String },
}

pub struct Client {
    connector: Connector,
    commands: mpsc::Sender<Cmd>,
    events: broadcast::Sender<Event>,
    runtime: Runtime,
}

impl Client {
    /// Blocks until the control stream is up; it then runs on the client's own runtime.
    pub fn connect(address: SocketAddr, server: &str, keystore: Keystore) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let (connector, connection, writer, reader) = runtime.block_on(async {
            let mut connector = Connector::new(keystore)?;
            let connection = connector.connect(address, server).await?;
            let (writer, reader) = open_control(&connection).await?;
            Ok::<_, Error>((connector, connection, writer, reader))
        })?;

        let (commands, command_queue) = mpsc::channel(QUEUE_DEPTH);
        let (events, _) = broadcast::channel(QUEUE_DEPTH);
        runtime.spawn(control(
            connection,
            writer,
            reader,
            command_queue,
            events.clone(),
        ));

        Ok(Self {
            connector,
            commands,
            events,
            runtime,
        })
    }

    pub fn keystore(&self) -> &Keystore {
        self.connector.keystore()
    }

    pub fn cmd(&self, command: Cmd) {
        if let Err(e) = self.commands.try_send(command) {
            tracing::error!(error = %e, "dropped a command");
        }
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Blocks until the runtime has shut down.
    pub fn shutdown(self) {
        let _ = self.commands.try_send(Cmd::Disconnect);
        drop(self.commands);
        self.runtime.shutdown_timeout(SHUTDOWN_GRACE);
    }
}

async fn open_control(connection: &Connection) -> Result<(ControlWriter, ControlReader)> {
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|e| Error::Protocol(format!("accepting the control stream: {e}")))?;
    let writer = ControlWriter::new(send);
    let mut reader = ControlReader::new(recv);

    let hello: ServerHello = reader
        .read()
        .await?
        .ok_or_else(|| Error::Protocol("the control stream closed before the hello".into()))?;
    if hello.proto != PROTO_VERSION {
        return Err(Error::Protocol(format!(
            "server speaks protocol {}, this client speaks {PROTO_VERSION}",
            hello.proto
        )));
    }

    Ok((writer, reader))
}

async fn control(
    connection: Connection,
    writer: ControlWriter,
    reader: ControlReader,
    mut commands: mpsc::Receiver<Cmd>,
    events: broadcast::Sender<Event>,
) {
    let reason = match run(writer, reader, &mut commands, &events).await {
        Ok(()) => connection
            .close_reason()
            .map_or_else(|| "closed".to_string(), |reason| reason.to_string()),
        Err(e) => e.to_string(),
    };

    connection.close(0u32.into(), b"bye");
    let _ = events.send(Event::Disconnected { reason });
}

async fn run(
    mut writer: ControlWriter,
    mut reader: ControlReader,
    commands: &mut mpsc::Receiver<Cmd>,
    events: &broadcast::Sender<Event>,
) -> Result<()> {
    let (inbound, mut server_messages) = mpsc::channel(QUEUE_DEPTH);
    tokio::spawn(async move {
        while let Ok(Some(message)) = reader.read::<ServerMessage>().await {
            if inbound.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut pending: HashMap<u32, oneshot::Sender<Resp>> = HashMap::new();
    let mut next_request_id: u32 = 0;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let req = match command {
                    None | Some(Cmd::Disconnect) => return Ok(()),
                    Some(Cmd::SetRoom(room)) => Req::SetRoom(room),
                };

                let id = next_request_id;
                next_request_id = next_request_id
                    .checked_add(1)
                    .ok_or_else(|| Error::Protocol("request ids exhausted".into()))?;

                let (reply, wait_for_reply) = oneshot::channel();
                pending.insert(id, reply);
                writer.write(&ReqEnvelope { id, req }).await?;

                let events = events.clone();
                tokio::spawn(async move {
                    if let Ok(resp) = wait_for_reply.await
                        && let Some(event) = event_for(resp)
                    {
                        let _ = events.send(event);
                    }
                });
            }

            message = server_messages.recv() => {
                match message {
                    None => return Ok(()),
                    Some(ServerMessage::Event(event)) => tracing::debug!(?event, "event"),
                    Some(ServerMessage::Resp(RespEnvelope { id, resp })) => {
                        let Some(reply) = pending.remove(&id) else {
                            return Err(Error::Protocol(format!(
                                "response {id} answers no pending request"
                            )));
                        };
                        let _ = reply.send(resp);
                    }
                }
            }
        }
    }
}

fn event_for(resp: Resp) -> Option<Event> {
    match resp {
        Resp::Err { code, msg } => Some(Event::Failed {
            message: format!("{code:?}: {msg}"),
        }),
        other => {
            tracing::debug!(?other, "response");
            None
        }
    }
}
