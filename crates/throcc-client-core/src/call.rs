use std::sync::Arc;

use anyhow::{Result, bail};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Event, Placement, Request, Response, Welcome};
use tokio::sync::mpsc;
use tracing::warn;

use crate::connection::Connected;
use crate::control::{self, ControlLink, LinkError, Requester};
use crate::floors::EpochFloors;
use crate::keys::KeyNotice;
use crate::keystore::Keystore;
use crate::media::engine::MediaEngine;

const EVENTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
    Server(Event),
    Key(KeyNotice),
}

/// A signed-in connection with its media engine attached to the control stream.
pub struct Call {
    pub welcome: Welcome,
    pub requester: Requester,
    pub engine: Arc<MediaEngine>,
    pub events: mpsc::Receiver<CallEvent>,
    /// Our tracks whose encoder must recover after a local drop.
    pub recovery: mpsc::Receiver<MediaId>,
    pub connection: quinn::Connection,
    /// The newest epochs we took part in, which the session saves now and then.
    pub floors: EpochFloors,
    connected: Connected,
}

impl Call {
    pub fn start(link: ControlLink, keystore: &Keystore) -> Result<Self> {
        Self::start_after(link, keystore, None)
    }

    /// Starts the call that replaces `previous` after a reconnect, keeping the epoch floors
    /// it raised even where they never reached the file.
    pub fn restart(link: ControlLink, keystore: &Keystore, previous: &Call) -> Result<Self> {
        Self::start_after(link, keystore, Some(previous))
    }

    fn start_after(
        link: ControlLink,
        keystore: &Keystore,
        previous: Option<&Call>,
    ) -> Result<Self> {
        let ControlLink {
            welcome,
            connected,
            requester,
            events: mut server_events,
        } = link;
        let connection = connected.connection.clone();
        let enc = keystore.encryption_key_pair()?;
        let floors = match previous {
            Some(previous) => previous.floors.clone(),
            None => EpochFloors::load(keystore.dir(), &connected.fingerprint)?,
        };
        let (engine, mut engine_requests, recovery) = MediaEngine::start(
            connection.clone(),
            welcome.me,
            enc,
            connected.fingerprint,
            floors.clone(),
        );
        let engine = Arc::new(engine);

        for p in &welcome.online {
            engine.set_presence(p.user, p.room);
        }
        let placed_notices = match welcome.placement.room {
            Some(_) => engine.placed(&welcome.placement),
            None => Vec::new(),
        };

        let (events_tx, events) = mpsc::channel(EVENTS);
        for notice in placed_notices {
            events_tx.try_send(CallEvent::Key(notice)).ok();
        }
        let pump_engine = engine.clone();
        tokio::spawn(async move {
            while let Some(event) = server_events.recv().await {
                let notices = pump_engine.event(&event);
                events_tx.send(CallEvent::Server(event)).await.ok();
                for notice in notices {
                    events_tx.send(CallEvent::Key(notice)).await.ok();
                }
            }
        });
        let pump_requester = requester.clone();
        tokio::spawn(async move {
            while let Some(request) = engine_requests.recv().await {
                match pump_requester.request(request).await {
                    Ok(Response::Err { code, message }) => {
                        warn!(?code, "media request refused: {message}")
                    }
                    Ok(_) => {}
                    Err(LinkError::TimedOut) => warn!("a media request went unanswered"),
                    Err(_) => return,
                }
            }
        });

        Ok(Self {
            welcome,
            requester,
            engine,
            events,
            recovery,
            connection,
            floors,
            connected,
        })
    }

    /// Saves the epoch floors and leaves the server cleanly.
    pub async fn close(&self) {
        if let Err(e) = self.floors.save() {
            warn!("saving epoch floors: {e:#}");
        }
        self.connected.close().await;
    }

    pub fn me(&self) -> UserId {
        self.welcome.me
    }

    pub fn server(&self) -> Fingerprint {
        self.connected.fingerprint
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        Ok(self.requester.request(request).await?)
    }

    /// Moves to `room`, with its password if it is locked, and rekeys. Returns the placement
    /// and any key warnings.
    pub async fn join(
        &self,
        room: Option<RoomId>,
        password: Option<String>,
    ) -> Result<(Placement, Vec<KeyNotice>)> {
        let request = match room {
            Some(room) => Request::JoinRoom { room, password },
            None => Request::LeaveRoom,
        };
        match self.request(request).await? {
            Response::Placement(placed) => {
                let notices = self.engine.placed(&placed);
                Ok((placed, notices))
            }
            other => bail!("{}", control::refusal(other)),
        }
    }
}
