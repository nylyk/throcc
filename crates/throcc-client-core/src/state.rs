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
    /// Our name as kept on this computer, once we have chosen one.
    pub profile_name: Option<String>,
    /// Our picture as kept on this computer, readable through the client's avatars.
    pub profile_picture: Option<Hash>,
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
pub const BITRATES: [u32; 13] = [
    1_000, 1_500, 2_000, 3_000, 4_000, 5_000, 6_000, 8_000, 10_000, 12_000, 15_000, 18_000, 20_000,
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
            (Resolution::P720, ..=15) => 3_000,
            (Resolution::P720, ..=30) => 5_000,
            (Resolution::P720, _) => 8_000,
            (Resolution::P1080, ..=15) => 5_000,
            (Resolution::P1080, ..=30) => 10_000,
            (Resolution::P1080, _) => 15_000,
            (Resolution::P1440, ..=15) => 8_000,
            (Resolution::P1440, ..=30) => 15_000,
            (Resolution::P1440, _) => 20_000,
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
    pub fn config(self, codec: Codec, audio: bool) -> ShareConfig {
        let height = self.resolution.height();
        ShareConfig {
            audio,
            kbps: self.kbps,
            codec,
            width: height / 9 * 16,
            height,
            fps: self.fps,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_stop_at_the_cap_and_a_lower_choice_lowers_the_bitrate() {
        let top = ShareChoice {
            resolution: Resolution::P1440,
            fps: 60,
            kbps: 20_000,
        };
        assert_eq!(top.bitrates().last(), Some(&20_000));
        let lower = top.with_resolution(Resolution::P720).with_fps(15);
        assert_eq!(lower.kbps, 3_000);
        assert_eq!(lower.bitrates(), [1_000, 1_500, 2_000, 3_000]);
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
