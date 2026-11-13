use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::limits::{MAX_ROOMS, MAX_SIGNED_IN};
use throcc_proto::messages::{Codec, PublicKey, Recovery, Room, ShareConfig, valid_name};
use throcc_proto::permissions;

pub(crate) const MIC_KBPS: u32 = 64;
pub(crate) const SHARE_AUDIO_KBPS: u32 = crate::media::share_audio::KBPS;
pub(crate) const VOICE_DETAIL: &str = "Opus voice, mono";
pub(crate) const SHARE_AUDIO_DETAIL: &str = "Opus music, stereo";

/// Everything the UI shows, rebuilt by the core and sent whole after every change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub server: Option<ServerView>,
    pub connection: ConnectionStatus,
    pub me: Option<UserId>,
    pub role: Option<Role>,
    pub rooms: Vec<RoomView>,
    pub people: BTreeMap<UserId, Person>,
    pub current: Option<CurrentRoom>,
    /// Our mic and deafen switches, which stay as set across rooms.
    pub mic: bool,
    pub deafened: bool,
    pub key_warnings: Vec<KeyWarning>,
    pub health: Health,
}

impl State {
    pub fn person(&self, user: UserId) -> Option<&Person> {
        self.people.get(&user)
    }

    pub fn occupants(&self, room: RoomId) -> impl Iterator<Item = (&UserId, &Person)> {
        self.people
            .iter()
            .filter(move |(_, p)| p.room == Some(room))
    }

    pub fn room(&self, room: RoomId) -> Option<&RoomView> {
        self.rooms.iter().find(|r| r.id == room)
    }

    pub fn room_mut(&mut self, room: RoomId) -> Option<&mut RoomView> {
        self.rooms.iter_mut().find(|r| r.id == room)
    }

    /// Whether the identity key the server shows for `user` replaced one the user verified.
    pub fn key_changed(&self, user: UserId) -> bool {
        self.key_warnings.iter().any(|w| w.user == user)
    }

    /// What we know of the identity key the server shows for `user`.
    pub fn trust(&self, user: UserId) -> KeyTrust {
        if self.key_changed(user) {
            KeyTrust::KeyChanged
        } else if self.me == Some(user) || self.person(user).is_some_and(|p| p.verified) {
            KeyTrust::Verified
        } else {
            KeyTrust::Unverified
        }
    }

    /// The room we are in while the connection is up or coming back. A link that came back
    /// lists us among the room's people a moment before `current` names the room.
    pub fn live_room(&self) -> Option<RoomId> {
        if !matches!(
            self.connection,
            ConnectionStatus::Connected | ConnectionStatus::Reconnecting { .. }
        ) {
            return None;
        }
        let ours = || self.person(self.me?)?.room;
        self.current.as_ref().map(|c| c.room).or_else(ours)
    }

    /// What picking `room` does: nothing where we are in it already, and asking for its
    /// password first where it is locked and we can't manage rooms.
    pub fn join_action(&self, room: RoomId) -> JoinAction {
        let manages_rooms = self.role.is_some_and(permissions::can_manage_rooms);
        if self.current.as_ref().is_some_and(|c| c.room == room) {
            JoinAction::AlreadyIn
        } else if self.room(room).is_some_and(|r| r.locked) && !manages_rooms {
            JoinAction::AskPassword
        } else {
            JoinAction::Join
        }
    }

    /// The pictures of the server and everyone shown.
    pub fn avatars_in_use(&self) -> HashSet<Hash> {
        let server = self.server.as_ref().and_then(|s| s.avatar);
        let people = self.people.values().filter_map(|p| p.avatar);
        server.into_iter().chain(people).collect()
    }

    /// Lists `room`, or updates it if it is listed already.
    pub(crate) fn add_room(&mut self, room: RoomView) {
        if let Some(listed) = self.room_mut(room.id) {
            *listed = room;
        } else if self.rooms.len() < MAX_ROOMS {
            self.rooms.push(room);
        }
    }

    /// Shows `person` as `user`, unless that would show more than [`MAX_SIGNED_IN`].
    pub(crate) fn add_person(&mut self, user: UserId, person: Person) {
        if self.people.len() < MAX_SIGNED_IN || self.people.contains_key(&user) {
            self.people.insert(user, person);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerView {
    pub address: String,
    pub fingerprint: Fingerprint,
    /// The name its admins gave it.
    pub name: Option<String>,
    pub avatar: Option<Hash>,
}

impl ServerView {
    /// The name its admins gave it, or its address.
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.address)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connected,
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    /// The link was down too long to sign in again unasked, which could push another
    /// device of ours off the server. `UiCommand::Reconnect` signs in again.
    AwaitingReconnect,
    /// Another device signed in as us, which ended this session.
    SignedInElsewhere,
    Failed(String),
}

impl ConnectionStatus {
    /// Whether a session holds on to the server: connected, or bringing the link back
    /// with or without asking first. Signing in again would only throw that away.
    pub fn holds_session(&self) -> bool {
        matches!(
            self,
            Self::Connected | Self::Reconnecting { .. } | Self::AwaitingReconnect
        )
    }

    /// What to tell the user about the connection, or nothing while it is up or ended on
    /// purpose.
    pub fn notice(&self) -> Option<ConnectionNotice> {
        let notice = match self {
            Self::Connected | Self::Disconnected => return None,
            Self::Reconnecting { attempt, reason } => ConnectionNotice {
                summary: format!(
                    "Reconnecting (attempt {attempt}): {reason}. You’ll be put back in your room."
                ),
                hint: format!("Reconnecting, attempt {attempt}. You’ll be put back in your room."),
                problem: Some(format!("Lost the connection: {reason}.")),
                action: None,
                busy: true,
                serious: false,
            },
            Self::AwaitingReconnect => ConnectionNotice {
                summary: "Disconnected after a while away. Reconnect from the room.".into(),
                hint: "The connection was down for a while. Reconnecting here signs you out on any other device.".into(),
                problem: None,
                action: Some(NoticeAction::Reconnect),
                busy: false,
                serious: false,
            },
            Self::SignedInElsewhere => ConnectionNotice {
                summary: "Disconnected: signed in on another device".into(),
                hint: "Pick the server to sign in here again.".into(),
                problem: Some("Signed in on another device.".into()),
                action: Some(NoticeAction::SignInHere),
                busy: false,
                serious: true,
            },
            Self::Failed(reason) => ConnectionNotice {
                summary: format!("Disconnected: {reason}"),
                hint: "Pick the server to try again.".into(),
                problem: Some(reason.clone()),
                action: Some(NoticeAction::TryAgain),
                busy: false,
                serious: true,
            },
        };
        Some(notice)
    }
}

/// What became of a connection that isn't up, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionNotice {
    /// All of it in one line, for a strip across a page.
    pub summary: String,
    /// What happens next, or what the user can do.
    pub hint: String,
    /// What went wrong, where something did.
    pub problem: Option<String>,
    pub action: Option<NoticeAction>,
    /// Whether the core is at work on it.
    pub busy: bool,
    /// Whether the connection ended, rather than coming back.
    pub serious: bool,
}

/// What the user can do about a connection that isn't up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeAction {
    /// `UiCommand::Reconnect`.
    Reconnect,
    /// Sign in to the server again, which pushes the other device off.
    SignInHere,
    /// Sign in to the server again.
    TryAgain,
}

/// What picking a room does, from [`State::join_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinAction {
    AlreadyIn,
    AskPassword,
    Join,
}

/// What we know of the identity key the server shows for someone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyTrust {
    /// It replaced a key the user verified.
    KeyChanged,
    Unverified,
    /// Verified, or our own.
    Verified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomView {
    pub id: RoomId,
    pub name: String,
    /// Whether joining needs a password.
    pub locked: bool,
}

impl From<&Room> for RoomView {
    fn from(room: &Room) -> Self {
        Self {
            id: room.id,
            name: room_name(room.id, &room.name),
            locked: room.locked,
        }
    }
}

/// A name from the server as shown: as given if an honest server would have taken it,
/// else nothing, so text that hides or reorders characters never reaches the screen.
pub fn shown_name(name: &str) -> Option<String> {
    valid_name(name).map(str::to_owned)
}

/// The letter a picture stands in with for `name`: its first letter or digit.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".into(), |c| c.to_uppercase().to_string())
}

pub(crate) fn person_name(user: UserId, name: &str) -> String {
    shown_name(name).unwrap_or_else(|| format!("User {}", user.0))
}

pub(crate) fn room_name(room: RoomId, name: &str) -> String {
    shown_name(name).unwrap_or_else(|| format!("Room {}", room.0))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    pub avatar: Option<Hash>,
    pub room: Option<RoomId>,
    pub mic: bool,
    pub deafened: bool,
    pub sharing: Option<ShareConfig>,
    /// How many people in our room watch their share.
    pub audience: usize,
    pub speaking: bool,
    pub playback: Playback,
    /// Whether the user verified the identity key they presented this session.
    pub verified: bool,
    /// The name that key was verified under, while it is verified.
    pub verified_name: Option<String>,
    /// The identity key they presented this session, in a room with us. None until then,
    /// even for someone verified before: nothing else ties their account here to that key.
    pub identity_key: Option<PublicKey>,
    pub role: Role,
    /// Whether the user blocked them. A blocked person keeps their place in the room, under
    /// [`BLOCKED_NAME`] and without their picture, sounds or shares.
    pub blocked: bool,
}

impl Person {
    /// The name they were verified under, where the server now shows another: the server
    /// picks the name, so it alone doesn't say who is verified.
    pub fn verified_as(&self) -> Option<&str> {
        let name = self.verified_name.as_deref().filter(|_| self.verified)?;
        (!self.blocked && name != self.name).then_some(name)
    }
}

/// The name a blocked person is shown under.
pub const BLOCKED_NAME: &str = "Blocked person";

/// Which of someone's sounds a [`SoundLevel`] is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    Voice,
    Share,
}

impl Sound {
    pub const ALL: [Sound; 2] = [Sound::Voice, Sound::Share];
}

/// How loud one person's voice and share audio play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub voice: SoundLevel,
    pub share: SoundLevel,
}

impl Playback {
    pub fn get(&self, sound: Sound) -> SoundLevel {
        match sound {
            Sound::Voice => self.voice,
            Sound::Share => self.share,
        }
    }

    pub fn get_mut(&mut self, sound: Sound) -> &mut SoundLevel {
        match sound {
            Sound::Voice => &mut self.voice,
            Sound::Share => &mut self.share,
        }
    }
}

/// The highest gain a volume can boost a sound by, +12 dB.
pub const MAX_VOLUME: f32 = 4.0;

/// The quietest a volume slider goes before silence.
const QUIETEST_DB: f32 = -36.0;

/// The gain for a volume slider at `loudness`, from 0 (silent) to 2 where 1 plays the sound
/// as it arrives. Each half rises evenly in decibels: from -36 dB to unity, then to
/// [`MAX_VOLUME`].
pub fn loudness_to_gain(loudness: f32) -> f32 {
    let loudness = loudness.clamp(0.0, 2.0);
    if loudness == 0.0 {
        0.0
    } else if loudness <= 1.0 {
        10f32.powf(QUIETEST_DB * (1.0 - loudness) / 20.0)
    } else {
        MAX_VOLUME.powf(loudness - 1.0)
    }
}

/// Where a volume slider sits for `gain`, the inverse of [`loudness_to_gain`].
pub fn gain_to_loudness(gain: f32) -> f32 {
    let gain = gain.clamp(0.0, MAX_VOLUME);
    if gain == 0.0 {
        0.0
    } else if gain <= 1.0 {
        (1.0 - 20.0 * gain.log10() / QUIETEST_DB).max(0.0)
    } else {
        1.0 + gain.ln() / MAX_VOLUME.ln()
    }
}

/// How loud one of someone's sounds plays on this machine. It is never sent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundLevel {
    /// From 0 to [`MAX_VOLUME`], where 1 plays the sound as it arrives.
    pub volume: f32,
    pub muted: bool,
}

impl Default for SoundLevel {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

impl SoundLevel {
    /// What the mix multiplies the sound by.
    pub fn gain(self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentRoom {
    pub room: RoomId,
    pub sharing: Option<ShareConfig>,
    /// Whose shares we receive.
    pub watching: Vec<UserId>,
    /// Whose watched shares have a picture to show.
    pub showing: Vec<UserId>,
}

/// The server showed `user`, whose identity key `verified` was verified, with another key
/// `now` that isn't. The new key still gets our media: it is only unverified, like anyone
/// new. Kept, across restarts and even once `verified` is shown again, until the new key is
/// verified or the user dismisses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyWarning {
    pub user: UserId,
    pub verified: Fingerprint,
    pub now: Fingerprint,
}

/// Someone blocked on this install, as they looked when blocked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedPerson {
    pub key: Fingerprint,
    pub name: String,
    pub avatar: Option<Hash>,
    /// The server they were blocked on.
    pub server: Option<Fingerprint>,
}

/// An identity key verified on this install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub key: Fingerprint,
    /// The name they had when verified.
    pub name: String,
    /// The servers it was seen on.
    pub servers: Vec<Fingerprint>,
}

/// The connection panel's numbers, refreshed every 2 s.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub rtt_ms: f32,
    /// Everything the connection received and sent over the last refresh, overhead included.
    pub download_kbps: u32,
    pub upload_kbps: u32,
    pub upload_loss: f32,
    pub download_loss: f32,
    pub dropped_for_you: u32,
    /// Sharers whose frames the server dropped for us in its last two reports: our connection
    /// can't keep up with their share.
    pub slow_shares: BTreeSet<UserId>,
    /// Sharers whose pictures this device decodes too slowly to keep up with.
    pub lagging_shares: BTreeSet<UserId>,
    pub receiving: Vec<TrackHealth>,
    pub sending: Vec<SentTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackHealth {
    pub label: String,
    /// The codec, and for video its size, frame rate and decoder.
    pub detail: String,
    pub set_kbps: Option<u32>,
    pub arriving_kbps: u32,
    pub loss: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SentTrack {
    pub label: &'static str,
    /// The codec, and for video its size, frame rate and encoder.
    pub detail: String,
    pub set_kbps: u32,
    pub leaving_kbps: u32,
    /// Units dropped before they left, for tracks that can drop them.
    pub dropped: Option<u64>,
}

/// The most lines a share is sent with; its width follows the screen's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    P720,
    P1080,
    P1440,
}

impl Resolution {
    pub const ALL: [Resolution; 3] = [Resolution::P720, Resolution::P1080, Resolution::P1440];

    pub fn height(self) -> u16 {
        match self {
            Resolution::P720 => 720,
            Resolution::P1080 => 1080,
            Resolution::P1440 => 1440,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Resolution::P720 => "720p",
            Resolution::P1080 => "1080p",
            Resolution::P1440 => "1440p",
        }
    }
}

pub const FRAME_RATES: [u8; 3] = [15, 30, 60];

/// The bitrates a share can be set to, in kbps.
pub const BITRATES: [u32; 11] = [
    1_000, 1_500, 2_000, 3_000, 4_000, 5_000, 6_000, 8_000, 10_000, 12_000, 15_000,
];

/// What a share asks for before the screen is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareChoice {
    pub resolution: Resolution,
    pub fps: u8,
    pub kbps: u32,
}

impl Default for ShareChoice {
    fn default() -> Self {
        Self {
            resolution: Resolution::P1080,
            fps: 60,
            kbps: 6_000,
        }
    }
}

impl ShareChoice {
    /// The highest bitrate offered at this resolution and frame rate; more than this buys
    /// nothing visible for screen content.
    pub fn max_kbps(self) -> u32 {
        match (self.resolution, self.fps) {
            (Resolution::P720, ..=15) => 2_000,
            (Resolution::P720, ..=30) => 4_000,
            (Resolution::P720, _) => 6_000,
            (Resolution::P1080, ..=15) => 4_000,
            (Resolution::P1080, ..=30) => 6_000,
            (Resolution::P1080, _) => 10_000,
            (Resolution::P1440, ..=15) => 5_000,
            (Resolution::P1440, ..=30) => 10_000,
            (Resolution::P1440, _) => 15_000,
        }
    }

    /// The bitrates offered at this resolution and frame rate, lowest first.
    pub fn bitrates(self) -> Vec<u32> {
        let max = self.max_kbps();
        BITRATES.into_iter().filter(|k| *k <= max).collect()
    }

    pub fn with_resolution(self, resolution: Resolution) -> Self {
        Self { resolution, ..self }.clamped()
    }

    pub fn with_fps(self, fps: u8) -> Self {
        Self { fps, ..self }.clamped()
    }

    fn clamped(self) -> Self {
        Self {
            kbps: self.kbps.min(self.max_kbps()),
            ..self
        }
    }

    /// The share to ask for. Its size is a 16:9 box the sender fits the screen's shape in.
    /// The encoder decides its codec and recovery.
    pub fn config(self, codec: Codec, audio: bool) -> ShareConfig {
        let height = self.resolution.height();
        ShareConfig {
            audio,
            kbps: self.kbps,
            codec,
            width: height / 9 * 16,
            height,
            fps: self.fps,
            recovery: Recovery::Idr,
        }
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::Av1 => "AV1",
        Codec::Hevc => "HEVC",
        Codec::H264 => "H.264",
        Codec::Vp9 => "VP9",
    }
}

/// "1440p60 AV1" for a share.
pub fn share_label(config: &ShareConfig) -> String {
    format!(
        "{}p{} {}",
        config.height,
        config.fps,
        codec_name(config.codec)
    )
}

/// "AV1 · 2560×1440 · 60 fps · VA-API" for a share and what encodes or decodes it.
pub fn share_detail(config: &ShareConfig, engine: &str) -> String {
    format!(
        "{} · {}×{} · {} fps · {engine}",
        codec_name(config.codec),
        config.width,
        config.height,
        config.fps
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Person {
        Person {
            name: name.into(),
            avatar: None,
            room: None,
            mic: false,
            deafened: false,
            sharing: None,
            audience: 0,
            speaking: false,
            playback: Playback::default(),
            verified: false,
            verified_name: None,
            identity_key: None,
            role: Role::User,
            blocked: false,
        }
    }

    #[test]
    fn names_that_hide_text_are_shown_as_their_id() {
        assert_eq!(person_name(UserId(7), " Bob "), "Bob");
        for hiding in [
            "Bob\u{202E}",
            "\u{200B}",
            "two\nlines",
            "",
            &"x".repeat(200),
        ] {
            assert_eq!(person_name(UserId(7), hiding), "User 7");
            assert_eq!(room_name(RoomId(3), hiding), "Room 3");
        }
    }

    #[test]
    fn a_repeated_room_is_listed_once_and_the_lists_stop_at_their_caps() {
        let mut state = State::default();
        let room = |id, name: &str| RoomView {
            id: RoomId(id),
            name: name.into(),
            locked: false,
        };
        state.add_room(room(1, "a"));
        state.add_room(room(1, "b"));
        assert_eq!(state.rooms, [room(1, "b")]);
        for id in 0..MAX_ROOMS as u64 + 10 {
            state.add_room(room(id, "r"));
        }
        assert_eq!(state.rooms.len(), MAX_ROOMS);

        for id in 0..MAX_SIGNED_IN as u64 + 10 {
            state.add_person(UserId(id), named("p"));
        }
        assert_eq!(state.people.len(), MAX_SIGNED_IN);
        state.add_person(UserId(0), named("renamed"));
        assert_eq!(state.people[&UserId(0)].name, "renamed");
        state.add_person(UserId(MAX_SIGNED_IN as u64), named("over"));
        assert!(state.person(UserId(MAX_SIGNED_IN as u64)).is_none());
    }

    #[test]
    fn bitrates_stop_at_the_cap_and_a_lower_choice_lowers_the_bitrate() {
        let top = ShareChoice {
            resolution: Resolution::P1440,
            fps: 60,
            kbps: 15_000,
        };
        assert_eq!(top.bitrates().last(), Some(&15_000));
        let lower = top.with_resolution(Resolution::P720).with_fps(15);
        assert_eq!(lower.kbps, 2_000);
        assert_eq!(lower.bitrates(), [1_000, 1_500, 2_000]);
        assert!(
            ShareChoice::default()
                .bitrates()
                .contains(&ShareChoice::default().kbps)
        );
    }

    #[test]
    fn every_choice_has_a_bitrate_and_a_sixteen_by_nine_box() {
        for resolution in Resolution::ALL {
            for fps in FRAME_RATES {
                let choice = ShareChoice::default()
                    .with_resolution(resolution)
                    .with_fps(fps);
                assert!(choice.bitrates().contains(&choice.max_kbps()), "{choice:?}");
                let config = choice.config(Codec::Av1, false);
                assert_eq!(u32::from(config.width) * 9, u32::from(config.height) * 16);
            }
        }
    }
}
