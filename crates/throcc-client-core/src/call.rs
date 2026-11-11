use std::sync::Arc;

use anyhow::{Result, bail};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Event, Placement, PublicKey, Request, Response, Welcome};
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
    /// The identity keys the server event that follows made the key manager trust, oldest
    /// first. They are told apart from the room as it stands once that event is handled,
    /// because a later event may have replaced them by then.
    Trusted(Vec<(UserId, PublicKey)>),
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
    shows_people: bool,
}

impl Call {
    /// Starts a call that tells the key manager itself where people are, as their
    /// presences arrive. For a client that shows nobody.
    pub fn start(link: ControlLink, keystore: &Keystore) -> Result<Self> {
        Self::start_after(link, keystore, None, false)
    }

    /// Starts a call for a client that shows who is in a room. The key manager then learns
    /// where someone is from whoever reads `events`, through the engine's `set_presence`
    /// and `end_presence`, once that reader shows them there. A reader that is behind
    /// shows nobody new, so nobody gets our key before they are shown.
    pub fn start_shown(link: ControlLink, keystore: &Keystore) -> Result<Self> {
        Self::start_after(link, keystore, None, true)
    }

    /// Starts the call that replaces `previous` after a reconnect, keeping the epoch floors
    /// it raised even where they never reached the file.
    pub fn restart(link: ControlLink, keystore: &Keystore, previous: &Call) -> Result<Self> {
        Self::start_after(link, keystore, Some(previous), previous.shows_people)
    }

    fn start_after(
        link: ControlLink,
        keystore: &Keystore,
        previous: Option<&Call>,
        shows_people: bool,
    ) -> Result<Self> {
        let ControlLink {
            welcome,
            connected,
            requester,
            events: mut server_events,
        } = link;
        let connection = connected.connection.clone();
        let encryption = keystore.encryption_key_pair()?;
        let floors = match previous {
            Some(previous) => previous.floors.clone(),
            None => EpochFloors::load(keystore.dir(), &connected.fingerprint)?,
        };
        let (engine, mut engine_requests, recovery) = MediaEngine::start(
            connection.clone(),
            welcome.me,
            encryption,
            connected.fingerprint,
            floors.clone(),
        );
        let engine = Arc::new(engine);

        for p in &welcome.presence {
            engine.set_presence(p.user, p.room);
        }
        let placement_notices = match welcome.placement.room {
            Some(_) => engine.apply_placement(&welcome.placement),
            None => Vec::new(),
        };

        let (events_tx, events) = mpsc::channel(EVENTS);
        for notice in placement_notices {
            events_tx.try_send(CallEvent::Key(notice)).ok();
        }
        let pump_engine = engine.clone();
        tokio::spawn(async move {
            while let Some(event) = server_events.recv().await {
                let notices = match &event {
                    Event::PresenceChanged(_) | Event::PresenceEnded(_) if shows_people => {
                        Vec::new()
                    }
                    Event::PresenceChanged(p) => pump_engine.set_presence(p.user, p.room),
                    Event::PresenceEnded(user) => pump_engine.end_presence(*user),
                    _ => pump_engine.event(&event),
                };
                let trusted = pump_engine.keys().take_newly_trusted();
                if !trusted.is_empty() {
                    events_tx.send(CallEvent::Trusted(trusted)).await.ok();
                }
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
            shows_people,
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
            Response::Placement(placement) => {
                let notices = self.engine.apply_placement(&placement);
                Ok((placement, notices))
            }
            other => bail!("{}", control::refusal(other)),
        }
    }
}
