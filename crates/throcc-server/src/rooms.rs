use std::collections::HashMap;

use anyhow::{Context, Result};
use quinn::Connection;
use throcc_proto::{Codec, Epoch, Event, PeerState, Placed, RoomId, ServerMessage, Tracks, UserId};
use tokio::sync::mpsc;

use crate::database::Database;

/// A client this far behind on control events has a divergent roster, so its
/// connection is closed rather than left to carry on with one.
const BACKED_UP: u32 = 2;

#[derive(Default)]
pub struct Registry {
    sessions: HashMap<UserId, Session>,
}

pub enum Placement {
    Placed(Placed),
    NoSuchRoom(RoomId),
}

struct Session {
    outbound: mpsc::Sender<ServerMessage>,
    connection: Connection,
    room: Option<RoomId>,
    tracks: Option<Tracks>,
    media: Media,
}

struct Media {
    mic: bool,
    screen: bool,
    share_audio: bool,
    screen_kbps: u32,
    codec: Codec,
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
            room: None,
            tracks: None,
            media: Media::default(),
        };
        self.sessions
            .insert(user, session)
            .map(|displaced| displaced.connection)
    }

    pub fn detach(&mut self, database: &Database, user: UserId) -> Result<()> {
        let Some(session) = self.sessions.remove(&user) else {
            return Ok(());
        };
        let Some(room) = session.room else {
            return Ok(());
        };

        let transition = database
            .transition(Some(room), None)?
            .context("only the room being entered can be missing")?;
        self.announce_exit(room, transition.leaving, user);
        Ok(())
    }

    /// The one membership transition: both rooms' epochs bump, fresh tracks are
    /// allocated, and the reply is built before anybody is told.
    pub fn set_room(
        &mut self,
        database: &Database,
        user: UserId,
        target: Option<RoomId>,
    ) -> Result<Placement> {
        let leaving = self
            .sessions
            .get(&user)
            .context("a request arrived for a session that is not attached")?
            .room;

        let Some(transition) = database.transition(leaving, target)? else {
            return Ok(Placement::NoSuchRoom(
                target.expect("only the room being entered can be missing"),
            ));
        };
        let epoch = transition.entering.unwrap_or(Epoch(0));

        let session = self
            .sessions
            .get_mut(&user)
            .expect("the session was there a moment ago");
        session.room = target;
        session.tracks = transition.tracks;

        let placed = Placed {
            room: target,
            epoch,
            tracks: session.tracks.clone(),
            peers: self.peers_in(target, user),
        };

        if let Some(room) = leaving.filter(|room| Some(*room) != target) {
            self.announce_exit(room, transition.leaving, user);
        }
        if let (Some(room), Some(peer)) = (target, self.peer_state(user)) {
            self.send_to_room(room, Event::UserEntered { room, epoch, peer }, Some(user));
        }

        Ok(Placement::Placed(placed))
    }

    /// The room asked for on auth, or no room when that room no longer exists.
    /// Substituting a different one would put somebody somewhere they never asked
    /// to be.
    pub fn place(
        &mut self,
        database: &Database,
        user: UserId,
        want_room: Option<RoomId>,
    ) -> Result<Placed> {
        match self.set_room(database, user, want_room)? {
            Placement::Placed(placed) => Ok(placed),
            Placement::NoSuchRoom(room) => {
                tracing::info!(%user, %room, "the room asked for is gone; placing in no room");
                match self.set_room(database, user, None)? {
                    Placement::Placed(placed) => Ok(placed),
                    Placement::NoSuchRoom(room) => {
                        unreachable!("no room cannot be missing, unlike {room}")
                    }
                }
            }
        }
    }

    /// The occupants of a deleted room are moved to no room. The room is gone,
    /// so `RoomDeleted` is the event that carries the news.
    pub fn clear_room(&mut self, room: RoomId) {
        for session in self.sessions.values_mut() {
            if session.room == Some(room) {
                session.room = None;
                session.tracks = None;
            }
        }
    }

    pub fn broadcast(&mut self, event: Event) {
        let recipients: Vec<UserId> = self.sessions.keys().copied().collect();
        for user in recipients {
            self.send(user, event.clone());
        }
    }

    fn announce_exit(&mut self, room: RoomId, epoch: Option<Epoch>, user: UserId) {
        let epoch = epoch.unwrap_or(Epoch(0));
        self.send_to_room(room, Event::UserExited { room, epoch, user }, None);
    }

    fn send_to_room(&mut self, room: RoomId, event: Event, except: Option<UserId>) {
        let recipients: Vec<UserId> = self
            .sessions
            .iter()
            .filter(|(user, session)| session.room == Some(room) && Some(**user) != except)
            .map(|(user, _)| *user)
            .collect();
        for user in recipients {
            self.send(user, event.clone());
        }
    }

    fn peers_in(&self, room: Option<RoomId>, except: UserId) -> Vec<PeerState> {
        let Some(room) = room else {
            return Vec::new();
        };
        self.sessions
            .iter()
            .filter(|(user, session)| session.room == Some(room) && **user != except)
            .filter_map(|(user, _)| self.peer_state(*user))
            .collect()
    }

    fn peer_state(&self, user: UserId) -> Option<PeerState> {
        let session = self.sessions.get(&user)?;
        Some(PeerState {
            user,
            tracks: session.tracks.clone()?,
            mic: session.media.mic,
            screen: session.media.screen,
            share_audio: session.media.share_audio,
            screen_kbps: session.media.screen_kbps,
            codec: session.media.codec,
        })
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

impl Default for Media {
    fn default() -> Self {
        Self {
            mic: false,
            screen: false,
            share_audio: false,
            screen_kbps: 0,
            codec: Codec::H264,
        }
    }
}
