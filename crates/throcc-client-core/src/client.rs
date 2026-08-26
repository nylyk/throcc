use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::Connection;
use throcc_proto::{
    Request, RequestEnvelope, Response, ResponseEnvelope, Role, RoomId, ServerMessage,
};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::auth::{self, Welcome};
use crate::control::{ControlReader, ControlWriter};
use crate::{Connector, Error, Keystore, Result};

const QUEUE_DEPTH: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
/// This must stay under `SHUTDOWN_GRACE`, or the runtime tears the drain down
/// mid-flush.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    SetRoom(Option<RoomId>),
    CreateInvite { role: Role, ttl_secs: u32 },
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Invited { code: String, expires: u64 },
    Failed { message: String },
    Disconnected { reason: String },
}

pub struct Client {
    welcome: Welcome,
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
        let disconnect_reason = Arc::new(Mutex::new(None));
        let control = runtime.spawn(control(
            connection,
            writer,
            reader,
            command_queue,
            events.clone(),
            disconnect_reason.clone(),
        ));

        Ok(Self {
            welcome,
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
    disconnect_reason: Arc<Mutex<Option<String>>>,
) {
    let outcome = run(&mut writer, reader, &mut commands, &events).await;
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
                    Some(Ok(ServerMessage::Event(event))) => tracing::debug!(?event, "event"),
                    Some(Ok(ServerMessage::Response(ResponseEnvelope { id, response }))) => {
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

fn event_for(response: Response) -> Option<Event> {
    match response {
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
