use std::collections::BTreeMap;

use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::messages::{Codec, ShareConfig};

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerView {
    pub address: String,
    pub fingerprint: Fingerprint,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomView {
    pub id: RoomId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    pub avatar: Option<Hash>,
    pub room: Option<RoomId>,
    pub mic: bool,
    pub deafened: bool,
    pub sharing: Option<ShareConfig>,
    pub speaking: bool,
    pub volume: f32,
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentRoom {
    pub room: RoomId,
    pub mic: bool,
    pub deafened: bool,
    pub sharing: Option<ShareConfig>,
    /// Whose shares we receive.
    pub watching: Vec<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyWarning {
    pub user: UserId,
    pub pinned: Fingerprint,
    pub now: Fingerprint,
}

/// The connection panel's numbers, refreshed every 2 s.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub rtt_ms: f32,
    pub upload_loss: f32,
    pub download_loss: f32,
    pub dropped_for_you: u32,
    pub verdict: String,
    pub receiving: Vec<TrackHealth>,
    pub sending: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackHealth {
    pub label: String,
    pub set_kbps: Option<u32>,
    pub arriving_kbps: Option<u32>,
    pub loss: Option<f32>,
}

/// A share preset from PLAN §5.9.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Low,
    Default,
    High,
    Extreme,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Low, Preset::Default, Preset::High, Preset::Extreme];

    pub fn config(self, codec: Codec, audio: bool) -> ShareConfig {
        let (width, height, fps, kbps) = match self {
            Preset::Low => (1280, 720, 30, 2_500),
            Preset::Default => (1920, 1080, 60, 6_000),
            Preset::High => (2560, 1440, 60, 10_000),
            Preset::Extreme => (2560, 1440, 60, 18_000),
        };
        ShareConfig {
            audio,
            kbps,
            codec,
            width,
            height,
            fps,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Low => "Low",
            Preset::Default => "Default",
            Preset::High => "High",
            Preset::Extreme => "Extreme",
        }
    }
}

/// "1440p60 · AV1" for a share.
pub fn share_label(config: &ShareConfig) -> String {
    let codec = match config.codec {
        Codec::Av1 => "AV1",
        Codec::Hevc => "HEVC",
        Codec::H264 => "H.264",
        Codec::Vp9 => "VP9",
    };
    format!("{}p{} · {codec}", config.height, config.fps)
}
