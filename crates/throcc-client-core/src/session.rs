use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use throcc_proto::Fingerprint;
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{
    Event as ServerEvent, MediaState, Placed, Presence, Request, Response, RoomPeer, ShareConfig,
    Welcome,
};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::account;
use crate::call::{Call, CallEvent};
use crate::client::{Command, Event};
use crate::keys::KeyNotice;
use crate::keystore::Keystore;
use crate::media::audio::VoiceSettings;
use crate::media::engine::Kind;
use crate::media::voice::{VoiceCommand, VoiceEngine};
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, Person, RoomView, ServerView, State,
    TrackHealth,
};

const LEVELS_EVERY: Duration = Duration::from_millis(100);
const HEALTH_EVERY: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
const MIC_KBPS: u32 = 32;
const SHARE_AUDIO_KBPS: u32 = 128;

#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    pub voice: VoiceSettings,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Opens audio devices. Off for headless tests.
    pub audio: bool,
}

/// What the client and its session task share.
pub(crate) struct Shared {
    pub state: Mutex<Arc<State>>,
    pub events: broadcast::Sender<Event>,
}

impl Shared {
    pub fn state(&self) -> Arc<State> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = (**guard).clone();
        change(&mut state);
        if state != **guard {
            *guard = Arc::new(state);
            self.events.send(Event::State(guard.clone())).ok();
        }
    }

    fn emit(&self, event: Event) {
        self.events.send(event).ok();
    }
}

/// One subscribed remote track, with counters from the last health refresh.
struct Watched {
    user: UserId,
    label: &'static str,
    set_kbps: u32,
    last_bytes: u64,
    last_received: u64,
    last_lost: u64,
}

pub(crate) struct Session {
    shared: Arc<Shared>,
    keystore: Keystore,
    address: String,
    options: ClientOptions,
    call: Call,
    voice: Option<VoiceEngine>,
    room: Option<RoomId>,
    my_tracks: Option<throcc_proto::messages::Tracks>,
    media: MediaState,
    peers: BTreeMap<UserId, RoomPeer>,
    last_room_state: Option<(RoomId, throcc_proto::ids::Epoch, Vec<RoomPeer>)>,
    watching: Vec<UserId>,
    tracks: HashMap<MediaId, Watched>,
    dropped_for_you: u32,
    last_sent: u64,
    last_lost_sent: u64,
    last_local_drops: u64,
}

impl Session {
    pub fn new(
        shared: Arc<Shared>,
        keystore: Keystore,
        address: String,
        options: ClientOptions,
        call: Call,
    ) -> Self {
        let mut session = Self {
            shared,
            keystore,
            address,
            options,
            voice: None,
            room: None,
            my_tracks: None,
            media: MediaState::default(),
            peers: BTreeMap::new(),
            last_room_state: None,
            watching: Vec::new(),
            tracks: HashMap::new(),
            dropped_for_you: 0,
            last_sent: 0,
            last_lost_sent: 0,
            last_local_drops: 0,
            call,
        };
        session.start_voice();
        let welcome = session.call.welcome.clone();
        session.apply_welcome(&welcome);
        session
    }

    fn start_voice(&mut self) {
        self.voice = None;
        if !self.options.audio {
            return;
        }
        match VoiceEngine::start(
            self.call.engine.clone(),
            self.options.voice,
            self.options.input_device.as_deref(),
            self.options.output_device.as_deref(),
        ) {
            Ok(voice) => self.voice = Some(voice),
            Err(e) => {
                warn!("no audio: {e:#}");
                self.shared
                    .emit(Event::Notice(format!("Audio is unavailable: {e}")));
            }
        }
    }

    fn voice(&self, command: VoiceCommand) {
        if let Some(voice) = &self.voice {
            voice.command(command);
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut levels = tokio::time::interval(LEVELS_EVERY);
        let mut health = tokio::time::interval(HEALTH_EVERY);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Disconnect) | None => {
                        self.call.close().await;
                        self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                        return;
                    }
                    Some(command) => self.command(command).await,
                },
                event = self.call.events.recv() => match event {
                    Some(event) => self.event(event).await,
                    None => {
                        if !self.reconnect(&mut commands).await {
                            return;
                        }
                    }
                },
                _ = levels.tick() => self.refresh_levels(),
                _ = health.tick() => self.refresh_health(),
                Some(_) = self.call.recovery.recv() => {}
            }
        }
    }

    /// Reconnects with backoff, back into the same room. Returns false if told to stop.
    async fn reconnect(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
        let reason = self
            .call
            .connection
            .close_reason()
            .map_or("connection lost".to_owned(), |e| e.to_string());
        let mut backoff = Duration::from_secs(1);
        for attempt in 1.. {
            self.shared.update(|s| {
                s.connection = ConnectionStatus::Reconnecting {
                    attempt,
                    reason: reason.clone(),
                }
            });
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                command = commands.recv() => {
                    if matches!(command, None | Some(Command::Disconnect)) {
                        self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                        return false;
                    }
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            let link = match account::sign_in(&self.keystore, &self.address, self.room).await {
                Ok(link) => link,
                Err(e) => {
                    if let Some((expected, actual)) = e.pin_mismatch() {
                        let failed = format!(
                            "The server's key changed from {} to {}",
                            expected.short(),
                            actual.short()
                        );
                        self.shared
                            .update(|s| s.connection = ConnectionStatus::Failed(failed));
                        return false;
                    }
                    info!("reconnect attempt {attempt} failed: {e}");
                    continue;
                }
            };
            match Call::start(link, &self.keystore) {
                Ok(call) => {
                    self.call = call;
                    self.reset_room();
                    self.start_voice();
                    let welcome = self.call.welcome.clone();
                    self.apply_welcome(&welcome);
                    return true;
                }
                Err(e) => warn!("reconnect: {e:#}"),
            }
        }
        false
    }

    fn reset_room(&mut self) {
        self.room = None;
        self.my_tracks = None;
        self.peers.clear();
        self.tracks.clear();
        self.watching.clear();
        self.last_room_state = None;
    }

    fn apply_welcome(&mut self, welcome: &Welcome) {
        let fingerprint = self
            .keystore
            .known_servers
            .get(&self.address)
            .unwrap_or(Fingerprint([0; 32]));
        let address = self.address.clone();
        let welcome_copy = welcome.clone();
        self.shared.update(move |s| {
            s.server = Some(ServerView {
                address,
                fingerprint,
            });
            s.connection = ConnectionStatus::Connected;
            s.me = Some(welcome_copy.me);
            s.role = Some(welcome_copy.role);
            s.rooms = welcome_copy
                .rooms
                .iter()
                .map(|r| RoomView {
                    id: r.id,
                    name: r.name.clone(),
                })
                .collect();
            let volumes: HashMap<UserId, f32> =
                s.people.iter().map(|(u, p)| (*u, p.volume)).collect();
            s.people = welcome_copy
                .presence
                .iter()
                .map(|p| {
                    (
                        p.user,
                        person(p, volumes.get(&p.user).copied().unwrap_or(1.0)),
                    )
                })
                .collect();
            s.current = None;
        });
        if welcome.placed.room.is_some() {
            self.after_join(&welcome.placed.clone());
        }
    }

    fn after_join(&mut self, placed: &Placed) {
        self.reset_room();
        self.room = placed.room;
        self.my_tracks = placed.tracks.clone();
        self.media = MediaState::default();
        self.voice(VoiceCommand::ForgetAll);
        self.voice(VoiceCommand::Muted(true));
        self.voice(VoiceCommand::Deafened(false));
        self.voice(VoiceCommand::SendOn(placed.tracks.as_ref().map(|t| t.mic)));
        self.sync_peers(&placed.peers);
        let room = placed.room;
        self.shared.update(|s| {
            s.current = room.map(|room| CurrentRoom {
                room,
                mic: false,
                deafened: false,
                sharing: None,
                watching: vec![],
            });
        });
    }

    /// Follows the room's peers: hears every mic, notes shares, drops who left.
    fn sync_peers(&mut self, peers: &[RoomPeer]) {
        let me = self.call.me();
        let current: BTreeMap<UserId, RoomPeer> = peers
            .iter()
            .filter(|p| p.user != me)
            .map(|p| (p.user, p.clone()))
            .collect();
        for (user, old) in &self.peers {
            if current
                .get(user)
                .is_none_or(|p| p.tracks.mic != old.tracks.mic)
            {
                self.call.engine.unsubscribe(old.tracks.mic);
                self.tracks.remove(&old.tracks.mic);
                self.voice(VoiceCommand::Forget(*user));
            }
        }
        for (user, peer) in &current {
            if self
                .peers
                .get(user)
                .is_none_or(|old| old.tracks.mic != peer.tracks.mic)
            {
                let units = self
                    .call
                    .engine
                    .subscribe(peer.tracks.mic, Kind::Audio, 256);
                self.voice(VoiceCommand::Listen { user: *user, units });
                self.tracks
                    .insert(peer.tracks.mic, watched(*user, "mic", MIC_KBPS));
            }
        }
        self.peers = current;
        let shares: Vec<(UserId, Option<ShareConfig>)> =
            self.peers.values().map(|p| (p.user, p.share)).collect();
        self.shared.update(|s| {
            for (user, share) in shares {
                if let Some(person) = s.people.get_mut(&user) {
                    person.sharing = share;
                }
            }
        });
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Join(room) => match self.call.join(room).await {
                Ok((placed, notices)) => {
                    self.after_join(&placed);
                    self.notices(notices);
                }
                Err(e) => self
                    .shared
                    .emit(Event::Notice(format!("Couldn't join: {e}"))),
            },
            Command::SetMic(on) => {
                self.media.mic = on;
                if on {
                    self.media.deafened = false;
                }
                self.push_media().await;
            }
            Command::SetDeafened(deafened) => {
                self.media.deafened = deafened;
                if deafened {
                    self.media.mic = false;
                }
                self.push_media().await;
            }
            Command::StartShare(config) => {
                self.media.share = Some(config);
                self.push_media().await;
            }
            Command::StopShare => {
                self.media.share = None;
                self.push_media().await;
            }
            Command::Watch(user) => {
                if !self.watching.contains(&user) {
                    self.watching.push(user);
                }
                self.push_watching().await;
            }
            Command::Unwatch(user) => {
                self.watching.retain(|u| *u != user);
                self.push_watching().await;
            }
            Command::SetVolume(user, volume) => {
                self.voice(VoiceCommand::Volume(user, volume));
                self.shared.update(|s| {
                    if let Some(p) = s.people.get_mut(&user) {
                        p.volume = volume;
                    }
                });
            }
            Command::SetName(name) => {
                self.simple(Request::SetProfile { name, avatar: None })
                    .await
            }
            Command::CreateRoom(name) => self.simple(Request::CreateRoom { name }).await,
            Command::RenameRoom(room, name) => {
                self.simple(Request::RenameRoom { room, name }).await
            }
            Command::DeleteRoom(room) => self.simple(Request::DeleteRoom(room)).await,
            Command::CreateInvite => match self.call.request(Request::CreateInvite).await {
                Ok(Response::Invite { code, expires }) => {
                    self.shared.emit(Event::Invite { code, expires })
                }
                other => self.refused(other),
            },
            Command::ListUsers => match self.call.request(Request::ListUsers).await {
                Ok(Response::Users(users)) => self.shared.emit(Event::Users(users)),
                other => self.refused(other),
            },
            Command::SetRole(user, role) => {
                self.simple(Request::SetRole { user, role }).await;
                self.list_users().await;
            }
            Command::RemoveUser { user, cascade } => {
                self.simple(Request::RemoveUser { user, cascade }).await;
                self.list_users().await;
            }
            Command::AcceptKey(user) => self.accept_key(user),
            Command::SetVerified(user, verified) => {
                if let Err(e) = self
                    .call
                    .engine
                    .keys()
                    .contacts
                    .set_verified(user, verified)
                {
                    warn!("saving verification: {e:#}");
                }
                self.shared.update(|s| {
                    if let Some(p) = s.people.get_mut(&user) {
                        p.verified = verified;
                    }
                });
            }
            Command::Disconnect => {}
        }
    }

    async fn list_users(&mut self) {
        if let Ok(Response::Users(users)) = self.call.request(Request::ListUsers).await {
            self.shared.emit(Event::Users(users));
        }
    }

    async fn simple(&mut self, request: Request) {
        match self.call.request(request).await {
            Ok(Response::Ok) => {}
            other => self.refused(other),
        }
    }

    fn refused(&self, response: anyhow::Result<Response>) {
        let message = match response {
            Ok(Response::Err { message, .. }) => message,
            Ok(other) => format!("unexpected reply {other:?}"),
            Err(e) => e.to_string(),
        };
        self.shared.emit(Event::Notice(message));
    }

    async fn push_media(&mut self) {
        if self.room.is_none() {
            return;
        }
        self.voice(VoiceCommand::Muted(!self.media.mic));
        self.voice(VoiceCommand::Deafened(self.media.deafened));
        let media = self.media;
        self.shared.update(|s| {
            if let Some(c) = s.current.as_mut() {
                c.mic = media.mic;
                c.deafened = media.deafened;
                c.sharing = media.share;
            }
        });
        self.simple(Request::SetMedia(media)).await;
    }

    async fn push_watching(&mut self) {
        let mut ids = Vec::new();
        for user in &self.watching {
            if let Some(share) = self.peers.get(user).and_then(|p| p.tracks.shares.first()) {
                ids.push(share.video);
                ids.push(share.audio);
            }
        }
        let watching = self.watching.clone();
        self.shared.update(|s| {
            if let Some(c) = s.current.as_mut() {
                c.watching = watching;
            }
        });
        self.simple(Request::SetWatching(ids)).await;
    }

    fn accept_key(&mut self, user: UserId) {
        let Some((room, epoch, peers)) = self.last_room_state.clone() else {
            return;
        };
        let Some(peer) = peers.iter().find(|p| p.user == user) else {
            return;
        };
        if let Err(e) = self
            .call
            .engine
            .keys()
            .contacts
            .pin(user, peer.identity_key)
        {
            warn!("saving the new key: {e:#}");
        }
        self.shared
            .update(|s| s.key_warnings.retain(|w| w.user != user));
        let notices = self
            .call
            .engine
            .event(&ServerEvent::RoomState { room, epoch, peers });
        self.notices(notices);
    }

    fn notices(&self, notices: Vec<KeyNotice>) {
        for notice in notices {
            match notice {
                KeyNotice::KeyChanged { user, pinned, now } => self.shared.update(|s| {
                    if !s.key_warnings.iter().any(|w| w.user == user) {
                        s.key_warnings.push(KeyWarning {
                            user,
                            pinned: Fingerprint(pinned),
                            now: Fingerprint(now),
                        });
                    }
                }),
                KeyNotice::BadEncKey { user } => {
                    self.shared.emit(Event::Notice(format!(
                        "User {}'s encryption key failed its check; they can't hear you.",
                        user.0
                    )));
                }
                KeyNotice::NewContact { .. } => {}
            }
        }
    }

    async fn event(&mut self, event: CallEvent) {
        match event {
            CallEvent::Key(notice) => self.notices(vec![notice]),
            CallEvent::Server(event) => self.server_event(event).await,
        }
    }

    async fn server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::PresenceChanged(p) => self.shared.update(|s| {
                let volume = s.people.get(&p.user).map_or(1.0, |old| old.volume);
                s.people.insert(p.user, person(&p, volume));
            }),
            ServerEvent::PresenceEnded(user) => self.shared.update(|s| {
                s.people.remove(&user);
            }),
            ServerEvent::RoomCreated(room) => self.shared.update(|s| {
                s.rooms.push(RoomView {
                    id: room.id,
                    name: room.name,
                });
            }),
            ServerEvent::RoomRenamed { room, name } => self.shared.update(|s| {
                if let Some(r) = s.rooms.iter_mut().find(|r| r.id == room) {
                    r.name = name;
                }
            }),
            ServerEvent::RoomDeleted(room) => {
                if self.room == Some(room) {
                    self.call.engine.placed(&Placed {
                        room: None,
                        epoch: throcc_proto::ids::Epoch(0),
                        tracks: None,
                        peers: vec![],
                    });
                    self.reset_room();
                    self.voice(VoiceCommand::ForgetAll);
                    self.voice(VoiceCommand::SendOn(None));
                    self.shared
                        .emit(Event::Notice("The room you were in was deleted.".into()));
                }
                self.shared.update(|s| {
                    s.rooms.retain(|r| r.id != room);
                    if s.current.as_ref().is_some_and(|c| c.room == room) {
                        s.current = None;
                    }
                });
            }
            ServerEvent::RoomState { room, epoch, peers } => {
                if Some(room) == self.room {
                    self.sync_peers(&peers);
                    self.last_room_state = Some((room, epoch, peers));
                    if !self.watching.is_empty() {
                        self.push_watching().await;
                    }
                }
            }
            ServerEvent::LinkStats { tracks } => {
                self.dropped_for_you += tracks.iter().map(|t| t.dropped_units).sum::<u32>();
            }
            ServerEvent::RoleChanged { user, role } => {
                if user == self.call.me() {
                    self.shared.update(|s| s.role = Some(role));
                }
            }
            ServerEvent::InviteBurned { code_id } => self.shared.emit(Event::Notice(format!(
                "Invite {code_id}-… was used up by three wrong tries and no longer works."
            ))),
            ServerEvent::Kicked { reason } => self.shared.emit(Event::Kicked(reason)),
            ServerEvent::UserAdded(_) | ServerEvent::UserRemoved(_) => self.list_users().await,
            ServerEvent::SenderKey { .. } | ServerEvent::KeyframeWanted { .. } => {}
        }
    }

    fn refresh_levels(&mut self) {
        let Some(voice) = &self.voice else { return };
        let levels = voice.levels();
        let me = self.call.me();
        let mine = levels.transmitting;
        self.shared.update(|s| {
            for (user, person) in s.people.iter_mut() {
                person.speaking = if *user == me {
                    mine
                } else {
                    levels.speaking.get(user).copied().unwrap_or(false)
                };
            }
        });
    }

    fn refresh_health(&mut self) {
        let connection = &self.call.connection;
        let stats = connection.stats();
        let sent = stats.path.sent_packets;
        let lost = stats.path.lost_packets;
        let upload_loss = percent(
            lost - self.last_lost_sent.min(lost),
            sent - self.last_sent.min(sent),
        );
        self.last_sent = sent;
        self.last_lost_sent = lost;
        let engine_stats = &self.call.engine.stats;
        let local_drops = engine_stats.send_dropped_units.load(Ordering::Relaxed);
        let new_local_drops = local_drops - self.last_local_drops.min(local_drops);
        self.last_local_drops = local_drops;

        let state = self.shared.state();
        let mut rows = Vec::new();
        let (mut received_total, mut lost_total) = (0, 0);
        for (media_id, track) in self.tracks.iter_mut() {
            let bytes = self.call.engine.received_bytes(*media_id);
            let (received, lost) = self.call.engine.datagrams(*media_id);
            let new_received = received - track.last_received.min(received);
            let new_lost = lost - track.last_lost.min(lost);
            let kbps =
                ((bytes - track.last_bytes.min(bytes)) * 8 / HEALTH_EVERY.as_secs() / 1000) as u32;
            track.last_bytes = bytes;
            track.last_received = received;
            track.last_lost = lost;
            received_total += new_received;
            lost_total += new_lost;
            let name = state
                .person(track.user)
                .map_or("someone".to_owned(), |p| p.name.clone());
            rows.push(TrackHealth {
                label: format!("{name}, {}", track.label),
                set_kbps: Some(track.set_kbps),
                arriving_kbps: (new_received > 0).then_some(kbps),
                loss: (new_received + new_lost > 0)
                    .then(|| percent(new_lost, new_received + new_lost)),
            });
        }
        rows.sort_by(|a, b| a.label.cmp(&b.label));
        let download_loss = percent(lost_total, received_total + lost_total);
        let dropped = std::mem::take(&mut self.dropped_for_you);
        let health = Health {
            rtt_ms: connection.rtt().as_secs_f32() * 1000.0,
            upload_loss,
            download_loss,
            dropped_for_you: dropped,
            verdict: verdict(&rows, dropped, new_local_drops, download_loss),
            sending: sending(&self.media, new_local_drops),
            receiving: rows,
        };
        self.shared.update(|s| s.health = health);
    }
}

fn watched(user: UserId, label: &'static str, set_kbps: u32) -> Watched {
    Watched {
        user,
        label,
        set_kbps,
        last_bytes: 0,
        last_received: 0,
        last_lost: 0,
    }
}

fn person(p: &Presence, volume: f32) -> Person {
    Person {
        name: p.name.clone(),
        avatar: p.avatar,
        room: p.room,
        mic: p.mic,
        deafened: p.deafened,
        sharing: None,
        speaking: false,
        volume,
        verified: false,
    }
}

fn percent(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 * 100.0 / whole as f32
    }
}

/// One plain-language sentence on what limits the call right now (PLAN §5.11).
fn verdict(
    rows: &[TrackHealth],
    dropped_for_you: u32,
    local_drops: u64,
    download_loss: f32,
) -> String {
    if dropped_for_you > 0 {
        return format!(
            "{dropped_for_you} frames were dropped for you in the last 2 s. Your download can't carry everything you watch."
        );
    }
    if local_drops > 0 {
        return "Your upload can't carry your share's bitrate, so frames are dropped before they leave.".into();
    }
    let slow = rows.iter().find(|r| match (r.set_kbps, r.arriving_kbps) {
        (Some(set), Some(arriving)) => set >= 1000 && arriving * 10 < set * 8,
        _ => false,
    });
    if let Some(row) = slow {
        let mbps = |k: u32| f64::from(k) / 1000.0;
        return format!(
            "{} arrives at {:.1} of the {:.0} Mbps set. Nothing was dropped for you, so the sender's upload is the bottleneck.",
            row.label,
            mbps(row.arriving_kbps.unwrap_or(0)),
            mbps(row.set_kbps.unwrap_or(0))
        );
    }
    if download_loss > 2.0 {
        return format!("{download_loss:.1}% of packets are lost between the server and you.");
    }
    "Everything is arriving.".into()
}

fn sending(media: &MediaState, local_drops: u64) -> String {
    let mut parts = Vec::new();
    if media.mic {
        parts.push(format!("mic {MIC_KBPS} kbps"));
    }
    if let Some(share) = media.share {
        parts.push(format!("screen {} kbps", share.kbps));
        if share.audio {
            parts.push(format!("screen audio {SHARE_AUDIO_KBPS} kbps"));
        }
    }
    if parts.is_empty() {
        return "nothing".into();
    }
    format!("{}, {local_drops} dropped", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_names_the_bottleneck() {
        let row = |set, arriving| TrackHealth {
            label: "Ilse, screen".into(),
            set_kbps: Some(set),
            arriving_kbps: Some(arriving),
            loss: Some(0.0),
        };
        assert!(
            verdict(&[row(10_000, 6_100)], 0, 0, 0.0).contains("arrives at 6.1 of the 10 Mbps")
        );
        assert!(verdict(&[row(10_000, 6_100)], 3, 0, 0.0).contains("dropped for you"));
        assert_eq!(
            verdict(&[row(10_000, 9_900)], 0, 0, 0.0),
            "Everything is arriving."
        );
        assert!(verdict(&[], 0, 0, 4.0).contains("4.0% of packets"));
    }
}
