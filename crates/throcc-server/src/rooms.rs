use std::collections::HashMap;

use quinn::Connection;
use throcc_proto::{Event, ServerMessage, UserId};
use tokio::sync::mpsc;

/// A client this far behind on control events has a divergent roster, so its
/// connection is closed rather than left to carry on with one.
const BACKED_UP: u32 = 2;

#[derive(Default)]
pub struct Registry {
    sessions: HashMap<UserId, Session>,
}

struct Session {
    outbound: mpsc::Sender<ServerMessage>,
    connection: Connection,
}

impl Registry {
    /// The connection this one displaces, when the same key was already
    /// connected. One key is one session, and the newer one wins.
    pub fn attach(
        &mut self,
        user: UserId,
        outbound: mpsc::Sender<ServerMessage>,
        connection: Connection,
    ) -> Option<Connection> {
        let session = Session {
            outbound,
            connection,
        };
        self.sessions
            .insert(user, session)
            .map(|displaced| displaced.connection)
    }

    pub fn detach(&mut self, user: UserId) {
        self.sessions.remove(&user);
    }

    pub fn broadcast(&mut self, event: Event) {
        let recipients: Vec<UserId> = self.sessions.keys().copied().collect();
        for user in recipients {
            self.send(user, event.clone());
        }
    }

    fn send(&mut self, user: UserId, event: Event) {
        let Some(session) = self.sessions.get(&user) else {
            return;
        };
        if session
            .outbound
            .try_send(ServerMessage::Event(event))
            .is_err()
        {
            self.close_backed_up(user);
        }
    }

    fn close_backed_up(&mut self, user: UserId) {
        if let Some(session) = self.sessions.remove(&user) {
            tracing::warn!(%user, "closing a connection that stopped reading its events");
            session
                .connection
                .close(BACKED_UP.into(), b"control events backed up");
        }
    }
}
