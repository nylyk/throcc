use std::sync::Arc;

use anyhow::{Result, bail};
use throcc_crypto::hpke::EncKeyPair;
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Event, Placed, Request, Response, Welcome};
use tokio::sync::mpsc;
use tracing::warn;

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
        let (engine, mut engine_requests, recovery) =
            MediaEngine::start(connection.clone(), welcome.me, enc, contacts);
        let engine = Arc::new(engine);

        let (events_tx, events) = mpsc::channel(EVENTS);
        let pump_engine = engine.clone();
        tokio::spawn(async move {
            while let Some(event) = server_events.recv().await {
                for notice in pump_engine.event(&event) {
                    events_tx.send(CallEvent::Key(notice)).await.ok();
                }
                events_tx.send(CallEvent::Server(event)).await.ok();
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

        if welcome.placed.room.is_some() {
            engine.placed(&welcome.placed);
        }
        Ok(Self {
            welcome,
            requester,
            engine,
            events,
            recovery,
            connection,
        })
    }

    pub fn me(&self) -> UserId {
        self.welcome.me
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
