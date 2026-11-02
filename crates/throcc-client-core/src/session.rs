use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use throcc_crypto::identity::random;
use throcc_proto::ids::{Hash, MediaId, RoomId, UserId};
use throcc_proto::messages::{
    AuthError, ErrorCode, Event as ServerEvent, MediaState, Placed, Presence, PublicKey, Recovery,
    Request, Response, RoomPeer, Share, ShareConfig, User, Welcome,
};
use throcc_proto::transport::close;
use tokio::sync::{Semaphore, broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::account::{self, SignInError};
use crate::avatars;
use crate::call::{Call, CallEvent};
use crate::client::{ClientOptions, Command, Event};
use crate::control::{self, LoginError};
use crate::keys::{KeyNotice, MAX_ROOM_PEERS};
use crate::keystore::Keystore;
use crate::lock;
use crate::media::cues::Cue;
use crate::media::engine::Kind;
use crate::media::share_audio::Playout;
use crate::media::video::{Picture, PreviewDemand};
use crate::media::voice::{VoiceCommand, VoiceEngine};
use crate::playback::SavedPlayback;
use crate::profile::Profile;
use crate::server_look::ServerLook;
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, MIC_KBPS, Person, Playback,
    SHARE_AUDIO_DETAIL, SHARE_AUDIO_KBPS, SentTrack, ServerView, Sound, State, TrackHealth,
    VOICE_DETAIL, person_name, room_name, share_detail, shown_name,
};
use crate::trust::Trust;
use crate::verified::VerifiedKeys;

const LEVELS_EVERY: Duration = Duration::from_millis(100);
const HEALTH_EVERY: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
/// Each reconnect wait is spread over this fraction either side of the backoff, so clients
/// that lost the same server don't all come back in the same instant.
const BACKOFF_JITTER: f64 = 0.3;
const RENEWAL_CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
const MAX_QUEUED_WHILE_RECONNECTING: usize = 64;
/// How many pictures are fetched from the server at once.
const AVATAR_FETCHES: usize = 8;
const EVENTS: usize = 256;
/// Pictures kept in memory before the ones nothing shown uses are dropped.
const AVATARS_KEPT: usize = 256;

/// What the client and its session task share.
pub(crate) struct Shared {
    pub state: Mutex<Arc<State>>,
    pub events: broadcast::Sender<Event>,
    pub pictures: Mutex<HashMap<UserId, Picture>>,
    /// Our own user and the preview demand of our running share.
    pub preview: Mutex<Option<(UserId, Arc<PreviewDemand>)>>,
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
    pub fn new(state: State) -> Self {
        let (events, _) = broadcast::channel(EVENTS);
        Self {
            state: Mutex::new(Arc::new(state)),
            events,
            pictures: Mutex::default(),
            preview: Mutex::default(),
            avatars: Mutex::default(),
            mic_level: Mutex::default(),
        }
    }

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

    pub fn forget_avatar(&self, hash: Hash) {
        lock(&self.avatars).remove(&hash);
    }

    pub fn set_avatar(&self, hash: Hash, slot: AvatarSlot) {
        let ready = matches!(slot, AvatarSlot::Ready(_));
        let state = self.state();
        let mut avatars = lock(&self.avatars);
        if avatars.len() >= AVATARS_KEPT {
            let in_use = state.avatars_in_use();
            avatars.retain(|h, slot| matches!(slot, AvatarSlot::Fetching) || in_use.contains(h));
        }
        avatars.insert(hash, slot);
        drop(avatars);
        if ready {
            self.emit(Event::AvatarReady(hash));
        }
    }

    pub fn picture(&self, user: UserId) -> Option<Picture> {
        lock(&self.pictures).get(&user).cloned()
    }

    /// Marks `user`'s picture as drawn, which our own share needs to build its preview.
    pub fn picture_drawn(&self, user: UserId) {
        if let Some((me, demand)) = lock(&self.preview).as_ref()
            && *me == user
        {
            demand.drawn();
        }
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

    pub fn update(&self, change: impl FnOnce(&mut State)) {
        let mut guard = lock(&self.state);
        let mut state = (**guard).clone();
        change(&mut state);
        if state != **guard {
            *guard = Arc::new(state);
            self.events.send(Event::State(guard.clone())).ok();
        }
    }

    pub fn emit(&self, event: Event) {
        self.events.send(event).ok();
    }

    pub fn notice(&self, text: impl Into<String>) {
        self.emit(Event::Notice(text.into()));
    }
}

/// One subscribed remote track, with counters from the last health refresh.
struct Watched {
    user: UserId,
    carries: Carries,
    last_bytes: u64,
    last_received: u64,
    last_lost: u64,
}

/// What a watched track carries.
enum Carries {
    Mic,
    ShareAudio,
    ShareVideo {
        config: ShareConfig,
        decoder: Option<watch::Receiver<&'static str>>,
    },
}

impl Carries {
    fn label(&self) -> &'static str {
        match self {
            Carries::Mic => "mic",
            Carries::ShareAudio => "screen audio",
            Carries::ShareVideo { .. } => "screen",
        }
    }

    fn set_kbps(&self) -> u32 {
        match self {
            Carries::Mic => MIC_KBPS,
            Carries::ShareAudio => SHARE_AUDIO_KBPS,
            Carries::ShareVideo { config, .. } => config.kbps,
        }
    }

    fn detail(&self) -> String {
        match self {
            Carries::Mic => VOICE_DETAIL.into(),
            Carries::ShareAudio => SHARE_AUDIO_DETAIL.into(),
            Carries::ShareVideo { config, decoder } => share_detail(
                config,
                decoder.as_ref().map_or("not decoded", |d| *d.borrow()),
            ),
        }
    }
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
    /// The share the server last took from us. A share-start refusal leaves it in place.
    server_share: Option<ShareConfig>,
    /// The peers our key is wrapped to, whose tracks we follow.
    peers: BTreeMap<UserId, RoomPeer>,
    trust: Trust,
    watching: Vec<UserId>,
    /// The shares we receive, by who shares them.
    watched_shares: HashMap<UserId, WatchedShare>,
    mic_test: bool,
    /// Whether the mic was on when we deafened, so undeafening restores it.
    mic_before_deafen: bool,
    /// A picture to put on our profile once the server tells us our name.
    pending_avatar: Option<Option<Hash>>,
    profile: Profile,
    saved_playback: SavedPlayback,
    server_look: ServerLook,
    avatar_fetches: Arc<Semaphore>,
    #[cfg(feature = "ffmpeg")]
    sharing: Option<crate::media::share::ShareSender>,
    share_starts: ShareStarts,
    tracks: HashMap<MediaId, Watched>,
    dropped_for_you: u32,
    last: LastCounts,
}

/// The starts of our share. A capture starts beside the session loop, since picking what to
/// share waits on the user; only the latest start is applied, and only while still wanted.
#[cfg(feature = "ffmpeg")]
struct ShareStarts {
    /// The number of the start still wanted.
    wanted: Option<u64>,
    count: u64,
    done: mpsc::Sender<StartedShare>,
    started: mpsc::Receiver<StartedShare>,
}

#[cfg(feature = "ffmpeg")]
type StartedShare = (u64, StartResult);

#[cfg(feature = "ffmpeg")]
type StartResult = Result<
    Result<crate::media::share::ShareSender, crate::media::share::ShareError>,
    tokio::task::JoinError,
>;

#[cfg(feature = "ffmpeg")]
impl ShareStarts {
    fn new() -> Self {
        let (done, started) = mpsc::channel(4);
        Self {
            wanted: None,
            count: 0,
            done,
            started,
        }
    }

    async fn next(&mut self) -> Option<StartedShare> {
        self.started.recv().await
    }
}

/// Without ffmpeg nothing is shared, so no start ever ends.
#[cfg(not(feature = "ffmpeg"))]
struct ShareStarts;

#[cfg(not(feature = "ffmpeg"))]
type StartedShare = std::convert::Infallible;

#[cfg(not(feature = "ffmpeg"))]
impl ShareStarts {
    fn new() -> Self {
        Self
    }

    async fn next(&mut self) -> Option<StartedShare> {
        std::future::pending().await
    }
}

/// A share we receive: the tracks it comes on, and the config its decoder was set up for.
struct WatchedShare {
    video: MediaId,
    audio: Option<MediaId>,
    config: ShareConfig,
}

/// The connection's counters at the last health refresh.
#[derive(Default)]
struct LastCounts {
    sent: u64,
    lost_sent: u64,
    local_drops: u64,
    sent_bytes: HashMap<MediaId, u64>,
    udp_received: u64,
    udp_sent: u64,
}

impl Session {
    pub fn new(
        shared: Arc<Shared>,
        keystore: Keystore,
        address: String,
        options: ClientOptions,
        call: Call,
    ) -> Self {
        let server = call.server();
        let profile = Profile::load(keystore.dir()).unwrap_or_else(|e| {
            warn!("no saved profile: {e:#}");
            Profile::default()
        });
        let saved_playback = SavedPlayback::load(keystore.dir(), &server).unwrap_or_else(|e| {
            warn!("no saved playback: {e:#}");
            SavedPlayback::empty(keystore.dir(), &server)
        });
        let server_look = ServerLook::load(keystore.dir(), &server).unwrap_or_else(|e| {
            warn!("no kept server look: {e:#}");
            ServerLook::default()
        });
        let verified = VerifiedKeys::load(keystore.dir()).unwrap_or_else(|e| {
            warn!("no verified keys: {e:#}");
            shared.notice(format!(
                "Couldn't read who you verified ({e}), so everyone shows as not verified."
            ));
            VerifiedKeys::empty(keystore.dir())
        });
        let mut session = Self {
            shared,
            keystore,
            address,
            options,
            voice: None,
            room: None,
            my_tracks: None,
            media: MediaState {
                mic: true,
                ..MediaState::default()
            },
            server_share: None,
            peers: BTreeMap::new(),
            trust: Trust::new(server, verified),
            watching: Vec::new(),
            watched_shares: HashMap::new(),
            mic_test: false,
            mic_before_deafen: false,
            pending_avatar: None,
            profile,
            saved_playback,
            server_look,
            avatar_fetches: Arc::new(Semaphore::new(AVATAR_FETCHES)),
            #[cfg(feature = "ffmpeg")]
            sharing: None,
            share_starts: ShareStarts::new(),
            tracks: HashMap::new(),
            dropped_for_you: 0,
            last: LastCounts::default(),
            call,
        };
        session.show_profile();
        session.start_voice();
        let welcome = session.call.welcome.clone();
        session.apply_welcome(&welcome);
        session
    }

    fn show_profile(&self) {
        let picture = self.profile.picture.clone().map(|bytes| {
            let hash = Hash::of(&bytes);
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

    /// Starts the voice engine afresh and gives it everything it plays and sends.
    fn start_voice(&mut self) {
        self.voice = None;
        *lock(&self.shared.mic_level) = None;
        if self.options.audio {
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
        for (user, sound, playback) in self.saved_playback.iter() {
            self.voice(gain_command(user, sound, playback));
        }
        self.voice(VoiceCommand::Loopback(self.mic_test));
        if self.room.is_some() {
            self.sync_voice();
            self.peers.clear();
            self.sync_peers();
            self.stop_watching_all();
            self.sync_watching();
        }
    }

    /// Tells the voice engine our switches and the track our mic goes out on.
    fn sync_voice(&self) {
        self.voice(VoiceCommand::Muted(!self.media.mic));
        self.voice(VoiceCommand::Deafened(self.media.deafened));
        self.voice(VoiceCommand::SendOn(self.my_tracks.as_ref().map(|t| t.mic)));
    }

    fn voice(&self, command: VoiceCommand) {
        if let Some(voice) = &self.voice {
            voice.command(command);
        }
    }

    fn cue(&self, cue: Cue) {
        let gain = self.options.voice.cue_volume;
        if gain > 0.0 {
            self.voice(VoiceCommand::Cue(cue, gain));
        }
    }

    /// Plays join or leave when someone else enters or leaves our room.
    fn room_cue(&self, user: UserId, after: Option<RoomId>) {
        let before = self.shared.state().person(user).and_then(|p| p.room);
        if user == self.call.me() || self.room.is_none() || before == after {
            return;
        }
        if after == self.room {
            self.cue(Cue::Join);
        } else if before == self.room {
            self.cue(Cue::Leave);
        }
    }

    pub async fn run(mut self, commands: mpsc::Receiver<Command>) {
        self.run_until_done(commands).await;
        if let Err(e) = self.call.floors.save() {
            warn!("saving epoch floors: {e:#}");
        }
    }

    async fn run_until_done(&mut self, mut commands: mpsc::Receiver<Command>) {
        if self.profile != Profile::default() {
            self.push_profile().await;
        }
        let mut levels = tokio::time::interval(LEVELS_EVERY);
        let mut health = tokio::time::interval(HEALTH_EVERY);
        let mut renewal = tokio::time::interval(RENEWAL_CHECK_EVERY);
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let command = command.unwrap_or(Command::Disconnect);
                    if self.command(command).await.is_break() {
                        return;
                    }
                }
                event = self.call.events.recv() => match event {
                    Some(event) => self.event(event).await,
                    None => {
                        let Some(queued) = self.reconnect(&mut commands).await else {
                            return;
                        };
                        for command in queued {
                            if self.command(command).await.is_break() {
                                return;
                            }
                        }
                    }
                },
                _ = levels.tick() => {
                    self.refresh_levels();
                    self.stop_ended_share().await;
                }
                _ = health.tick() => {
                    self.refresh_health();
                    if let Err(e) = self.call.floors.save() {
                        warn!("saving epoch floors: {e:#}");
                    }
                    if let Err(e) = self.saved_playback.save() {
                        warn!("saving playback: {e:#}");
                    }
                }
                _ = renewal.tick() => self.renew_enc_key().await,
                Some(_) = self.call.recovery.recv() => self.recover_share(Recovery::Idr),
                Some(started) = self.share_starts.next() => self.share_started(started).await,
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
        if let Err(e) = self.call.floors.save_last() {
            warn!("saving epoch floors before leaving: {e:#}");
        }
        if let Err(e) = self.saved_playback.save_last() {
            warn!("saving playback before leaving: {e:#}");
        }
        if let Err(e) = self.keystore.known_servers.leave(&self.address) {
            warn!("forgetting the server: {e:#}");
        }
        self.call.close().await;
        self.shared
            .update(|s| s.connection = ConnectionStatus::Disconnected);
        self.shared.emit(Event::LeftServer);
        true
    }

    /// Reconnects with backoff, into the room last asked for, or else the same room.
    /// Returns the other commands that arrived meanwhile, or `None` if told to stop or the
    /// server won't take us back.
    async fn reconnect(&mut self, commands: &mut mpsc::Receiver<Command>) -> Option<Vec<Command>> {
        let reason = self
            .call
            .connection
            .close_reason()
            .map_or("connection lost".to_owned(), |e| e.to_string());
        self.cue(Cue::ConnectionLost);
        let mut backoff = Duration::from_secs(1);
        let mut queued = Vec::new();
        let mut dropped_any = false;
        let mut want_room = self.room;
        let (room_before, watching_before) = (self.room, self.watching.clone());
        for attempt in 1.. {
            self.shared.update(|s| {
                s.connection = ConnectionStatus::Reconnecting {
                    attempt,
                    reason: reason.clone(),
                }
            });
            let wait = tokio::time::sleep(jittered(backoff, u64::from_le_bytes(random())));
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    command = commands.recv() => match command {
                        None | Some(Command::Disconnect) => {
                            self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                            return None;
                        }
                        Some(Command::Join(room)) => want_room = room,
                        // Asked again on the next render once connected.
                        Some(Command::FetchAvatar(hash)) => self.shared.forget_avatar(hash),
                        Some(command) => {
                            queued.retain(|older| !supersedes(&command, older));
                            if queued.len() < MAX_QUEUED_WHILE_RECONNECTING {
                                queued.push(command);
                            } else if !dropped_any {
                                dropped_any = true;
                                warn!("dropped a command while reconnecting");
                                self.shared.notice("Still reconnecting, so that was dropped. Try again once connected.");
                            }
                        }
                    },
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            if let Err(e) = self.keystore.renew_enc_key_if_due() {
                warn!("renewing the encryption key: {e:#}");
            }
            let link = match account::sign_in(&self.keystore, &self.address, want_room).await {
                Ok(link) => link,
                Err(e) => {
                    let fatal = match &e {
                        SignInError::Login(LoginError::Refused(reason))
                            if *reason != AuthError::RateLimited =>
                        {
                            Some(format!("The server refused you: {reason}"))
                        }
                        _ => e.pin_mismatch().map(|(expected, actual)| {
                            format!(
                                "The server's key changed from {} to {}",
                                expected.short(),
                                actual.short()
                            )
                        }),
                    };
                    if let Some(failed) = fatal {
                        self.shared
                            .update(|s| s.connection = ConnectionStatus::Failed(failed));
                        return None;
                    }
                    info!("reconnect attempt {attempt} failed: {e}");
                    continue;
                }
            };
            match Call::restart(link, &self.keystore, &self.call) {
                Ok(call) => {
                    self.call = call;
                    self.trust.forget_presented();
                    self.reset_room();
                    self.start_voice();
                    self.cue(Cue::Reconnected);
                    let welcome = self.call.welcome.clone();
                    let wanted = want_room.map(|room| (room, self.room_name(room)));
                    self.apply_welcome(&welcome);
                    if let Some((room, name)) = wanted
                        && welcome.placed.room != Some(room)
                    {
                        self.shared.notice(format!(
                            "Reconnected, but not back into {name}; you're in the lobby."
                        ));
                    }
                    self.push_media().await;
                    if self.room.is_some() && self.room == room_before {
                        self.watching = watching_before;
                        self.push_watching().await;
                    }
                    return Some(queued);
                }
                Err(e) => warn!("reconnect: {e:#}"),
            }
        }
        None
    }

    fn room_name(&self, room: RoomId) -> String {
        let state = self.shared.state();
        state
            .room(room)
            .map_or("your room".to_owned(), |r| r.name.clone())
    }

    /// Leaves our room here after the server took us out of it.
    fn put_out_of_room(&mut self) {
        self.call.engine.placed(&Placed {
            room: None,
            epoch: throcc_proto::ids::Epoch(0),
            tracks: None,
            peers: vec![],
        });
        self.reset_room();
        self.voice(VoiceCommand::ForgetAll);
        self.voice(VoiceCommand::SendOn(None));
        self.shared.update(|s| s.current = None);
    }

    fn reset_room(&mut self) {
        self.stop_share();
        // The server ends our share whenever it moves us, and a new connection starts without one.
        self.server_share = None;
        self.stop_watching_all();
        self.room = None;
        self.my_tracks = None;
        self.peers.clear();
        self.tracks.clear();
        self.last.sent_bytes.clear();
        self.watching.clear();
        self.show_shares();
    }

    fn apply_welcome(&mut self, welcome: &Welcome) {
        let fingerprint = self.call.server();
        self.remember_server(welcome.server.name.clone(), welcome.server.avatar);
        let address = self.address.clone();
        let media = self.media;
        let saved = &self.saved_playback;
        self.shared.update(move |s| {
            s.mic = media.mic;
            s.deafened = media.deafened;
            s.server = Some(ServerView {
                address,
                fingerprint,
                name: welcome.server.name.as_deref().and_then(shown_name),
                avatar: welcome.server.avatar,
            });
            s.connection = ConnectionStatus::Connected;
            s.me = Some(welcome.me);
            s.role = Some(welcome.role);
            s.rooms.clear();
            for room in &welcome.rooms {
                s.add_room(room.into());
            }
            s.people.clear();
            for p in &welcome.presence {
                s.add_person(p.user, person(p, saved));
            }
            s.current = None;
        });
        self.refresh_trust();
        if welcome.placed.room.is_some() {
            self.after_join(&welcome.placed);
        }
    }

    async fn joined(&mut self, placed: &Placed, notices: Vec<KeyNotice>) {
        self.after_join(placed);
        self.cue(if placed.room.is_some() {
            Cue::Join
        } else {
            Cue::Leave
        });
        self.notices(notices);
        self.push_media().await;
    }

    fn after_join(&mut self, placed: &Placed) {
        // The floor of a new placement is what a replaying server wants lost, so it isn't
        // left to the next health tick.
        if let Err(e) = self.call.floors.save() {
            warn!("saving epoch floors: {e:#}");
        }
        self.reset_room();
        self.room = placed.room;
        self.my_tracks = placed.tracks.clone();
        self.voice(VoiceCommand::ForgetAll);
        self.sync_voice();
        self.sync_peers();
        let room = placed.room;
        self.shared.update(|s| {
            s.current = room.map(|room| CurrentRoom {
                room,
                sharing: None,
                watching: vec![],
            });
        });
    }

    /// Follows the peers our key is wrapped to: hears every mic, notes shares, drops who
    /// left. True when they changed.
    fn sync_peers(&mut self) -> bool {
        let trusted = self.call.engine.keys().trusted_room_peers();
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
                    self.watched(peer.tracks.mic, *user, Carries::Mic),
                );
            }
        }
        let share_change = current.iter().find_map(|(user, peer)| {
            share_cue(self.peers.get(user)?.share.is_some(), peer.share.is_some())
        });
        if let Some(cue) = share_change {
            self.cue(cue);
        }
        self.peers = current;
        self.show_shares();
        self.note_identities();
        changed
    }

    /// Notes the identity keys of the peers our key is wrapped to, then shows who is
    /// verified and whose verified key changed.
    fn note_identities(&mut self) {
        let presented: Vec<(UserId, PublicKey)> = self
            .peers
            .iter()
            .map(|(user, peer)| (*user, peer.identity_key))
            .collect();
        if self.trust.note(&presented) {
            self.refresh_trust();
        }
    }

    /// Follows a change in who we share keys with, and in the shares we watch from them.
    async fn follow_room(&mut self) {
        if self.sync_peers() && !self.watching.is_empty() {
            self.push_watching().await;
        }
    }

    /// Shows who is verified, whose verified key changed, and everyone verified anywhere.
    fn refresh_trust(&self) {
        let state = self.shared.state();
        let trust: BTreeMap<UserId, (bool, Option<PublicKey>)> = state
            .people
            .keys()
            .map(|user| (*user, self.trust.of(*user)))
            .collect();
        let me = self.call.me();
        let warnings: Vec<KeyWarning> = state
            .people
            .keys()
            .filter(|user| **user != me)
            .filter_map(|user| self.trust.warning(*user))
            .collect();
        let everyone = self.trust.list();
        self.shared.update(|s| {
            for (user, person) in &mut s.people {
                if let Some(&(verified, key)) = trust.get(user) {
                    (person.verified, person.identity_key) = (verified, key);
                }
            }
            s.key_warnings = warnings;
            s.verified = everyone;
        });
    }

    /// Verifies `key`, whose safety number the user compared, or takes it off the verified
    /// ones, while `user` still presents it.
    fn set_verified(&mut self, user: UserId, key: &PublicKey, verified: bool) {
        let name = self
            .shared
            .state()
            .person(user)
            .map_or_else(|| person_name(user, ""), |p| p.name.clone());
        let changed = self.trust.set_verified(user, key, verified, &name);
        if let Ok(false) = changed {
            self.shared.notice(format!(
                "{name}'s key changed before you confirmed, so nothing changed. Compare the new safety number."
            ));
            return;
        }
        self.after_trust_change(changed.map(drop));
    }

    fn after_trust_change(&mut self, saved: anyhow::Result<()>) {
        if let Err(e) = saved {
            warn!("saving who is verified: {e:#}");
            self.shared
                .notice(format!("Couldn't save who you verified: {e}"));
        }
        self.refresh_trust();
    }

    /// Marks who is sharing: the peers in our room that say so, and us while we share.
    fn show_shares(&self) {
        self.shared.update(|s| {
            for (user, person) in &mut s.people {
                person.sharing = self.sharing_of(*user);
            }
        });
    }

    fn sharing_of(&self, user: UserId) -> Option<ShareConfig> {
        if user == self.call.me() {
            self.media.share
        } else {
            self.peers.get(&user).and_then(|p| p.share)
        }
    }

    async fn command(&mut self, command: Command) -> ControlFlow<()> {
        match command {
            Command::Join(room) => match self.call.join(room, None).await {
                Ok((placed, notices)) => self.joined(&placed, notices).await,
                Err(e) => self.shared.notice(format!("Couldn't join: {e}")),
            },
            Command::JoinWithPassword(room, password) => {
                match self.call.join(Some(room), Some(password)).await {
                    Ok((placed, notices)) => self.joined(&placed, notices).await,
                    Err(e) => self.shared.emit(Event::PasswordRefused(e.to_string())),
                }
            }
            Command::SetMic(on) => {
                if let Some(cue) = mic_cue(&self.media, on) {
                    self.cue(cue);
                }
                self.media.mic = on;
                if on {
                    self.media.deafened = false;
                }
                self.push_media().await;
            }
            Command::SetDeafened(deafened) => {
                if deafened != self.media.deafened {
                    self.cue(if deafened { Cue::Deafen } else { Cue::Undeafen });
                }
                set_deafened(&mut self.media, &mut self.mic_before_deafen, deafened);
                self.push_media().await;
            }
            Command::StartShare(config) => self.start_share(config),
            Command::StopShare => self.end_share().await,
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
            Command::SetPlayback(user, sound, playback) => {
                self.voice(gain_command(user, sound, playback));
                self.saved_playback.set(user, sound, playback);
                self.shared.update(|s| {
                    if let Some(p) = s.people.get_mut(&user) {
                        *p.playback.get_mut(sound) = playback;
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
            Command::FetchAvatar(hash) => self.fetch_avatar(hash),
            Command::SetServerProfile { name, picture } => {
                let response = self.set_server_profile(name, picture).await;
                self.shared.emit(match response {
                    Ok(Response::Ok) => Event::ServerProfileSaved,
                    other => Event::ServerProfileRefused(refusal(other)),
                });
            }
            Command::CreateRoom(name, password) => {
                self.simple(Request::CreateRoom { name, password }).await
            }
            Command::RenameRoom(room, name) => {
                self.simple(Request::RenameRoom { room, name }).await
            }
            Command::SetRoomPassword(room, password) => {
                self.simple(Request::SetRoomPassword { room, password })
                    .await
            }
            Command::DeleteRoom(room) => self.simple(Request::DeleteRoom(room)).await,
            Command::KickFromRoom { user, room } => {
                self.simple(Request::KickFromRoom { user, room }).await
            }
            Command::CreateInvite => match self.call.request(Request::CreateInvite).await {
                Ok(Response::Invite { code, expires }) => {
                    self.shared.emit(Event::Invite { code, expires })
                }
                other => self.refused(other),
            },
            Command::ListUsers => match self.call.request(Request::ListUsers).await {
                Ok(Response::Users(users)) => self.shared.emit(Event::Users(shown_users(users))),
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
            Command::SetVerified {
                user,
                key,
                verified,
            } => self.set_verified(user, &key.0, verified),
            Command::DismissKeyChange(user) => {
                let dismissed = self.trust.dismiss_change(user);
                self.after_trust_change(dismissed);
            }
            Command::Unverify(key) => {
                let unverified = self.trust.unverify(&key.0);
                self.after_trust_change(unverified);
            }
            Command::Voice(settings) => {
                self.options.voice = settings;
                self.voice(VoiceCommand::Settings(settings));
            }
            Command::Devices { input, output } => {
                self.options.input_device = input;
                self.options.output_device = output;
                self.start_voice();
            }
            Command::MicTest(on) => {
                self.mic_test = on;
                self.voice(VoiceCommand::Loopback(on));
            }
            Command::PushToTalk(down) => self.voice(VoiceCommand::PushToTalk(down)),
            Command::PlayCue(cue) => self.cue(cue),
            Command::LeaveServer => {
                if self.leave_server().await {
                    return ControlFlow::Break(());
                }
            }
            Command::Disconnect => {
                self.call.close().await;
                self.shared
                    .update(|s| s.connection = ConnectionStatus::Disconnected);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    async fn list_users(&mut self) {
        if let Ok(Response::Users(users)) = self.call.request(Request::ListUsers).await {
            self.shared.emit(Event::Users(shown_users(users)));
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

    /// Finds a picture by its hash in the cache, or fetches it beside the session loop,
    /// [`AVATAR_FETCHES`] at a time. Keeps it only if the bytes match the hash.
    fn fetch_avatar(&self, hash: Hash) {
        let shared = self.shared.clone();
        let requester = self.call.requester.clone();
        let cache = self.keystore.cache_dir().to_owned();
        let fetches = self.avatar_fetches.clone();
        tokio::spawn(async move {
            let cached = {
                let cache = cache.clone();
                tokio::task::spawn_blocking(move || avatars::load(&cache, hash)).await
            };
            if let Ok(Some(bytes)) = cached {
                shared.set_avatar(hash, AvatarSlot::Ready(bytes));
                return;
            }
            let Ok(_turn) = fetches.acquire().await else {
                return;
            };
            let slot = match requester.request(Request::GetAvatar(hash)).await {
                Ok(Response::Avatar(bytes)) if avatars::matches(&bytes, hash) => {
                    let bytes: Arc<[u8]> = bytes.into();
                    let kept = bytes.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = avatars::save(&cache, hash, &kept) {
                            warn!("caching a picture: {e:#}");
                        }
                    });
                    AvatarSlot::Ready(bytes)
                }
                Ok(_) => AvatarSlot::Missing,
                Err(e) => {
                    info!("fetching a picture: {e:#}");
                    shared.forget_avatar(hash);
                    return;
                }
            };
            shared.set_avatar(hash, slot);
        });
    }

    /// Keeps the server's name and picture hash on this computer, to show while not
    /// connected.
    fn remember_server(&mut self, name: Option<String>, avatar: Option<Hash>) {
        let name = name.as_deref().and_then(shown_name);
        let look = ServerLook { name, avatar };
        if look == self.server_look {
            return;
        }
        self.server_look = look;
        if let Err(e) = self
            .server_look
            .save(self.keystore.dir(), &self.call.server())
        {
            warn!("keeping the server's name and picture: {e:#}");
        }
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
        let media = self.media;
        self.shared.update(|s| {
            s.mic = media.mic;
            s.deafened = media.deafened;
            if let Some(c) = s.current.as_mut() {
                c.sharing = media.share;
            }
        });
        if self.room.is_none() {
            return;
        }
        self.voice(VoiceCommand::Muted(!self.media.mic));
        self.voice(VoiceCommand::Deafened(self.media.deafened));
        self.show_shares();
        let response = self.call.request(Request::SetMedia(media)).await;
        if refuses_share_start(&response, media.share, self.server_share) {
            self.undo_refused_share().await;
            return;
        }
        match response {
            Ok(Response::Ok) => self.server_share = media.share,
            other => self.refused(other),
        }
    }

    /// Stops a share the server wouldn't start. Leaving it running would carry the refused
    /// start in every later `SetMedia`, such as each push-to-talk press. A share the server
    /// still holds from before is stopped there too, which it never refuses.
    async fn undo_refused_share(&mut self) {
        self.stop_share();
        self.shared.update(|s| {
            if let Some(c) = s.current.as_mut() {
                c.sharing = None;
            }
        });
        self.show_shares();
        self.shared
            .notice("Sharing was started too often. Try again in a moment.");
        if self.server_share.is_some() {
            Box::pin(self.push_media()).await;
        }
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
            .watched_shares
            .iter()
            .filter(|(user, watched)| {
                let old = &watched.config;
                wanted.get(user).is_none_or(|(share, config)| {
                    share.video != watched.video
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
            if self.watched_shares.contains_key(&user) {
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
                    self.watched(share.audio, user, Carries::ShareAudio),
                );
                playout
            });
            let video = share.video;
            #[cfg(feature = "ffmpeg")]
            let decoder = {
                let units = self.call.engine.subscribe(video, Kind::Video, 64);
                let ask_idr = self.call.engine.idr_requester(video);
                match crate::media::share::receive(units, config, playout, ask_idr) {
                    Ok((picture, decoder)) => {
                        self.shared.set_picture(user, Some(picture));
                        Some(decoder)
                    }
                    Err(e) => {
                        self.shared.notice(format!("Can't show this share: {e}"));
                        None
                    }
                }
            };
            #[cfg(not(feature = "ffmpeg"))]
            let decoder = {
                drop(playout);
                self.shared
                    .notice("This build can't show video; it was built without ffmpeg.");
                None
            };
            let carries = Carries::ShareVideo { config, decoder };
            self.tracks
                .insert(video, self.watched(video, user, carries));
            let watched = WatchedShare {
                video,
                audio: config.audio.then_some(share.audio),
                config,
            };
            self.watched_shares.insert(user, watched);
        }
    }

    /// Health bookkeeping for `media_id`, counting from the engine's totals so far.
    fn watched(&self, media_id: MediaId, user: UserId, carries: Carries) -> Watched {
        let arrived = self.call.engine.arrived(media_id);
        Watched {
            user,
            carries,
            last_bytes: arrived.bytes,
            last_received: arrived.datagrams,
            last_lost: arrived.lost,
        }
    }

    fn stop_watching(&mut self, user: UserId) {
        if let Some(watched) = self.watched_shares.remove(&user) {
            self.call.engine.unsubscribe(watched.video);
            self.tracks.remove(&watched.video);
            if let Some(audio) = watched.audio {
                self.call.engine.unsubscribe(audio);
                self.tracks.remove(&audio);
                self.voice(VoiceCommand::ForgetShare(user));
            }
        }
        self.shared.set_picture(user, None);
    }

    fn stop_watching_all(&mut self) {
        let users: Vec<UserId> = self.watched_shares.keys().copied().collect();
        for user in users {
            self.stop_watching(user);
        }
    }

    /// Starts capturing for a share, unless one runs or is starting. The capture may wait on
    /// the user picking what to share, so it reports back through [`ShareStarts`].
    #[cfg(feature = "ffmpeg")]
    fn start_share(&mut self, wanted: ShareConfig) {
        if self.share_starts.wanted.is_some() {
            self.shared
                .notice("Already choosing what to share. Finish in the picker that's open.");
            return;
        }
        if self.sharing.is_some() {
            return;
        }
        let Some(tracks) = self
            .my_tracks
            .as_ref()
            .and_then(|t| t.shares.first())
            .copied()
        else {
            self.shared.notice("Join a room before sharing.");
            return;
        };
        let starts = &mut self.share_starts;
        starts.count += 1;
        starts.wanted = Some(starts.count);
        let number = starts.count;
        let done = starts.done.clone();
        let engine = self.call.engine.clone();
        tokio::spawn(async move {
            let started = tokio::task::spawn_blocking(move || {
                let config = crate::media::capture::CaptureConfig::for_share(&wanted);
                let capture = crate::media::capture::start(config)?;
                crate::media::share::ShareSender::start(engine, tracks, wanted, capture)
            })
            .await;
            if let Err(mpsc::error::SendError((_, started))) = done.send((number, started)).await {
                drop_unwanted_share(started);
            }
        });
    }

    /// Applies a share start that has finished, if it is still wanted.
    #[cfg(feature = "ffmpeg")]
    async fn share_started(&mut self, (number, started): StartedShare) {
        if self.share_starts.wanted != Some(number) {
            drop_unwanted_share(started);
            return;
        }
        self.share_starts.wanted = None;
        match started {
            Ok(Ok(sender)) => {
                self.media.share = Some(sender.config);
                self.shared
                    .set_picture(self.call.me(), Some(sender.self_view.clone()));
                *lock(&self.shared.preview) = Some((self.call.me(), sender.preview.clone()));
                info!(encoder = sender.encoder, "sharing");
                self.sharing = Some(sender);
                self.cue(Cue::ShareStart);
                self.push_media().await;
            }
            Ok(Err(e)) => self.shared.notice(format!("Couldn't start sharing: {e}")),
            Err(_) => self.shared.notice("Couldn't start sharing."),
        }
    }

    #[cfg(not(feature = "ffmpeg"))]
    fn start_share(&mut self, _wanted: ShareConfig) {
        self.shared
            .notice("This build can't share video; it was built without ffmpeg.");
    }

    #[cfg(not(feature = "ffmpeg"))]
    async fn share_started(&mut self, started: StartedShare) {
        match started {}
    }

    /// Stops our share, and any start of one.
    fn stop_share(&mut self) {
        self.media.share = None;
        #[cfg(feature = "ffmpeg")]
        {
            self.share_starts.wanted = None;
            if let Some(sender) = self.sharing.take() {
                tokio::task::spawn_blocking(move || drop(sender));
                self.shared.set_picture(self.call.me(), None);
                *lock(&self.shared.preview) = None;
            }
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
            self.end_share().await;
        }
    }

    /// Stops our share, with its cue if one ran, and tells the server.
    async fn end_share(&mut self) {
        if self.media.share.is_some() {
            self.cue(Cue::ShareStop);
        }
        self.stop_share();
        self.push_media().await;
    }

    /// The running share's codec, size, frame rate and encoder.
    fn sharing_detail(&self) -> Option<String> {
        #[cfg(feature = "ffmpeg")]
        return self
            .sharing
            .as_ref()
            .map(|sender| share_detail(&sender.config, sender.engine));
        #[cfg(not(feature = "ffmpeg"))]
        None
    }

    fn recover_share(&self, kind: Recovery) {
        #[cfg(feature = "ffmpeg")]
        if let Some(sender) = &self.sharing {
            sender.recover(kind);
        }
        #[cfg(not(feature = "ffmpeg"))]
        let _ = kind;
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
        let enc = match self.keystore.enc_key_pair() {
            Ok(enc) => enc,
            Err(e) => {
                warn!("the renewed encryption key is unusable: {e:#}");
                return;
            }
        };
        self.call.engine.keys().replace_enc(enc, Instant::now());
        let (server, me) = (self.call.server(), self.call.me());
        let published = account::publish_enc_key(&self.keystore, &self.call.requester, &server, me);
        match published.await {
            Ok(()) => info!("renewed the encryption key"),
            Err(e) => {
                warn!("publishing the renewed encryption key: {e}");
                self.call
                    .connection
                    .close(close::NORMAL.into(), b"encryption key renewed");
            }
        }
    }

    fn notices(&self, notices: Vec<KeyNotice>) {
        for notice in notices {
            match notice {
                KeyNotice::BadEncKey { user } => {
                    self.shared.notice(format!(
                        "User {}'s encryption key failed its check; they can't hear you.",
                        user.0
                    ));
                }
                KeyNotice::ClashingTracks => self.shared.notice(
                    "The server described the room with two tracks under one id, so the call is paused until it sends a valid one.",
                ),
                KeyNotice::TooManyPeers => self.shared.notice(format!(
                    "The server described the room with more than {MAX_ROOM_PEERS} people, so the call is paused until it sends a valid one."
                )),
                KeyNotice::OldEpoch => self.shared.notice(
                    "The server put this room back to keys older than ones you've used here, as a restored backup or a replayed call would. You won't hear anyone here until the room moves past them.",
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
            ServerEvent::ServerProfileChanged(profile) => {
                self.remember_server(profile.name.clone(), profile.avatar);
                self.shared.update(|s| {
                    if let Some(server) = s.server.as_mut() {
                        server.name = profile.name.as_deref().and_then(shown_name);
                        server.avatar = profile.avatar;
                    }
                });
            }
            ServerEvent::PresenceChanged(p) => {
                self.room_cue(p.user, p.room);
                let (verified, identity_key) = self.trust.of(p.user);
                let saved = &self.saved_playback;
                let sharing = self.sharing_of(p.user);
                self.shared.update(|s| {
                    let speaking = s.people.get(&p.user).is_some_and(|old| old.speaking);
                    let updated = Person {
                        verified,
                        identity_key,
                        speaking,
                        sharing,
                        ..person(&p, saved)
                    };
                    s.add_person(p.user, updated);
                });
                self.refresh_trust();
                self.follow_room().await;
            }
            ServerEvent::PresenceEnded(user) => {
                self.room_cue(user, None);
                self.shared.update(|s| {
                    s.people.remove(&user);
                });
                self.follow_room().await;
            }
            ServerEvent::RoomCreated(room) => self.shared.update(|s| s.add_room((&room).into())),
            ServerEvent::RoomLocked { room, locked } => self.shared.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.locked = locked;
                }
            }),
            ServerEvent::RoomRenamed { room, name } => self.shared.update(|s| {
                if let Some(r) = s.room_mut(room) {
                    r.name = room_name(room, &name);
                }
            }),
            ServerEvent::RoomDeleted(room) => {
                if self.room == Some(room) {
                    self.put_out_of_room();
                    self.shared.notice("The room you were in was deleted.");
                }
                self.shared.update(|s| s.rooms.retain(|r| r.id != room));
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
            ServerEvent::KickedFromRoom { by, room } if self.room == Some(room) => {
                self.put_out_of_room();
                let state = self.shared.state();
                let by = state.person(by).map_or("Someone", |p| p.name.as_str());
                let room = state.room(room).map_or("the room", |r| r.name.as_str());
                self.shared.notice(format!("{by} moved you out of {room}."));
            }
            ServerEvent::KickedFromRoom { .. } => {}
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
        let speaking = |user: &UserId| {
            if *user == me {
                levels.transmitting
            } else {
                levels.speaking.get(user).copied().unwrap_or(false)
            }
        };
        let state = self.shared.state();
        if state.people.iter().all(|(u, p)| p.speaking == speaking(u)) {
            return;
        }
        self.shared.update(|s| {
            for (user, person) in s.people.iter_mut() {
                person.speaking = speaking(user);
            }
        });
    }

    fn refresh_health(&mut self) {
        let connection = &self.call.connection;
        let stats = connection.stats();
        let upload_loss = percent(
            advance(&mut self.last.lost_sent, stats.path.lost_packets),
            advance(&mut self.last.sent, stats.path.sent_packets),
        );
        let local_drops = self
            .call
            .engine
            .stats
            .send_dropped_units
            .load(Ordering::Relaxed);
        let new_local_drops = advance(&mut self.last.local_drops, local_drops);

        let state = self.shared.state();
        let mut rows = Vec::new();
        let (mut received_total, mut lost_total) = (0, 0);
        for (media_id, track) in self.tracks.iter_mut() {
            let arrived = self.call.engine.arrived(*media_id);
            let new_bytes = advance(&mut track.last_bytes, arrived.bytes);
            let new_received = advance(&mut track.last_received, arrived.datagrams);
            let new_lost = advance(&mut track.last_lost, arrived.lost);
            received_total += new_received;
            lost_total += new_lost;
            let name = state
                .person(track.user)
                .map_or("someone".to_owned(), |p| p.name.clone());
            rows.push(TrackHealth {
                label: format!("{name}, {}", track.carries.label()),
                detail: track.carries.detail(),
                set_kbps: Some(track.carries.set_kbps()),
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
            download_kbps: kbps(advance(&mut self.last.udp_received, stats.udp_rx.bytes)),
            upload_kbps: kbps(advance(&mut self.last.udp_sent, stats.udp_tx.bytes)),
            upload_loss,
            download_loss,
            dropped_for_you: dropped,
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
            let kbps = self.options.voice.kbps;
            wanted.push(("mic", VOICE_DETAIL.into(), tracks.mic, kbps, None));
        }
        if let (Some(config), Some(share)) = (self.media.share, tracks.shares.first()) {
            let detail = self
                .sharing_detail()
                .unwrap_or_else(|| share_detail(&config, "no encoder"));
            wanted.push((
                "screen",
                detail,
                share.video,
                config.kbps,
                Some(new_local_drops),
            ));
            if config.audio {
                let detail = SHARE_AUDIO_DETAIL.into();
                wanted.push(("screen audio", detail, share.audio, SHARE_AUDIO_KBPS, None));
            }
        }
        wanted
            .into_iter()
            .map(|(label, detail, media_id, set_kbps, dropped)| {
                let bytes = self.call.engine.sent_bytes(media_id);
                let last = self.last.sent_bytes.entry(media_id).or_insert(bytes);
                SentTrack {
                    label,
                    detail,
                    set_kbps,
                    leaving_kbps: kbps(advance(last, bytes)),
                    dropped,
                }
            })
            .collect()
    }
}

/// Drops a share that started after it was no longer wanted, off the async workers, since
/// stopping it joins its threads.
#[cfg(feature = "ffmpeg")]
fn drop_unwanted_share(started: StartResult) {
    if let Ok(Ok(sender)) = started {
        tokio::task::spawn_blocking(move || drop(sender));
    }
}

/// `users` with their names as shown.
fn shown_users(users: Vec<User>) -> Vec<User> {
    users
        .into_iter()
        .map(|user| User {
            name: person_name(user.id, &user.name),
            ..user
        })
        .collect()
}

/// `p` as a person, playing as loud as `saved` keeps them.
fn person(p: &Presence, saved: &SavedPlayback) -> Person {
    Person {
        name: person_name(p.user, &p.name),
        avatar: p.avatar,
        room: p.room,
        mic: p.mic,
        deafened: p.deafened,
        sharing: None,
        speaking: false,
        playback: saved.person(p.user),
        verified: false,
        identity_key: None,
        role: p.role,
    }
}

/// Why the server didn't answer `Ok`, in its own words where it gave some.
fn refusal(response: anyhow::Result<Response>) -> String {
    response.map_or_else(|e| e.to_string(), control::refusal)
}

/// Whether `response` to a `SetMedia` carrying `sent` refused to start that share. The server
/// still applies the mic and deafen then, and keeps the share it had (`on_server`).
fn refuses_share_start(
    response: &anyhow::Result<Response>,
    sent: Option<ShareConfig>,
    on_server: Option<ShareConfig>,
) -> bool {
    let rate_limited = matches!(
        response,
        Ok(Response::Err {
            code: ErrorCode::RateLimited,
            ..
        })
    );
    rate_limited && sent.is_some() && sent != on_server
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

/// The cue for someone starting or stopping a share, if they did.
fn share_cue(was_sharing: bool, sharing: bool) -> Option<Cue> {
    match (was_sharing, sharing) {
        (false, true) => Some(Cue::ShareStart),
        (true, false) => Some(Cue::ShareStop),
        _ => None,
    }
}

/// The cue for turning the mic `on`, which also undeafens, if it changes anything.
fn mic_cue(media: &MediaState, on: bool) -> Option<Cue> {
    match on {
        _ if on == media.mic => None,
        true if media.deafened => Some(Cue::Undeafen),
        true => Some(Cue::Unmute),
        false => Some(Cue::Mute),
    }
}

/// Deafens, which also mutes, or undeafens back to the mic state from before.
fn set_deafened(media: &mut MediaState, mic_before: &mut bool, deafened: bool) {
    if deafened == media.deafened {
        return;
    }
    if deafened {
        *mic_before = media.mic;
        media.mic = false;
    } else {
        media.mic = *mic_before;
    }
    media.deafened = deafened;
}

fn gain_command(user: UserId, sound: Sound, playback: Playback) -> VoiceCommand {
    match sound {
        Sound::Voice => VoiceCommand::Volume(user, playback.gain()),
        Sound::Share => VoiceCommand::ShareVolume(user, playback.gain()),
    }
}

/// Whether `newer` sets the same thing as `older`, so only `newer` needs replaying.
fn supersedes(newer: &Command, older: &Command) -> bool {
    use std::mem::discriminant;
    match (newer, older) {
        (Command::SetPlayback(a, sa, _), Command::SetPlayback(b, sb, _)) => a == b && sa == sb,
        (
            Command::SetMic(_)
            | Command::SetDeafened(_)
            | Command::PushToTalk(_)
            | Command::MicTest(_)
            | Command::Voice(_)
            | Command::Devices { .. },
            _,
        ) => discriminant(newer) == discriminant(older),
        _ => false,
    }
}

/// `backoff` scaled by a factor spread evenly over 1 ± `BACKOFF_JITTER`, picked by `random`.
/// The mean wait stays `backoff`.
fn jittered(backoff: Duration, random: u64) -> Duration {
    let unit = (random >> 11) as f64 / (1u64 << 53) as f64;
    backoff.mul_f64(1.0 - BACKOFF_JITTER + 2.0 * BACKOFF_JITTER * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_rate_limited_share_start_is_undone() {
        let share = Some(ShareConfig {
            codec: throcc_proto::messages::Codec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            kbps: 8000,
            audio: true,
        });
        let limited = || {
            Ok(Response::Err {
                code: ErrorCode::RateLimited,
                message: "too many share starts".into(),
            })
        };
        assert!(refuses_share_start(&limited(), share, None));
        // A push-to-talk press while the server already has our share.
        assert!(!refuses_share_start(&limited(), share, share));
        assert!(!refuses_share_start(&limited(), None, None));
        assert!(!refuses_share_start(&Ok(Response::Ok), share, None));
        let forbidden = Ok(Response::Err {
            code: ErrorCode::Forbidden,
            message: "no".into(),
        });
        assert!(!refuses_share_start(&forbidden, share, None));
        assert!(!refuses_share_start(
            &Err(anyhow::anyhow!("timed out")),
            share,
            None
        ));
    }

    #[test]
    fn reconnect_waits_are_jittered_within_bounds() {
        for backoff in [Duration::from_secs(1), MAX_BACKOFF] {
            let (low, high) = (backoff.mul_f64(0.7), backoff.mul_f64(1.3));
            let close = |a: Duration, b: Duration| a.abs_diff(b) < Duration::from_micros(1);
            assert!(close(jittered(backoff, 0), low));
            assert!(close(jittered(backoff, u64::MAX), high));
            assert!(close(jittered(backoff, 1 << 63), backoff));
            let waits: Vec<Duration> = (0..64)
                .map(|_| jittered(backoff, u64::from_le_bytes(random())))
                .collect();
            assert!(
                waits
                    .iter()
                    .all(|w| (low..=high).contains(w) || close(*w, low) || close(*w, high)),
                "{waits:?}"
            );
            assert!(waits.iter().any(|w| *w != waits[0]), "{waits:?}");
        }
    }

    #[test]
    fn pictures_nothing_shown_uses_are_dropped_once_many_are_kept() {
        let shown = Hash::of(b"shown");
        let shared = Shared::new(State {
            profile_picture: Some(shown),
            ..State::default()
        });
        let ready = || AvatarSlot::Ready(Arc::from(&b"png"[..]));
        shared.set_avatar(shown, ready());
        let fetching = Hash::of(b"fetching");
        shared.avatar(fetching);
        for i in 0..AVATARS_KEPT as u32 {
            shared.set_avatar(Hash::of(&i.to_be_bytes()), ready());
        }
        let kept = lock(&shared.avatars).len();
        assert!(kept < AVATARS_KEPT, "{kept} kept");
        assert!(shared.avatar(shown).0.is_some());
        assert!(matches!(
            lock(&shared.avatars).get(&fetching),
            Some(AvatarSlot::Fetching)
        ));
    }

    #[test]
    fn only_the_latest_setting_is_replayed() {
        use crate::client::Command;
        assert!(supersedes(
            &Command::PushToTalk(false),
            &Command::PushToTalk(true)
        ));
        assert!(supersedes(&Command::SetMic(false), &Command::SetMic(true)));
        assert!(!supersedes(
            &Command::SetMic(false),
            &Command::SetDeafened(true)
        ));
        let volume = |user, sound, v| {
            let playback = Playback {
                volume: v,
                muted: false,
            };
            Command::SetPlayback(UserId(user), sound, playback)
        };
        assert!(supersedes(
            &volume(1, Sound::Voice, 0.5),
            &volume(1, Sound::Voice, 1.0)
        ));
        assert!(!supersedes(
            &volume(1, Sound::Voice, 0.5),
            &volume(2, Sound::Voice, 1.0)
        ));
        assert!(!supersedes(
            &volume(1, Sound::Share, 0.5),
            &volume(1, Sound::Voice, 1.0)
        ));
        assert!(!supersedes(
            &Command::Watch(UserId(1)),
            &Command::Watch(UserId(2))
        ));
    }

    #[test]
    fn undeafening_restores_the_mic_from_before() {
        let mut media = MediaState {
            mic: true,
            ..MediaState::default()
        };
        let mut before = false;
        set_deafened(&mut media, &mut before, true);
        assert!(media.deafened && !media.mic);
        set_deafened(&mut media, &mut before, false);
        assert!(!media.deafened && media.mic);

        media.mic = false;
        set_deafened(&mut media, &mut before, true);
        set_deafened(&mut media, &mut before, false);
        assert!(!media.mic);
    }

    #[test]
    fn turning_the_mic_on_while_deafened_sounds_like_undeafening() {
        let mut media = MediaState::default();
        assert_eq!(mic_cue(&media, true), Some(Cue::Unmute));
        assert_eq!(mic_cue(&media, false), None);
        set_deafened(&mut media, &mut false, true);
        assert_eq!(mic_cue(&media, true), Some(Cue::Undeafen));
        media.mic = true;
        media.deafened = false;
        assert_eq!(mic_cue(&media, false), Some(Cue::Mute));
    }
}
