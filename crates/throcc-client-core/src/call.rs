use std::sync::Arc;

use anyhow::{Result, bail};
use throcc_crypto::hpke::EncKeyPair;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Event, Placed, Request, Response, Welcome};
use tokio::sync::mpsc;
use tracing::warn;

use crate::connection::Connected;
use crate::contacts::Contacts;
use crate::control::{Link, Requester};
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
    connected: Connected,
}

impl Call {
    pub fn start(link: Link, keystore: &Keystore) -> Result<Self> {
        let Link {
            welcome,
            connected,
            requester,
            events: mut server_events,
        } = link;
        let connection = connected.connection.clone();
        let enc = EncKeyPair::from_secret(&keystore.enc_key().secret())?;
        let contacts = Contacts::load(keystore.dir(), &connected.fingerprint)?;
        let (engine, mut engine_requests, recovery) = MediaEngine::start(
            connection.clone(),
            welcome.me,
            enc,
            connected.fingerprint,
            contacts,
        );
        let engine = Arc::new(engine);

        {
            let mut keys = engine.keys();
            for p in &welcome.presence {
                keys.keys.set_presence(p.user, p.room);
            }
        }
        let placed_notices = match welcome.placed.room {
            Some(_) => engine.placed(&welcome.placed),
            None => Vec::new(),
        };

        let (events_tx, events) = mpsc::channel(EVENTS);
        for notice in placed_notices {
            events_tx.try_send(CallEvent::Key(notice)).ok();
        }
        let pump_engine = engine.clone();
        tokio::spawn(async move {
            while let Some(event) = server_events.recv().await {
                let mut notices = pump_engine.event(&event);
                notices.extend(follow_presence(&pump_engine, &event));
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
            connected,
        })
    }

    /// Leaves the server cleanly.
    pub async fn close(&self) {
        self.connected.close().await;
    }

    pub fn me(&self) -> UserId {
        self.welcome.me
    }

    pub fn server(&self) -> Fingerprint {
        self.connected.fingerprint
    }

    /// Applies the current room state again, after a pin changed.
    pub fn refresh_keys(&self) -> Vec<KeyNotice> {
        refresh_keys(&self.engine)
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        Ok(self.requester.request(request).await?)
    }

    /// Moves to `room` and rekeys. Returns the placement and any key warnings.
    pub async fn join(&self, room: Option<RoomId>) -> Result<(Placed, Vec<KeyNotice>)> {
        match self.request(Request::SetRoom(room)).await? {
            Response::Placed(placed) => {
                let notices = self.engine.placed(&placed);
                Ok((placed, notices))
            }
            Response::Err { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        }
    }
}

/// Tells the key manager where `event` shows a user, and applies the room state again
/// when that changes who is shown in our room.
fn follow_presence(engine: &MediaEngine, event: &Event) -> Vec<KeyNotice> {
    let (user, room) = match event {
        Event::PresenceChanged(p) => (p.user, p.room),
        Event::PresenceEnded(user) => (*user, None),
        _ => return Vec::new(),
    };
    if engine.keys().keys.set_presence(user, room) {
        refresh_keys(engine)
    } else {
        Vec::new()
    }
}

fn refresh_keys(engine: &MediaEngine) -> Vec<KeyNotice> {
    let current = engine.keys().keys.current_room_state();
    match current {
        Some((room, epoch, peers)) => engine.event(&Event::RoomState { room, epoch, peers }),
        None => Vec::new(),
    }
}
