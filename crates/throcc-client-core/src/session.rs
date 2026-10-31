use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use throcc_crypto::hpke::EncKeyPair;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, MediaId, RoomId, UserId};
use throcc_proto::messages::{
    AuthError, Event as ServerEvent, MediaState, Placed, Presence, PublicKey, Recovery, Request,
    Response, RoomPeer, Share, ShareConfig, Welcome,
};
use throcc_proto::transport::close;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::account::{self, SignInError};
use crate::call::{Call, CallEvent};
use crate::client::{Command, Event};
use crate::control::LoginError;
use crate::keys::KeyNotice;
use crate::keystore::Keystore;
use crate::lock;
use crate::media::audio::VoiceSettings;
use crate::media::engine::Kind;
use crate::media::share_audio::Playout;
use crate::media::video::Picture;
use crate::media::voice::{VoiceCommand, VoiceEngine};
use crate::profile::Profile;
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, Person, RoomView, SentTrack, ServerView,
    State, TrackHealth,
};

const LEVELS_EVERY: Duration = Duration::from_millis(100);
const HEALTH_EVERY: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
const RENEWAL_CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
const MIC_KBPS: u32 = 64;
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
    pub pictures: Mutex<HashMap<UserId, Picture>>,
    pub avatars: Mutex<HashMap<Hash, AvatarSlot>>,
    /// The processed mic level in dBFS while audio runs.
    pub mic_level: Mutex<Option<f32>>,
}

/// A profile picture fetched once per hash and kept for the session.
#[derive(Clone)]
pub(crate) enum AvatarSlot {
    Fetching,
    Ready(Arc<[u8]>),
    Missing,
}

impl Shared {
    /// The picture behind `hash` once fetched. True in the second place when nobody has
    /// asked for it yet, so the caller should fetch it.
    pub fn avatar(&self, hash: Hash) -> (Option<Arc<[u8]>>, bool) {
        let mut avatars = lock(&self.avatars);
        match avatars.get(&hash) {
            Some(AvatarSlot::Ready(bytes)) => (Some(bytes.clone()), false),
            Some(_) => (None, false),
            None => {
                avatars.insert(hash, AvatarSlot::Fetching);
                (None, true)
            }
        }
    }

    /// Forgets a picture whose fetch failed, so the next ask fetches it again.
    fn forget_avatar(&self, hash: Hash) {
        lock(&self.avatars).remove(&hash);
    }

    fn set_avatar(&self, hash: Hash, slot: AvatarSlot) {
        let ready = matches!(slot, AvatarSlot::Ready(_));
        lock(&self.avatars).insert(hash, slot);
        if ready {
            self.emit(Event::AvatarReady(hash));
        }
    }

    pub fn picture(&self, user: UserId) -> Option<Picture> {
        lock(&self.pictures).get(&user).cloned()
    }

    fn set_picture(&self, user: UserId, picture: Option<Picture>) {
        let mut pictures = lock(&self.pictures);
        match picture {
            Some(picture) => pictures.insert(user, picture),
            None => pictures.remove(&user),
        };
    }

    pub fn state(&self) -> Arc<State> {
        lock(&self.state).clone()
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut guard = lock(&self.state);
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

    pub fn notice(&self, text: impl Into<String>) {
        self.emit(Event::Notice(text.into()));
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
    /// The peers our key is wrapped to, whose tracks we follow.
    peers: BTreeMap<UserId, RoomPeer>,
    watching: Vec<UserId>,
    /// The video track each watched share is decoded from, and the config it was set up for.
    watched_video: HashMap<UserId, (MediaId, ShareConfig)>,
    watched_audio: HashMap<UserId, MediaId>,
    mic_test: bool,
    /// A picture to put on our profile once the server tells us our name.
    pending_avatar: Option<Option<Hash>>,
    profile: Profile,
    #[cfg(feature = "ffmpeg")]
    sharing: Option<crate::media::share::ShareSender>,
    tracks: HashMap<MediaId, Watched>,
    dropped_for_you: u32,
    last_sent: u64,
    last_lost_sent: u64,
    last_local_drops: u64,
    last_sent_bytes: HashMap<MediaId, u64>,
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
            watching: Vec::new(),
            watched_video: HashMap::new(),
            watched_audio: HashMap::new(),
            mic_test: false,
            pending_avatar: None,
            profile: Profile::default(),
            #[cfg(feature = "ffmpeg")]
            sharing: None,
            tracks: HashMap::new(),
            dropped_for_you: 0,
            last_sent: 0,
            last_lost_sent: 0,
            last_local_drops: 0,
            last_sent_bytes: HashMap::new(),
            call,
        };
        session.profile = Profile::load(session.keystore.dir()).unwrap_or_else(|e| {
            warn!("no saved profile: {e:#}");
            Profile::default()
        });
        session.show_profile();
        session.start_voice();
        let welcome = session.call.welcome.clone();
        session.apply_welcome(&welcome);
        session
    }

    fn show_profile(&self) {
        let picture = self.profile.picture.clone().map(|bytes| {
            let hash = Hash(Sha256::digest(&bytes).into());
            self.shared.set_avatar(hash, AvatarSlot::Ready(bytes));
            hash
        });
        let name = self.profile.name.clone();
        self.shared.update(|s| {
            s.profile_name = name;
            s.profile_picture = picture;
        });
    }

    fn change_profile(&mut self, change: impl FnOnce(&mut Profile)) {
        change(&mut self.profile);
        if let Err(e) = self.profile.save(self.keystore.dir()) {
            warn!("saving the profile: {e:#}");
            self.shared
                .notice(format!("Couldn't keep your profile: {e}"));
        }
        self.show_profile();
    }

    /// Gives this server our profile, unless it has it already.
    async fn push_profile(&mut self) {
        let avatar = match self.profile.picture.clone() {
            Some(bytes) => match self.call.request(Request::SetAvatar(bytes.to_vec())).await {
                Ok(Response::AvatarHash(hash)) => Some(hash),
                other => {
                    self.shared.emit(Event::ProfileRefused(refusal(other)));
                    return;
                }
            },
            None => None,
        };
        let Some(name) = self.profile.name.clone() else {
            self.pending_avatar = Some(avatar);
            self.apply_pending_avatar().await;
            return;
        };
        self.pending_avatar = None;
        let known = self.my_person();
        if known.is_some_and(|me| me.name == name && me.avatar == avatar) {
            self.shared.emit(Event::ProfileSaved);
        } else {
            self.save_profile(name, avatar).await;
        }
    }

    fn start_voice(&mut self) {
        self.voice = None;
        *lock(&self.shared.mic_level) = None;
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
                self.shared.notice(format!("Audio is unavailable: {e}"));
            }
        }
    }

    fn voice(&self, command: VoiceCommand) {
        if let Some(voice) = &self.voice {
            voice.command(command);
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        if self.profile != Profile::default() {
            self.push_profile().await;
        }
        let mut levels = tokio::time::interval(LEVELS_EVERY);
        let mut health = tokio::time::interval(HEALTH_EVERY);
        let mut renewal = tokio::time::interval(RENEWAL_CHECK_EVERY);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Disconnect) | None => {
                        self.call.close().await;
                        self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                        return;
                    }
                    Some(Command::LeaveServer) => {
                        if self.leave_server().await {
                            return;
                        }
                    }
                    Some(command) => self.command(command).await,
                },
                event = self.call.events.recv() => match event {
                    Some(event) => self.event(event).await,
                    None => {
                        let Some(queued) = self.reconnect(&mut commands).await else {
                            return;
                        };
                        for command in queued {
                            if matches!(command, Command::LeaveServer) {
                                if self.leave_server().await {
                                    return;
                                }
                            } else {
                                self.command(command).await;
                            }
                        }
                    }
                },
                _ = levels.tick() => {
                    self.refresh_levels();
                    self.stop_ended_share().await;
                }
                _ = health.tick() => self.refresh_health(),
                _ = renewal.tick() => self.renew_enc_key().await,
                Some(_) = self.call.recovery.recv() => self.recover_share(Recovery::Idr),
            }
        }
    }

    /// Ends our membership and forgets the server. True once the server took us off; the
    /// server closes the connection as it does, which may lose its reply.
    async fn leave_server(&mut self) -> bool {
        let request = Request::RemoveUser {
            user: self.call.me(),
            cascade: false,
        };
        let removed = match self.call.request(request).await {
            Ok(Response::Ok) => true,
            Ok(Response::Err { message, .. }) => {
                self.shared
                    .notice(format!("Couldn't leave this server: {message}"));
                return false;
            }
            Ok(_) | Err(_) => matches!(
                self.call.connection.close_reason(),
                Some(quinn::ConnectionError::ApplicationClosed(c))
                    if c.error_code == quinn::VarInt::from_u32(close::REMOVED)
            ),
        };
        if !removed {
            self.shared
                .notice("Couldn't leave this server: the connection dropped. Try again.");
            return false;
        }
        if let Err(e) = self.keystore.known_servers.forget(&self.address) {
            warn!("forgetting the server: {e:#}");
        }
        self.call.close().await;
        self.shared
            .update(|s| s.connection = ConnectionStatus::Disconnected);
        self.shared.emit(Event::LeftServer);
        true
    }

    /// Reconnects with backoff, back into the same room. Returns the commands that arrived
    /// meanwhile, or `None` if told to stop or the server won't take us back.
    async fn reconnect(&mut self, commands: &mut mpsc::Receiver<Command>) -> Option<Vec<Command>> {
        let reason = self
            .call
            .connection
            .close_reason()
            .map_or("connection lost".to_owned(), |e| e.to_string());
        let mut backoff = Duration::from_secs(1);
        let mut queued = Vec::new();
        for attempt in 1.. {
            self.shared.update(|s| {
                s.connection = ConnectionStatus::Reconnecting {
                    attempt,
                    reason: reason.clone(),
                }
            });
            let wait = tokio::time::sleep(backoff);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    command = commands.recv() => match command {
                        None | Some(Command::Disconnect) => {
                            self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                            return None;
                        }
                        Some(command) => queued.push(command),
                    },
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            if let Err(e) = self.keystore.renew_enc_key_if_due() {
                warn!("renewing the encryption key: {e:#}");
            }
            let link = match account::sign_in(&self.keystore, &self.address, self.room).await {
                Ok(link) => link,
                Err(e) => {
                    if let SignInError::Login(LoginError::Refused(reason)) = &e
                        && *reason != AuthError::RateLimited
                    {
                        let failed = format!("The server refused you: {reason}");
                        self.shared
                            .update(|s| s.connection = ConnectionStatus::Failed(failed));
                        return None;
                    }
                    if let Some((expected, actual)) = e.pin_mismatch() {
                        let failed = format!(
                            "The server's key changed from {} to {}",
                            expected.short(),
                            actual.short()
                        );
                        self.shared
                            .update(|s| s.connection = ConnectionStatus::Failed(failed));
                        return None;
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
                    return Some(queued);
                }
                Err(e) => warn!("reconnect: {e:#}"),
            }
        }
        None
    }

    fn reset_room(&mut self) {
        self.stop_share();
        let watched: Vec<UserId> = self.watched_video.keys().copied().collect();
        for user in watched {
            self.stop_watching(user);
        }
        self.room = None;
        self.my_tracks = None;
        self.peers.clear();
        self.tracks.clear();
        self.watching.clear();
        self.show_shares();
    }

    fn apply_welcome(&mut self, welcome: &Welcome) {
        let fingerprint = self
            .keystore
            .known_servers
            .get(&self.address)
            .unwrap_or(Fingerprint([0; 32]));
        let address = self.address.clone();
        self.shared.update(move |s| {
            s.server = Some(ServerView {
                address,
                fingerprint,
                name: welcome.server.name.clone(),
                avatar: welcome.server.avatar,
            });
            s.connection = ConnectionStatus::Connected;
            s.me = Some(welcome.me);
            s.role = Some(welcome.role);
            s.rooms = welcome
                .rooms
                .iter()
                .map(|r| RoomView {
                    id: r.id,
                    name: r.name.clone(),
                })
                .collect();
            let volumes: HashMap<UserId, f32> =
                s.people.iter().map(|(u, p)| (*u, p.volume)).collect();
            s.people = welcome
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
        self.refresh_verified();
        if welcome.placed.room.is_some() {
            self.after_join(&welcome.placed);
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
        self.sync_peers();
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

    /// Follows the peers our key is wrapped to: hears every mic, notes shares, drops who
    /// left. True when they changed.
    fn sync_peers(&mut self) -> bool {
        let trusted = self.call.engine.keys().keys.trusted_room_peers();
        let current: BTreeMap<UserId, RoomPeer> =
            trusted.into_iter().map(|p| (p.user, p)).collect();
        let changed = current != self.peers;
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
                self.tracks.insert(
                    peer.tracks.mic,
                    self.watched(peer.tracks.mic, *user, "mic", MIC_KBPS),
                );
            }
        }
        self.peers = current;
        self.show_shares();
        changed
    }

    /// Follows a change in who we share keys with, and in the shares we watch from them.
    async fn follow_room(&mut self) {
        if self.sync_peers() && !self.watching.is_empty() {
            self.push_watching().await;
        }
    }

    /// Shows who we marked verified, by the key pinned for them.
    fn refresh_verified(&self) {
        let state = self.shared.state();
        let verified: BTreeMap<UserId, bool> = {
            let keys = self.call.engine.keys();
            state
                .people
                .keys()
                .map(|user| (*user, keys.contacts.get(*user).is_some_and(|c| c.verified)))
                .collect()
        };
        self.shared.update(|s| {
            for (user, person) in &mut s.people {
                person.verified = verified.get(user).copied().unwrap_or(false);
            }
        });
    }

    /// Marks who is sharing: the peers in our room that say so, and us while we share.
    fn show_shares(&self) {
        let me = self.call.me();
        let mine = self.media.share.filter(|_| self.room.is_some());
        let peers: BTreeMap<UserId, Option<ShareConfig>> =
            self.peers.values().map(|p| (p.user, p.share)).collect();
        self.shared.update(|s| {
            for (user, person) in &mut s.people {
                person.sharing = if *user == me {
                    mine
                } else {
                    peers.get(user).copied().flatten()
                };
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
                Err(e) => self.shared.notice(format!("Couldn't join: {e}")),
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
            Command::StartShare(config) => self.start_share(config).await,
            Command::StopShare => {
                self.stop_share();
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
            Command::SetProfile { name, picture } => {
                self.change_profile(|p| {
                    if let Some(name) = name {
                        p.name = Some(name);
                    }
                    if let Some(picture) = picture {
                        p.picture = picture.map(Arc::from);
                    }
                });
                self.push_profile().await;
            }
            Command::FetchAvatar(hash) => self.fetch_avatar(hash).await,
            Command::SetServerProfile { name, picture } => {
                let response = self.set_server_profile(name, picture).await;
                self.shared.emit(match response {
                    Ok(Response::Ok) => Event::ServerProfileSaved,
                    other => Event::ServerProfileRefused(refusal(other)),
                });
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
            Command::AcceptKey(user, key) => self.accept_key(user, key).await,
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
                self.refresh_verified();
            }
            Command::Voice(settings) => {
                self.options.voice = settings;
                self.voice(VoiceCommand::Settings(settings));
            }
            Command::Devices { input, output } => {
                self.options.input_device = input;
                self.options.output_device = output;
                self.start_voice();
                self.voice(VoiceCommand::Loopback(self.mic_test));
                if self.call.engine.keys().keys.room().is_some() {
                    self.voice(VoiceCommand::SendOn(self.my_tracks.as_ref().map(|t| t.mic)));
                    self.voice(VoiceCommand::Muted(!self.media.mic));
                    self.voice(VoiceCommand::Deafened(self.media.deafened));
                    self.peers.clear();
                    self.sync_peers();
                    let watched: Vec<UserId> = self.watched_video.keys().copied().collect();
                    for user in watched {
                        self.stop_watching(user);
                    }
                    self.sync_watching();
                    for (user, person) in &self.shared.state().people {
                        self.voice(VoiceCommand::Volume(*user, person.volume));
                    }
                }
            }
            Command::MicTest(on) => {
                self.mic_test = on;
                self.voice(VoiceCommand::Loopback(on));
            }
            Command::PushToTalk(down) => self.voice(VoiceCommand::PushToTalk(down)),
            Command::LeaveServer | Command::Disconnect => {}
        }
    }

    async fn list_users(&mut self) {
        if let Ok(Response::Users(users)) = self.call.request(Request::ListUsers).await {
            self.shared.emit(Event::Users(users));
        }
    }

    fn my_person(&self) -> Option<Person> {
        let state = self.shared.state();
        state.person(state.me?).cloned()
    }

    /// Sets the chosen picture on our profile once we know our own name, which arrives
    /// with our presence when we join a room.
    async fn apply_pending_avatar(&mut self) {
        let Some(avatar) = self.pending_avatar else {
            return;
        };
        let Some(me) = self.my_person() else { return };
        self.pending_avatar = None;
        if me.avatar == avatar {
            self.shared.emit(Event::ProfileSaved);
        } else {
            self.save_profile(me.name, avatar).await;
        }
    }

    async fn save_profile(&mut self, name: String, avatar: Option<Hash>) {
        match self
            .call
            .request(Request::SetProfile { name, avatar })
            .await
        {
            Ok(Response::Ok) => self.shared.emit(Event::ProfileSaved),
            other => self.shared.emit(Event::ProfileRefused(refusal(other))),
        }
    }

    /// Fetches a picture by its hash, keeping it only if the bytes match the hash.
    async fn fetch_avatar(&mut self, hash: Hash) {
        let slot = match self.call.request(Request::GetAvatar(hash)).await {
            Ok(Response::Avatar(bytes)) if Sha256::digest(&bytes).as_slice() == hash.0 => {
                AvatarSlot::Ready(bytes.into())
            }
            Ok(_) => AvatarSlot::Missing,
            Err(e) => {
                info!("fetching a picture: {e:#}");
                self.shared.forget_avatar(hash);
                return;
            }
        };
        self.shared.set_avatar(hash, slot);
    }

    async fn simple(&mut self, request: Request) {
        match self.call.request(request).await {
            Ok(Response::Ok) => {}
            other => self.refused(other),
        }
    }

    /// Sends the changed parts of the server's profile; the answer is the first refusal or `Ok`.
    async fn set_server_profile(
        &mut self,
        name: Option<Option<String>>,
        picture: Option<Option<Vec<u8>>>,
    ) -> anyhow::Result<Response> {
        if let Some(picture) = picture {
            let avatar = match picture {
                Some(bytes) => match self.call.request(Request::SetAvatar(bytes)).await? {
                    Response::AvatarHash(hash) => Some(hash),
                    other => return Ok(other),
                },
                None => None,
            };
            let answer = self.call.request(Request::SetServerAvatar(avatar)).await?;
            if answer != Response::Ok {
                return Ok(answer);
            }
        }
        match name {
            Some(name) => self.call.request(Request::SetServerName(name)).await,
            None => Ok(Response::Ok),
        }
    }

    fn refused(&self, response: anyhow::Result<Response>) {
        self.shared.notice(refusal(response));
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
        self.show_shares();
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
        self.sync_watching();
    }

    /// Keeps one decoder per watched share, following track and codec changes.
    fn sync_watching(&mut self) {
        let wanted: HashMap<UserId, (Share, ShareConfig)> = self
            .watching
            .iter()
            .filter_map(|user| {
                let peer = self.peers.get(user)?;
                Some((*user, (*peer.tracks.shares.first()?, peer.share?)))
            })
            .collect();
        let stale: Vec<UserId> = self
            .watched_video
            .iter()
            .filter(|(user, (video, old))| {
                wanted.get(user).is_none_or(|(share, config)| {
                    share.video != *video
                        || config.codec != old.codec
                        || config.audio != old.audio
                        || (config.width, config.height) != (old.width, old.height)
                })
            })
            .map(|(user, _)| *user)
            .collect();
        for user in stale {
            self.stop_watching(user);
        }
        for (user, (share, config)) in wanted {
            if self.watched_video.contains_key(&user) {
                continue;
            }
            let playout = config.audio.then(|| {
                let playout = Playout::new();
                let units = self.call.engine.subscribe(share.audio, Kind::Audio, 64);
                self.voice(VoiceCommand::ListenShare {
                    user,
                    units,
                    playout: playout.clone(),
                });
                self.tracks.insert(
                    share.audio,
                    self.watched(share.audio, user, "screen audio", SHARE_AUDIO_KBPS),
                );
                self.watched_audio.insert(user, share.audio);
                playout
            });
            let video = share.video;
            #[cfg(feature = "ffmpeg")]
            {
                let units = self.call.engine.subscribe(video, Kind::Video, 64);
                let ask_idr = self.call.engine.idr_requester(video);
                match crate::media::share::receive(units, config, playout, ask_idr) {
                    Ok(picture) => self.shared.set_picture(user, Some(picture)),
                    Err(e) => self.shared.notice(format!("Can't show this share: {e}")),
                }
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                drop(playout);
                self.shared
                    .notice("This build can't show video; it was built without ffmpeg.");
            }
            self.tracks
                .insert(video, self.watched(video, user, "screen", config.kbps));
            self.watched_video.insert(user, (video, config));
        }
    }

    /// Health bookkeeping for `media_id`, counting from the engine's totals so far.
    fn watched(
        &self,
        media_id: MediaId,
        user: UserId,
        label: &'static str,
        set_kbps: u32,
    ) -> Watched {
        let (last_received, last_lost) = self.call.engine.datagrams(media_id);
        Watched {
            user,
            label,
            set_kbps,
            last_bytes: self.call.engine.received_bytes(media_id),
            last_received,
            last_lost,
        }
    }

    fn stop_watching(&mut self, user: UserId) {
        if let Some((video, _)) = self.watched_video.remove(&user) {
            self.call.engine.unsubscribe(video);
            self.tracks.remove(&video);
        }
        if let Some(audio) = self.watched_audio.remove(&user) {
            self.call.engine.unsubscribe(audio);
            self.tracks.remove(&audio);
            self.voice(VoiceCommand::ForgetShare(user));
        }
        self.shared.set_picture(user, None);
    }

    #[cfg(feature = "ffmpeg")]
    async fn start_share(&mut self, wanted: ShareConfig) {
        let Some(tracks) = self
            .my_tracks
            .as_ref()
            .and_then(|t| t.shares.first())
            .copied()
        else {
            self.shared.notice("Join a room before sharing.");
            return;
        };
        let engine = self.call.engine.clone();
        let started = tokio::task::spawn_blocking(move || {
            let config = crate::media::capture::CaptureConfig::for_share(&wanted);
            let capture = crate::media::capture::start(config)?;
            crate::media::share::ShareSender::start(engine, tracks, wanted, capture)
        })
        .await;
        match started {
            Ok(Ok(sender)) => {
                self.media.share = Some(sender.config);
                self.shared
                    .set_picture(self.call.me(), Some(sender.self_view.clone()));
                info!(encoder = sender.encoder, "sharing");
                self.sharing = Some(sender);
                self.push_media().await;
            }
            Ok(Err(e)) => self.shared.notice(format!("Couldn't start sharing: {e}")),
            Err(_) => self.shared.notice("Couldn't start sharing."),
        }
    }

    #[cfg(not(feature = "ffmpeg"))]
    async fn start_share(&mut self, _wanted: ShareConfig) {
        self.shared
            .notice("This build can't share video; it was built without ffmpeg.");
    }

    fn stop_share(&mut self) {
        #[cfg(feature = "ffmpeg")]
        if let Some(sender) = self.sharing.take() {
            tokio::task::spawn_blocking(move || drop(sender));
            self.shared.set_picture(self.call.me(), None);
        }
    }

    /// Stops and announces the end of a share whose capture stopped on its own.
    async fn stop_ended_share(&mut self) {
        #[cfg(feature = "ffmpeg")]
        if self
            .sharing
            .as_ref()
            .is_some_and(crate::media::share::ShareSender::ended)
        {
            self.stop_share();
            self.media.share = None;
            self.push_media().await;
        }
    }

    fn recover_share(&self, kind: Recovery) {
        #[cfg(feature = "ffmpeg")]
        if let Some(sender) = &self.sharing {
            sender.recover(kind);
        }
        #[cfg(not(feature = "ffmpeg"))]
        let _ = kind;
    }

    /// Pins `key` for `user` if it is the changed key we warned about.
    async fn accept_key(&mut self, user: UserId, key: PublicKey) {
        let warned = self
            .shared
            .state()
            .key_warnings
            .iter()
            .any(|w| w.user == user && w.now.0 == key);
        if !warned {
            return;
        }
        let pinned = self.call.engine.keys().contacts.pin(user, key);
        if let Err(e) = pinned {
            warn!("saving the new key: {e:#}");
            self.shared
                .notice(format!("Couldn't keep the new key: {e}"));
        }
        self.shared
            .update(|s| s.key_warnings.retain(|w| w.user != user));
        let notices = self.call.refresh_keys();
        self.notices(notices);
        self.refresh_verified();
        self.follow_room().await;
    }

    /// Replaces our encryption key once it nears expiry, and gives the server the new one.
    async fn renew_enc_key(&mut self) {
        match self.keystore.renew_enc_key_if_due() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                warn!("renewing the encryption key: {e:#}");
                return;
            }
        }
        let enc = match EncKeyPair::from_secret(&self.keystore.enc_key().secret()) {
            Ok(enc) => enc,
            Err(e) => {
                warn!("the renewed encryption key is unusable: {e}");
                return;
            }
        };
        let signed = self
            .keystore
            .signed_enc_key(&self.call.server(), self.call.me());
        self.call.engine.keys().keys.replace_enc(enc);
        match self.call.request(Request::SetEncKey(signed)).await {
            Ok(Response::Ok) => info!("renewed the encryption key"),
            other => {
                warn!(
                    "the server refused the renewed encryption key: {}",
                    refusal(other)
                );
                self.call
                    .connection
                    .close(close::NORMAL.into(), b"encryption key renewed");
            }
        }
    }

    fn name_of(&self, user: UserId) -> String {
        self.shared
            .state()
            .person(user)
            .map_or(format!("user {}", user.0), |p| p.name.clone())
    }

    fn notices(&self, notices: Vec<KeyNotice>) {
        for notice in notices {
            match notice {
                KeyNotice::KeyChanged { user, pinned, now } => self.shared.update(|s| {
                    let warning = KeyWarning {
                        user,
                        pinned: Fingerprint(pinned),
                        now: Fingerprint(now),
                    };
                    if !s.key_warnings.contains(&warning) {
                        s.key_warnings.retain(|w| w.user != user);
                        s.key_warnings.push(warning);
                    }
                }),
                KeyNotice::BadEncKey { user } => {
                    self.shared.notice(format!(
                        "User {}'s encryption key failed its check; they can't hear you.",
                        user.0
                    ));
                }
                KeyNotice::NewContact { user } => self.shared.notice(format!(
                    "You're meeting {} here for the first time. Compare safety numbers in Privacy and keys to verify them.",
                    self.name_of(user)
                )),
                KeyNotice::ClashingTracks => self.shared.notice(
                    "The server described the room with two tracks under one id, so the call is paused until it sends a valid one.",
                ),
            }
        }
    }

    async fn event(&mut self, event: CallEvent) {
        match event {
            CallEvent::Key(notice) => self.notices(vec![notice]),
            CallEvent::Server(event) => {
                self.server_event(event).await;
                self.apply_pending_avatar().await;
            }
        }
    }

    async fn server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::ServerProfileChanged(profile) => self.shared.update(|s| {
                if let Some(server) = s.server.as_mut() {
                    server.name = profile.name;
                    server.avatar = profile.avatar;
                }
            }),
            ServerEvent::PresenceChanged(p) => {
                self.shared.update(|s| {
                    let updated = match s.people.get(&p.user) {
                        Some(old) => Person {
                            verified: old.verified,
                            speaking: old.speaking,
                            ..person(&p, old.volume)
                        },
                        None => person(&p, 1.0),
                    };
                    s.people.insert(p.user, updated);
                });
                self.refresh_verified();
                self.follow_room().await;
            }
            ServerEvent::PresenceEnded(user) => {
                self.shared.update(|s| {
                    s.people.remove(&user);
                });
                self.follow_room().await;
            }
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
                    self.shared.notice("The room you were in was deleted.");
                }
                self.shared.update(|s| {
                    s.rooms.retain(|r| r.id != room);
                    if s.current.as_ref().is_some_and(|c| c.room == room) {
                        s.current = None;
                    }
                });
            }
            ServerEvent::RoomState { room, .. } => {
                if Some(room) == self.room {
                    self.follow_room().await;
                }
            }
            ServerEvent::LinkStats { tracks } => {
                self.dropped_for_you = tracks.iter().fold(self.dropped_for_you, |sum, t| {
                    sum.saturating_add(t.dropped_units)
                });
            }
            ServerEvent::RoleChanged { user, role } => {
                if user == self.call.me() {
                    self.shared.update(|s| s.role = Some(role));
                }
            }
            ServerEvent::InviteBurned { code_id } => self.shared.notice(format!(
                "Invite {code_id}-… was used up by three wrong tries and no longer works."
            )),
            ServerEvent::Kicked { reason } => self.shared.emit(Event::Kicked(reason)),
            ServerEvent::UserAdded(_) | ServerEvent::UserRemoved(_) => self.list_users().await,
            ServerEvent::KeyframeWanted { media_id, kind } => {
                if self
                    .my_tracks
                    .as_ref()
                    .and_then(|t| t.shares.first())
                    .is_some_and(|s| s.video == media_id)
                {
                    self.recover_share(kind);
                }
            }
            ServerEvent::SenderKey { .. } => {}
        }
    }

    fn refresh_levels(&mut self) {
        let Some(voice) = &self.voice else { return };
        let levels = voice.levels();
        *lock(&self.shared.mic_level) = Some(levels.mic_dbfs);
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
        let upload_loss = percent(
            advance(&mut self.last_lost_sent, stats.path.lost_packets),
            advance(&mut self.last_sent, stats.path.sent_packets),
        );
        let local_drops = self
            .call
            .engine
            .stats
            .send_dropped_units
            .load(Ordering::Relaxed);
        let new_local_drops = advance(&mut self.last_local_drops, local_drops);

        let state = self.shared.state();
        let mut rows = Vec::new();
        let (mut received_total, mut lost_total) = (0, 0);
        for (media_id, track) in self.tracks.iter_mut() {
            let bytes = self.call.engine.received_bytes(*media_id);
            let (received, lost) = self.call.engine.datagrams(*media_id);
            let new_bytes = advance(&mut track.last_bytes, bytes);
            let new_received = advance(&mut track.last_received, received);
            let new_lost = advance(&mut track.last_lost, lost);
            received_total += new_received;
            lost_total += new_lost;
            let name = state
                .person(track.user)
                .map_or("someone".to_owned(), |p| p.name.clone());
            rows.push(TrackHealth {
                label: format!("{name}, {}", track.label),
                set_kbps: Some(track.set_kbps),
                arriving_kbps: (new_received > 0).then_some(kbps(new_bytes)),
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
            verdict: verdict(dropped, new_local_drops, download_loss),
            sending: self.sent_tracks(new_local_drops),
            receiving: rows,
        };
        self.shared.update(|s| s.health = health);
    }

    fn sent_tracks(&mut self, new_local_drops: u64) -> Vec<SentTrack> {
        let Some(tracks) = &self.my_tracks else {
            return Vec::new();
        };
        let mut wanted = Vec::new();
        if self.media.mic {
            wanted.push(("mic", tracks.mic, MIC_KBPS, None));
        }
        if let (Some(config), Some(share)) = (self.media.share, tracks.shares.first()) {
            wanted.push(("screen", share.video, config.kbps, Some(new_local_drops)));
            if config.audio {
                wanted.push(("screen audio", share.audio, SHARE_AUDIO_KBPS, None));
            }
        }
        wanted
            .into_iter()
            .map(|(label, media_id, set_kbps, dropped)| {
                let bytes = self.call.engine.sent_bytes(media_id);
                let last = self.last_sent_bytes.entry(media_id).or_insert(bytes);
                SentTrack {
                    label,
                    set_kbps,
                    leaving_kbps: kbps(advance(last, bytes)),
                    dropped,
                }
            })
            .collect()
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

/// Why the server didn't answer `Ok`, in its own words where it gave some.
fn refusal(response: anyhow::Result<Response>) -> String {
    match response {
        Ok(Response::Err { message, .. }) => message,
        Ok(other) => format!("unexpected reply {other:?}"),
        Err(e) => e.to_string(),
    }
}

/// How far `counter` has moved since `last`, which it then replaces.
fn advance(last: &mut u64, counter: u64) -> u64 {
    counter.saturating_sub(std::mem::replace(last, counter))
}

/// The rate of `bytes` sent or received over one health refresh.
fn kbps(bytes: u64) -> u32 {
    (bytes * 8 / HEALTH_EVERY.as_secs() / 1000) as u32
}

fn percent(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 * 100.0 / whole as f32
    }
}

/// One plain-language sentence on what limits the call right now (PLAN §5.11).
fn verdict(dropped_for_you: u32, local_drops: u64, download_loss: f32) -> String {
    if dropped_for_you > 0 {
        return format!(
            "{dropped_for_you} frames were dropped for you in the last 2 s. Your download can't carry everything you watch."
        );
    }
    if local_drops > 0 {
        return "Your upload can't carry your share's bitrate, so frames are dropped before they leave.".into();
    }
    if download_loss > 2.0 {
        return format!("{download_loss:.1}% of packets are lost between the server and you.");
    }
    "Everything is arriving.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_names_the_bottleneck() {
        assert!(verdict(3, 0, 0.0).contains("dropped for you"));
        assert!(verdict(0, 2, 0.0).contains("Your upload"));
        assert!(verdict(0, 0, 4.0).contains("4.0% of packets"));
        assert_eq!(verdict(0, 0, 0.0), "Everything is arriving.");
    }
}
