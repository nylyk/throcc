use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use throcc_crypto::identity::random;
use throcc_proto::ids::{Hash, MediaId, RoomId, UserId};
use throcc_proto::limits::{MAX_ROOM_MEMBERS, MAX_ROOM_SHARES};
use throcc_proto::messages::{
    Audiences, AuthError, ErrorCode, Event as ServerEvent, MediaState, Peer, Placement, Presence,
    PublicKey, Recovery, Request, Response, ShareConfig, ShareTracks, Tracks, User, UserProfile,
    Welcome,
};
use throcc_proto::transport::close;
use tokio::sync::{Semaphore, broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::account::{self, SignInError};
use crate::avatars;
use crate::call::{Call, CallEvent};
use crate::client::{UiCommand, UiEvent};
use crate::control::{self, LoginError};
use crate::keys::KeyNotice;
use crate::keystore::Keystore;
use crate::lock;
use crate::media::cues::Cue;
use crate::media::engine::Kind;
use crate::media::share_audio::Playout;
use crate::media::video::{Picture, PreviewDemand};
use crate::media::voice::{AudioEngine, CallAudio, CallCommand};
use crate::playback::SavedPlayback;
use crate::profile::Profile;
use crate::server_look::ServerLook;
use crate::state::{
    ConnectionStatus, CurrentRoom, Health, KeyWarning, MIC_KBPS, Person, Playback,
    SHARE_AUDIO_DETAIL, SHARE_AUDIO_KBPS, SentTrack, ServerView, Sound, SoundLevel, State,
    TrackHealth, VOICE_DETAIL, person_name, room_name, share_detail, shown_name,
};
use crate::trust::Trust;
use crate::verified::VerifiedStore;

const LEVELS_EVERY: Duration = Duration::from_millis(100);
const HEALTH_EVERY: Duration = Duration::from_secs(2);
/// The shortest time between two cues for others joining or leaving an audience.
const AUDIENCE_CUE_GAP: Duration = Duration::from_secs(1);
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
    pub events: broadcast::Sender<UiEvent>,
    pub pictures: Mutex<HashMap<UserId, Picture>>,
    /// Our own user and the preview demand of our running share.
    pub preview: Mutex<Option<(UserId, Arc<PreviewDemand>)>>,
    pub avatars: Mutex<HashMap<Hash, AvatarSlot>>,
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
            self.emit(UiEvent::AvatarReady(hash));
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
            self.events.send(UiEvent::State(guard.clone())).ok();
        }
    }

    pub fn emit(&self, event: UiEvent) {
        self.events.send(event).ok();
    }

    pub fn notice(&self, text: impl Into<String>) {
        self.emit(UiEvent::Notice(text.into()));
    }
}

/// One subscribed remote track, with counters from the last health refresh.
struct Watched {
    user: UserId,
    carries: Carries,
    last_bytes: u64,
    last_received: u64,
    last_lost: u64,
    /// How many server reports in a row said it dropped units of this track for us.
    reports_with_drops: u32,
}

impl Watched {
    fn server_reported(&mut self, dropped_units: u32) {
        self.reports_with_drops = if dropped_units > 0 {
            self.reports_with_drops.saturating_add(1)
        } else {
            0
        };
    }

    /// Whether the track is too slow for our connection: the server dropped units of it for
    /// us in its last two reports.
    fn too_slow(&self) -> bool {
        self.reports_with_drops >= 2
    }
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
    call: Call,
    audio: Option<CallAudio>,
    room: Option<RoomId>,
    my_tracks: Option<throcc_proto::messages::Tracks>,
    media: MediaState,
    /// The share the server last took from us. A share-start refusal leaves it in place.
    server_share: Option<ShareConfig>,
    /// The peers our key is wrapped to, whose tracks we follow.
    peers: BTreeMap<UserId, Peer>,
    trust: Trust,
    watching: Vec<UserId>,
    /// Who watches each watched share in our room, by its video.
    audiences: Audiences,
    last_audience_cue: Option<Instant>,
    /// The shares we receive, by who shares them.
    watched_shares: HashMap<UserId, WatchedShare>,
    /// Whether the mic was on when we deafened, so undeafening restores it.
    mic_before_deafen: bool,
    profile: Profile,
    /// Our profile as the server has it.
    server_profile: UserProfile,
    /// Our profile as the server refused it for coming too soon after the last change. Its
    /// picture is uploaded, and the health refresh sets it again.
    too_soon_profile: Option<UserProfile>,
    saved_playback: SavedPlayback,
    /// Each sound set for people who presented no identity key yet, saved under the key
    /// once they do.
    unkeyed_playback: HashMap<(UserId, Sound), SoundLevel>,
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
        profile: Profile,
        audio: Option<Arc<AudioEngine>>,
        call: Call,
        verified: VerifiedStore,
    ) -> Self {
        let server = call.server();
        let saved_playback = SavedPlayback::load(keystore.dir()).unwrap_or_else(|e| {
            warn!("no saved playback: {e:#}");
            SavedPlayback::empty(keystore.dir())
        });
        let server_look = ServerLook::load(keystore.dir(), &server).unwrap_or_else(|e| {
            warn!("no kept server look: {e:#}");
            ServerLook::default()
        });
        let mut session = Self {
            shared,
            keystore,
            address,
            audio: audio.map(|engine| engine.call()),
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
            audiences: Audiences::default(),
            last_audience_cue: None,
            watched_shares: HashMap::new(),
            mic_before_deafen: false,
            server_profile: call.welcome.profile.clone(),
            too_soon_profile: None,
            profile,
            saved_playback,
            unkeyed_playback: HashMap::new(),
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
        let welcome = session.call.welcome.clone();
        session.apply_welcome(&welcome);
        session
    }

    /// Gives the server our profile, uploading the picture only if the server lacks it, and
    /// tells the UI how that went.
    async fn push_profile(&mut self) {
        self.too_soon_profile = None;
        let wanted = UserProfile {
            name: self.profile.name.clone(),
            avatar: self.profile.avatar(),
        };
        if wanted == self.server_profile {
            return self.shared.emit(UiEvent::ProfileSaved);
        }
        if let Some(bytes) = &self.profile.picture
            && wanted.avatar != self.server_profile.avatar
        {
            match self
                .call
                .request(Request::UploadAvatar(bytes.to_vec()))
                .await
            {
                Ok(Response::AvatarHash(hash)) if Some(hash) == wanted.avatar => {}
                other => return self.profile_refused(other),
            }
        }
        self.set_profile(wanted).await;
    }

    /// Puts `wanted`, whose picture the server holds, on our profile and tells the UI how that
    /// went. A refusal for coming too soon tells it nothing until a later try settles it.
    async fn set_profile(&mut self, wanted: UserProfile) {
        let request = Request::SetProfile {
            name: wanted.name.clone(),
            avatar: wanted.avatar,
        };
        match self.call.request(request).await {
            Ok(Response::Ok) => {
                self.server_profile = wanted;
                self.shared.emit(UiEvent::ProfileSaved);
            }
            Ok(Response::Err {
                code: ErrorCode::RateLimited,
                ..
            }) => self.too_soon_profile = Some(wanted),
            other => self.profile_refused(other),
        }
    }

    /// Tells the UI the server refused our profile. A push the closing connection cut off
    /// tells it nothing, since the session pushes again once reconnected.
    fn profile_refused(&self, response: anyhow::Result<Response>) {
        if self.call.connection.close_reason().is_none() {
            self.shared.emit(UiEvent::ProfileRefused(refusal(response)));
        }
    }

    /// Tells the call audio our switches and the track our mic goes out on.
    fn sync_audio(&self) {
        self.audio(CallCommand::Muted(!self.media.mic));
        self.audio(CallCommand::Deafened(self.media.deafened));
        let mic = self
            .my_tracks
            .as_ref()
            .map(|t| (self.call.engine.clone(), t.mic));
        self.audio(CallCommand::SendOn(mic));
    }

    fn audio(&self, command: CallCommand) {
        if let Some(audio) = &self.audio {
            audio.command(command);
        }
    }

    fn cue(&self, cue: Cue) {
        if let Some(audio) = &self.audio {
            audio.cue(cue);
        }
    }

    /// Plays `cue` for us joining or leaving `user`'s share, while they share.
    fn stream_cue(&self, user: UserId, cue: Cue) {
        if user != self.call.me() && self.sharing_of(user).is_some() {
            self.cue(cue);
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

    pub async fn run(mut self, commands: mpsc::Receiver<UiCommand>) {
        self.run_until_done(commands).await;
        if let Err(e) = self.call.floors.save() {
            warn!("saving epoch floors: {e:#}");
        }
    }

    async fn run_until_done(&mut self, mut commands: mpsc::Receiver<UiCommand>) {
        self.push_profile().await;
        let mut levels = tokio::time::interval(LEVELS_EVERY);
        let mut health = tokio::time::interval(HEALTH_EVERY);
        let mut renewal = tokio::time::interval(RENEWAL_CHECK_EVERY);
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let command = command.unwrap_or(UiCommand::Disconnect);
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
                        self.push_profile().await;
                    }
                },
                _ = levels.tick() => {
                    self.refresh_levels();
                    self.stop_ended_share().await;
                }
                _ = health.tick() => {
                    self.refresh_health();
                    if let Some(wanted) = self.too_soon_profile.take() {
                        self.set_profile(wanted).await;
                    }
                    let floors = self.call.floors.clone();
                    let saved = tokio::task::spawn_blocking(move || floors.save())
                        .await
                        .unwrap_or_else(|e| Err(e.into()));
                    if let Err(e) = saved {
                        warn!("saving epoch floors: {e:#}");
                    }
                    if let Err(e) = self.saved_playback.save() {
                        warn!("saving playback: {e:#}");
                    }
                }
                _ = renewal.tick() => self.renew_encryption_key().await,
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
        if let Err(e) = self.keystore.known_servers.leave(&self.address) {
            warn!("forgetting the server: {e:#}");
        }
        self.call.close().await;
        self.shared
            .update(|s| s.connection = ConnectionStatus::Disconnected);
        self.shared.emit(UiEvent::LeftServer);
        true
    }

    /// Reconnects with backoff, into the room last asked for, or else the same room.
    /// Returns the other commands that arrived meanwhile, or `None` if told to stop or the
    /// server won't take us back.
    async fn reconnect(
        &mut self,
        commands: &mut mpsc::Receiver<UiCommand>,
    ) -> Option<Vec<UiCommand>> {
        let lost = self
            .call
            .connection
            .close_reason()
            .map_or("connection lost".to_owned(), |e| e.to_string());
        let mut reason = lost.clone();
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
                        None | Some(UiCommand::Disconnect) => {
                            self.shared.update(|s| s.connection = ConnectionStatus::Disconnected);
                            return None;
                        }
                        Some(UiCommand::Join(room)) => want_room = room,
                        // Asked again on the next render once connected.
                        Some(UiCommand::FetchAvatar(hash)) => self.shared.forget_avatar(hash),
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
            if let Err(e) = self.keystore.renew_encryption_key_if_due() {
                warn!("renewing the encryption key: {e:#}");
            }
            let link = match account::sign_in(&self.keystore, &self.address, want_room).await {
                Ok(link) => link,
                Err(e) => {
                    let fatal = match &e {
                        SignInError::Login(LoginError::Refused(reason))
                            if !matches!(
                                reason,
                                AuthError::RateLimited | AuthError::ServerFull
                            ) =>
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
                    reason = match e {
                        SignInError::Login(LoginError::Refused(AuthError::ServerFull)) => {
                            "the server is full".to_owned()
                        }
                        _ => lost.clone(),
                    };
                    info!("reconnect attempt {attempt} failed: {e}");
                    continue;
                }
            };
            match Call::restart(link, &self.keystore, &self.call) {
                Ok(call) => {
                    self.call = call;
                    self.trust.forget_presented();
                    self.reset_room();
                    self.audio = self.audio.as_ref().map(CallAudio::renew);
                    self.cue(Cue::Reconnected);
                    let welcome = self.call.welcome.clone();
                    let wanted = want_room.map(|room| (room, self.room_name(room)));
                    self.apply_welcome(&welcome);
                    if let Some((room, name)) = wanted
                        && welcome.placement.room != Some(room)
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
        self.call.engine.apply_placement(&Placement {
            room: None,
            epoch: throcc_proto::ids::Epoch(0),
            tracks: None,
            peers: vec![],
            audiences: Audiences::default(),
        });
        self.reset_room();
        self.audiences.shares.clear();
        self.audio(CallCommand::ForgetAll);
        self.audio(CallCommand::SendOn(None));
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
        // A new connection counts audiences afresh.
        self.audiences = Audiences::default();
        let fingerprint = self.call.server();
        self.server_profile = welcome.profile.clone();
        self.remember_server(welcome.server.name.clone(), welcome.server.avatar);
        let address = self.address.clone();
        let media = self.media;
        let playbacks: Vec<Playback> = welcome
            .presence
            .iter()
            .map(|p| self.playback_of(p.user))
            .collect();
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
            for (p, playback) in welcome.presence.iter().zip(playbacks) {
                s.add_person(p.user, person(p, playback));
            }
            s.current = None;
        });
        self.refresh_trust();
        if welcome.placement.room.is_some() {
            self.after_join(&welcome.placement);
        }
    }

    async fn joined(&mut self, placement: &Placement, notices: Vec<KeyNotice>) {
        self.after_join(placement);
        self.cue(if placement.room.is_some() {
            Cue::Join
        } else {
            Cue::Leave
        });
        self.notices(notices);
        self.push_media().await;
    }

    fn after_join(&mut self, placement: &Placement) {
        // The floor of a new placement is what a replaying server wants lost, so it isn't
        // left to the next health tick.
        if let Err(e) = self.call.floors.save() {
            warn!("saving epoch floors: {e:#}");
        }
        self.reset_room();
        self.room = placement.room;
        self.my_tracks = placement.tracks.clone();
        if placement.room.is_some() {
            self.take_newer(placement.audiences.clone());
        } else {
            self.audiences.shares.clear();
        }
        self.audio(CallCommand::ForgetAll);
        self.sync_audio();
        self.sync_peers();
        let room = placement.room;
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
        let current: BTreeMap<UserId, Peer> = trusted.into_iter().map(|p| (p.user, p)).collect();
        let changed = current != self.peers;
        self.note_identities(&current);
        let arrived: Vec<UserId> = current
            .iter()
            .filter(|(user, peer)| {
                self.peers
                    .get(user)
                    .is_none_or(|old| old.identity_key != peer.identity_key)
            })
            .map(|(user, _)| *user)
            .collect();
        self.play_as_kept(arrived);
        for (user, old) in &self.peers {
            if current
                .get(user)
                .is_none_or(|p| p.tracks.mic != old.tracks.mic)
            {
                self.call.engine.unsubscribe(old.tracks.mic);
                self.tracks.remove(&old.tracks.mic);
                self.audio(CallCommand::Forget(*user));
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
                self.audio(CallCommand::Listen { user: *user, units });
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
        changed
    }

    /// Notes the identity keys `peers` present, saves playback set before under them, then
    /// shows who is verified and whose verified key changed.
    fn note_identities(&mut self, peers: &BTreeMap<UserId, Peer>) {
        let presented: Vec<(UserId, PublicKey)> = peers
            .iter()
            .map(|(user, peer)| (*user, peer.identity_key))
            .collect();
        let noted = self.trust.note(&presented);
        for (user, key) in &presented {
            for sound in Sound::ALL {
                if let Some(playback) = self.unkeyed_playback.remove(&(*user, sound)) {
                    self.saved_playback.set(key, sound, playback);
                }
            }
        }
        if noted {
            self.refresh_trust();
        }
    }

    /// How `user` plays: each sound as set before they presented an identity key, else as
    /// saved for the key they presented.
    fn playback_of(&self, user: UserId) -> Playback {
        let mut playback = self
            .trust
            .of(user)
            .1
            .map_or_else(Playback::default, |key| self.saved_playback.person(&key));
        for sound in Sound::ALL {
            if let Some(set) = self.unkeyed_playback.get(&(user, sound)) {
                *playback.get_mut(sound) = *set;
            }
        }
        playback
    }

    /// Plays each of `users` as [`Self::playback_of`] says, and shows it.
    fn play_as_kept(&mut self, users: impl IntoIterator<Item = UserId>) {
        let kept: Vec<(UserId, Playback)> = users
            .into_iter()
            .map(|user| (user, self.playback_of(user)))
            .collect();
        if kept.is_empty() {
            return;
        }
        for (user, playback) in &kept {
            for sound in Sound::ALL {
                self.audio(gain_command(*user, sound, playback.get(sound)));
            }
        }
        self.shared.update(|s| {
            for (user, playback) in &kept {
                if let Some(p) = s.people.get_mut(user) {
                    p.playback = *playback;
                }
            }
        });
    }

    /// Follows a change in who we share keys with, and in the shares we watch from them.
    async fn follow_room(&mut self) {
        if self.sync_peers() && !self.watching.is_empty() {
            self.push_watching().await;
        }
    }

    /// Shows who is verified and whose verified key changed.
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
        self.shared.update(|s| {
            for (user, person) in &mut s.people {
                if let Some(&(verified, key)) = trust.get(user) {
                    (person.verified, person.identity_key) = (verified, key);
                }
            }
            s.key_warnings = warnings;
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
            self.shared.notice(if self.trust.of(user).1.is_some() {
                format!(
                    "{name}'s key changed before you confirmed, so nothing changed. Compare the new safety number."
                )
            } else {
                format!(
                    "{name} isn't presenting a key right now, so nothing changed. Compare the safety number again once they're in a room with you."
                )
            });
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
                person.audience = self.audience_of(*user);
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

    fn tracks_of(&self, user: UserId) -> Option<&Tracks> {
        if user == self.call.me() {
            self.my_tracks.as_ref()
        } else {
            self.peers.get(&user).map(|p| &p.tracks)
        }
    }

    fn audience_of(&self, user: UserId) -> usize {
        self.tracks_of(user)
            .and_then(|t| t.shares.first())
            .and_then(|share| receivers_of(&self.audiences, share.video))
            .map_or(0, <[UserId]>::len)
    }

    /// Takes `audiences` unless it is older than the one we hold.
    fn take_newer(&mut self, audiences: Audiences) -> bool {
        let Some(audiences) = newer_audiences(&self.audiences, audiences) else {
            return false;
        };
        self.audiences = audiences;
        true
    }

    /// Notes who receives the shares in our room. Plays a join or leave when someone joins or
    /// leaves the audience of our live share, at most once per [`AUDIENCE_CUE_GAP`].
    fn audiences_changed(&mut self, audiences: Audiences) {
        let ours = self
            .media
            .share
            .and(self.my_tracks.as_ref())
            .and_then(|t| t.shares.first())
            .map(|share| share.video);
        let before = ours
            .and_then(|video| receivers_of(&self.audiences, video))
            .unwrap_or_default()
            .to_vec();
        if !self.take_newer(audiences) {
            return;
        }
        if let Some(video) = ours {
            let room = self.room;
            let state = self.shared.state();
            let still_here: Vec<UserId> = before
                .into_iter()
                .filter(|user| state.person(*user).is_some_and(|p| p.room == room))
                .collect();
            let after = receivers_of(&self.audiences, video).unwrap_or_default();
            if let Some(cue) = audience_cue(&still_here, after)
                && self
                    .last_audience_cue
                    .is_none_or(|at| at.elapsed() >= AUDIENCE_CUE_GAP)
            {
                self.last_audience_cue = Some(Instant::now());
                self.cue(cue);
            }
        }
        self.show_shares();
    }

    async fn command(&mut self, command: UiCommand) -> ControlFlow<()> {
        match command {
            UiCommand::Join(room) => match self.call.join(room, None).await {
                Ok((placement, notices)) => self.joined(&placement, notices).await,
                Err(e) => self.shared.notice(format!("Couldn't join: {e}")),
            },
            UiCommand::JoinWithPassword(room, password) => {
                match self.call.join(Some(room), Some(password)).await {
                    Ok((placement, notices)) => self.joined(&placement, notices).await,
                    Err(e) => self.shared.emit(UiEvent::PasswordRefused(e.to_string())),
                }
            }
            UiCommand::SetMic(on) => {
                if let Some(cue) = mic_cue(&self.media, on) {
                    self.cue(cue);
                }
                self.media.mic = on;
                if on {
                    self.media.deafened = false;
                }
                self.push_media().await;
            }
            UiCommand::SetDeafened(deafened) => {
                if deafened != self.media.deafened {
                    self.cue(if deafened { Cue::Deafen } else { Cue::Undeafen });
                }
                set_deafened(&mut self.media, &mut self.mic_before_deafen, deafened);
                self.push_media().await;
            }
            UiCommand::StartShare(config) => self.start_share(config),
            UiCommand::StopShare => self.end_share().await,
            UiCommand::Watch(user) => {
                if !self.watching.contains(&user) {
                    self.watching.push(user);
                    self.stream_cue(user, Cue::StreamJoin);
                }
                self.push_watching().await;
            }
            UiCommand::Unwatch(user) => {
                if self.watching.contains(&user) {
                    self.watching.retain(|u| *u != user);
                    self.stream_cue(user, Cue::StreamLeave);
                }
                self.push_watching().await;
            }
            UiCommand::SetSoundLevel(user, sound, level) => {
                self.audio(gain_command(user, sound, level));
                match self.trust.of(user).1 {
                    Some(key) => self.saved_playback.set(&key, sound, level),
                    None => {
                        self.unkeyed_playback.insert((user, sound), level);
                    }
                }
                self.shared.update(|s| {
                    if let Some(p) = s.people.get_mut(&user) {
                        *p.playback.get_mut(sound) = level;
                    }
                });
            }
            UiCommand::SetProfile(profile) => {
                self.profile = profile;
                self.push_profile().await;
            }
            UiCommand::FetchAvatar(hash) => self.fetch_avatar(hash),
            UiCommand::SetServerProfile { name, picture } => {
                let response = self.set_server_profile(name, picture).await;
                self.shared.emit(match response {
                    Ok(Response::Ok) => UiEvent::ServerProfileSaved,
                    other => UiEvent::ServerProfileRefused(refusal(other)),
                });
            }
            UiCommand::CreateRoom(name, password) => {
                self.simple(Request::CreateRoom { name, password }).await
            }
            UiCommand::RenameRoom(room, name) => {
                self.simple(Request::RenameRoom { room, name }).await
            }
            UiCommand::SetRoomPassword(room, password) => {
                self.simple(Request::SetRoomPassword { room, password })
                    .await
            }
            UiCommand::DeleteRoom(room) => self.simple(Request::DeleteRoom(room)).await,
            UiCommand::KickFromRoom { user, room } => {
                self.simple(Request::KickFromRoom { user, room }).await
            }
            UiCommand::CreateInvite => match self.call.request(Request::CreateInvite).await {
                Ok(Response::Invite { code, expires }) => {
                    self.shared.emit(UiEvent::Invite { code, expires })
                }
                other => self.refused(other),
            },
            UiCommand::ListUsers => match self.call.request(Request::ListUsers).await {
                Ok(Response::Users(users)) => self.shared.emit(UiEvent::Users(shown_users(users))),
                other => self.refused(other),
            },
            UiCommand::SetRole(user, role) => {
                self.simple(Request::SetRole { user, role }).await;
                self.list_users().await;
            }
            UiCommand::RemoveUser { user, cascade } => {
                self.simple(Request::RemoveUser { user, cascade }).await;
                self.list_users().await;
            }
            UiCommand::SetVerified {
                user,
                key,
                verified,
            } => self.set_verified(user, &key.0, verified),
            UiCommand::DismissKeyChange { user, shown } => {
                let shown = shown.map(|key| key.0);
                match self.trust.dismiss_change(user, shown.as_ref()) {
                    Ok(false) => {
                        let name = self
                            .shared
                            .state()
                            .person(user)
                            .map_or_else(|| person_name(user, ""), |p| p.name.clone());
                        self.shared.notice(format!(
                            "{name}'s key changed before you dismissed the warning, so it stays. Compare the new safety number."
                        ));
                    }
                    dismissed => self.after_trust_change(dismissed.map(drop)),
                }
            }
            UiCommand::VerifiedChanged => self.refresh_trust(),
            UiCommand::LeaveServer => {
                if self.leave_server().await {
                    return ControlFlow::Break(());
                }
            }
            UiCommand::Disconnect => {
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
            self.shared.emit(UiEvent::Users(shown_users(users)));
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
            let slot = match requester.request(Request::FetchAvatar(hash)).await {
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
                Some(bytes) => match self.call.request(Request::UploadAvatar(bytes)).await? {
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
        self.audio(CallCommand::Muted(!self.media.mic));
        self.audio(CallCommand::Deafened(self.media.deafened));
        self.show_shares();
        let response = self.call.request(Request::SetMedia(media)).await;
        if let Some(code) = share_start_refusal(&response, media.share, self.server_share) {
            self.undo_refused_share(code == ErrorCode::Full).await;
            return;
        }
        match response {
            Ok(Response::Ok) => self.server_share = media.share,
            other => self.refused(other),
        }
    }

    /// Stops a share the server wouldn't start, for a `full` room or the share allowance, and
    /// stops there any share the server still holds from before.
    async fn undo_refused_share(&mut self, full: bool) {
        self.stop_share();
        self.shared.update(|s| {
            if let Some(c) = s.current.as_mut() {
                c.sharing = None;
            }
        });
        self.show_shares();
        self.shared.notice(if full {
            format!("This room already has {MAX_ROOM_SHARES} screen shares running.")
        } else {
            "Sharing was started too often. Try again in a moment.".to_owned()
        });
        if self.server_share.is_some() {
            Box::pin(self.push_media()).await;
        }
    }

    /// Tells the server which shares we watch. Only running shares are named: one that ended
    /// would still count against [`throcc_proto::limits::MAX_WATCHED`].
    async fn push_watching(&mut self) {
        let mut ids = Vec::new();
        for user in &self.watching {
            let running = self.peers.get(user).filter(|p| p.share.is_some());
            if let Some(share) = running.and_then(|p| p.tracks.shares.first()) {
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
        self.simple(Request::SetWatched(ids)).await;
        self.sync_watching();
    }

    /// Keeps one decoder per watched share, following track and codec changes.
    fn sync_watching(&mut self) {
        let wanted: HashMap<UserId, (ShareTracks, ShareConfig)> = self
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
                self.audio(CallCommand::ListenShare {
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
            reports_with_drops: 0,
        }
    }

    fn stop_watching(&mut self, user: UserId) {
        if let Some(watched) = self.watched_shares.remove(&user) {
            self.call.engine.unsubscribe(watched.video);
            self.tracks.remove(&watched.video);
            if let Some(audio) = watched.audio {
                self.call.engine.unsubscribe(audio);
                self.tracks.remove(&audio);
                self.audio(CallCommand::ForgetShare(user));
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
                self.cue(Cue::StreamStart);
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
            self.cue(Cue::StreamStop);
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
    async fn renew_encryption_key(&mut self) {
        match self.keystore.renew_encryption_key_if_due() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                warn!("renewing the encryption key: {e:#}");
                return;
            }
        }
        let encryption = match self.keystore.encryption_key_pair() {
            Ok(encryption) => encryption,
            Err(e) => {
                warn!("the renewed encryption key is unusable: {e:#}");
                return;
            }
        };
        self.call
            .engine
            .keys()
            .replace_encryption(encryption, Instant::now());
        let (server, me) = (self.call.server(), self.call.me());
        let published =
            account::publish_encryption_key(&self.keystore, &self.call.requester, &server, me);
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
                KeyNotice::BadEncryptionKey { user } => {
                    self.shared.notice(format!(
                        "User {}'s encryption key failed its check; they can't hear you.",
                        user.0
                    ));
                }
                KeyNotice::ClashingTracks => self.shared.notice(
                    "The server described the room with two tracks under one id, so the call is paused until it sends a valid one.",
                ),
                KeyNotice::TooManyPeers => self.shared.notice(format!(
                    "The server described the room with more than {MAX_ROOM_MEMBERS} people, so the call is paused until it sends a valid one."
                )),
                KeyNotice::TooManyShares => self.shared.notice(format!(
                    "The server described the room with more than {MAX_ROOM_SHARES} screen shares for one person, so the call is paused until it sends a valid one."
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
            CallEvent::Server(event) => self.server_event(event).await,
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
                let playback = self.playback_of(p.user);
                let sharing = self.sharing_of(p.user);
                let audience = self.audience_of(p.user);
                self.shared.update(|s| {
                    let speaking = s.people.get(&p.user).is_some_and(|old| old.speaking);
                    let updated = Person {
                        verified,
                        identity_key,
                        speaking,
                        sharing,
                        audience,
                        ..person(&p, playback)
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
            ServerEvent::RoomLockChanged { room, locked } => self.shared.update(|s| {
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
            ServerEvent::PeersChanged { room, .. } => {
                if Some(room) == self.room {
                    self.follow_room().await;
                }
            }
            ServerEvent::AudiencesChanged(changed) => self.audiences_changed(changed),
            ServerEvent::ReceiveStats { tracks } => {
                for t in &tracks {
                    self.dropped_for_you = self.dropped_for_you.saturating_add(t.dropped_units);
                }
                for (media_id, watched) in self.tracks.iter_mut() {
                    if matches!(watched.carries, Carries::ShareVideo { .. }) {
                        let dropped = tracks
                            .iter()
                            .find(|t| t.media_id == *media_id)
                            .map_or(0, |t| t.dropped_units);
                        watched.server_reported(dropped);
                    }
                }
            }
            ServerEvent::RoleChanged { user, role } => {
                if user == self.call.me() {
                    self.shared.update(|s| s.role = Some(role));
                }
            }
            ServerEvent::InviteSpent { code_id } => self.shared.notice(format!(
                "Invite {code_id}-… was used up by three wrong tries and no longer works."
            )),
            ServerEvent::RemovedFromServer { reason } => {
                self.shared.emit(UiEvent::RemovedFromServer(reason))
            }
            ServerEvent::KickedFromRoom { by, room } if self.room == Some(room) => {
                self.put_out_of_room();
                let state = self.shared.state();
                let by = state.person(by).map_or("Someone", |p| p.name.as_str());
                let room = state.room(room).map_or("the room", |r| r.name.as_str());
                self.shared.notice(format!("{by} moved you out of {room}."));
            }
            ServerEvent::KickedFromRoom { .. } => {}
            ServerEvent::UserAdded(_) | ServerEvent::UserRemoved(_) => self.list_users().await,
            ServerEvent::KeyframeRequested { media_id, kind } => {
                if self
                    .my_tracks
                    .as_ref()
                    .and_then(|t| t.shares.first())
                    .is_some_and(|s| s.video == media_id)
                {
                    self.recover_share(kind);
                }
            }
            ServerEvent::SenderKeyPublished { .. } => {}
        }
    }

    fn refresh_levels(&mut self) {
        let Some(audio) = &self.audio else { return };
        let levels = audio.levels();
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
        let mut slow_shares = BTreeSet::new();
        let (mut received_total, mut lost_total) = (0, 0);
        for (media_id, track) in self.tracks.iter_mut() {
            let arrived = self.call.engine.arrived(*media_id);
            let new_bytes = advance(&mut track.last_bytes, arrived.bytes);
            let new_received = advance(&mut track.last_received, arrived.datagrams);
            let new_lost = advance(&mut track.last_lost, arrived.lost);
            received_total += new_received;
            lost_total += new_lost;
            if track.too_slow() {
                slow_shares.insert(track.user);
            }
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
            slow_shares,
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
            wanted.push(("mic", VOICE_DETAIL.into(), tracks.mic, MIC_KBPS, None));
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

/// `p` as a person, playing as `playback` says.
fn person(p: &Presence, playback: Playback) -> Person {
    Person {
        name: person_name(p.user, &p.name),
        avatar: p.avatar,
        room: p.room,
        mic: p.mic,
        deafened: p.deafened,
        sharing: None,
        audience: 0,
        speaking: false,
        playback,
        verified: false,
        identity_key: None,
        role: p.role,
    }
}

/// Why the server didn't answer `Ok`, in its own words where it gave some.
fn refusal(response: anyhow::Result<Response>) -> String {
    response.map_or_else(|e| e.to_string(), control::refusal)
}

/// The code with which `response` to a `SetMedia` carrying `sent` refused to start that share.
/// The server still applies the mic and deafen then, and keeps its share (`on_server`).
fn share_start_refusal(
    response: &anyhow::Result<Response>,
    sent: Option<ShareConfig>,
    on_server: Option<ShareConfig>,
) -> Option<ErrorCode> {
    match response {
        Ok(Response::Err {
            code: code @ (ErrorCode::RateLimited | ErrorCode::Full),
            ..
        }) if sent.is_some() && sent != on_server => Some(*code),
        _ => None,
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

/// The cue for someone joining or leaving a share's audience, if anyone did.
fn audience_cue(before: &[UserId], after: &[UserId]) -> Option<Cue> {
    let arrived = |from: &[UserId], to: &[UserId]| to.iter().any(|u| !from.contains(u));
    if arrived(before, after) {
        Some(Cue::StreamJoin)
    } else if arrived(after, before) {
        Some(Cue::StreamLeave)
    } else {
        None
    }
}

/// The cue for someone starting or stopping a share, if they did.
fn share_cue(was_sharing: bool, sharing: bool) -> Option<Cue> {
    match (was_sharing, sharing) {
        (false, true) => Some(Cue::StreamStart),
        (true, false) => Some(Cue::StreamStop),
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

fn gain_command(user: UserId, sound: Sound, level: SoundLevel) -> CallCommand {
    match sound {
        Sound::Voice => CallCommand::Volume(user, level.gain()),
        Sound::Share => CallCommand::ShareVolume(user, level.gain()),
    }
}

/// Whether `newer` sets the same thing as `older`, so only `newer` needs replaying.
fn supersedes(newer: &UiCommand, older: &UiCommand) -> bool {
    use std::mem::discriminant;
    match (newer, older) {
        (UiCommand::SetSoundLevel(a, sa, _), UiCommand::SetSoundLevel(b, sb, _)) => {
            a == b && sa == sb
        }
        (UiCommand::SetMic(_) | UiCommand::SetDeafened(_) | UiCommand::SetProfile(_), _) => {
            discriminant(newer) == discriminant(older)
        }
        _ => false,
    }
}

/// `backoff` scaled by a factor spread evenly over 1 ± `BACKOFF_JITTER`, picked by `random`.
/// The mean wait stays `backoff`.
fn jittered(backoff: Duration, random: u64) -> Duration {
    let unit = (random >> 11) as f64 / (1u64 << 53) as f64;
    backoff.mul_f64(1.0 - BACKOFF_JITTER + 2.0 * BACKOFF_JITTER * unit)
}

/// `new` unless it is older than `held`, cut to what a room can hold so a server can't make
/// us keep or search more.
fn newer_audiences(held: &Audiences, mut new: Audiences) -> Option<Audiences> {
    if new.seq < held.seq {
        return None;
    }
    new.shares.truncate(MAX_ROOM_SHARES);
    for audience in &mut new.shares {
        audience.receivers.truncate(MAX_ROOM_MEMBERS);
    }
    Some(new)
}

/// Who receives the share sent on `video`, if anyone does.
fn receivers_of(audiences: &Audiences, video: MediaId) -> Option<&[UserId]> {
    audiences
        .shares
        .iter()
        .find(|a| a.video == video)
        .map(|a| a.receivers.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the track counts as too slow at each health refresh, given the units each
    /// server report between refreshes dropped.
    fn too_slow_at_refreshes(reports: &[&[u32]]) -> Vec<bool> {
        let mut track = Watched {
            user: UserId(4),
            carries: Carries::ShareAudio,
            last_bytes: 0,
            last_received: 0,
            last_lost: 0,
            reports_with_drops: 0,
        };
        reports
            .iter()
            .map(|between| {
                for &dropped in *between {
                    track.server_reported(dropped);
                }
                track.too_slow()
            })
            .collect()
    }

    #[test]
    fn a_share_is_too_slow_only_while_consecutive_server_reports_carry_drops() {
        assert_eq!(
            too_slow_at_refreshes(&[&[3], &[0], &[1], &[2], &[5], &[0], &[4]]),
            [false, false, false, true, true, false, false]
        );
        assert_eq!(too_slow_at_refreshes(&[&[1, 1]]), [true]);
        assert_eq!(too_slow_at_refreshes(&[&[1, 0]]), [false]);
        assert_eq!(too_slow_at_refreshes(&[&[1, 1], &[0, 1]]), [true, false]);
    }

    #[test]
    fn a_refresh_without_a_server_report_keeps_the_slow_share_streak() {
        assert_eq!(
            too_slow_at_refreshes(&[&[1], &[], &[1], &[], &[0], &[]]),
            [false, false, true, true, false, false]
        );
    }

    #[test]
    fn only_a_share_start_refused_for_limits_is_undone() {
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
        assert_eq!(
            share_start_refusal(&limited(), share, None),
            Some(ErrorCode::RateLimited)
        );
        // A push-to-talk press while the server already has our share.
        assert!(share_start_refusal(&limited(), share, share).is_none());
        assert!(share_start_refusal(&limited(), None, None).is_none());
        assert!(share_start_refusal(&Ok(Response::Ok), share, None).is_none());
        let forbidden = Ok(Response::Err {
            code: ErrorCode::Forbidden,
            message: "no".into(),
        });
        assert!(share_start_refusal(&forbidden, share, None).is_none());
        let full = Ok(Response::Err {
            code: ErrorCode::Full,
            message: "this room already has 5 screen shares running".into(),
        });
        assert_eq!(
            share_start_refusal(&full, share, None),
            Some(ErrorCode::Full)
        );
        assert!(share_start_refusal(&Err(anyhow::anyhow!("timed out")), share, None).is_none());
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
            server: Some(ServerView {
                address: "voice.example.net".into(),
                fingerprint: throcc_proto::Fingerprint([0x2c; 32]),
                name: None,
                avatar: Some(shown),
            }),
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
        use crate::client::UiCommand;
        assert!(supersedes(
            &UiCommand::SetMic(false),
            &UiCommand::SetMic(true)
        ));
        assert!(!supersedes(
            &UiCommand::SetMic(false),
            &UiCommand::SetDeafened(true)
        ));
        let profile = |name: &str| {
            UiCommand::SetProfile(Profile {
                name: name.into(),
                picture: None,
            })
        };
        assert!(supersedes(&profile("Ilse"), &profile("Mara")));
        let volume = |user, sound, v| {
            let level = SoundLevel {
                volume: v,
                muted: false,
            };
            UiCommand::SetSoundLevel(UserId(user), sound, level)
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
            &UiCommand::Watch(UserId(1)),
            &UiCommand::Watch(UserId(2))
        ));
    }

    #[test]
    fn only_audiences_as_new_as_held_are_taken_and_within_a_room() {
        let audiences = |seq, shares: usize, receivers: u64| Audiences {
            seq,
            shares: (0..shares as u32)
                .map(|video| throcc_proto::messages::Audience {
                    video: MediaId(video),
                    receivers: (0..receivers).map(UserId).collect(),
                })
                .collect(),
        };
        let held = audiences(5, 1, 1);
        assert_eq!(newer_audiences(&held, audiences(4, 0, 0)), None);
        assert_eq!(
            newer_audiences(&held, audiences(5, 0, 0)),
            Some(audiences(5, 0, 0))
        );
        let huge = newer_audiences(&held, audiences(6, 1000, 1000)).unwrap();
        assert_eq!(huge.shares.len(), MAX_ROOM_SHARES);
        assert!(
            huge.shares
                .iter()
                .all(|a| a.receivers.len() == MAX_ROOM_MEMBERS)
        );
    }

    #[test]
    fn joining_or_leaving_an_audience_cues() {
        let (bob, eve) = (UserId(2), UserId(3));
        assert_eq!(audience_cue(&[], &[bob]), Some(Cue::StreamJoin));
        assert_eq!(audience_cue(&[bob, eve], &[bob]), Some(Cue::StreamLeave));
        assert_eq!(audience_cue(&[bob], &[bob]), None);
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
