use throcc_client_core::state::{self as core_state, FRAME_RATES, Resolution};

/// What a share asks for before the screen is known: the height of the box the screen is
/// fitted in, the frame rate and the bitrate in kbps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShareChoice {
    pub height: u16,
    pub fps: u8,
    pub kbps: u32,
}

impl From<core_state::ShareChoice> for ShareChoice {
    fn from(choice: core_state::ShareChoice) -> Self {
        Self {
            height: choice.resolution.height(),
            fps: choice.fps,
            kbps: choice.kbps,
        }
    }
}

impl From<ShareChoice> for core_state::ShareChoice {
    /// The nearest choice the core offers: an unknown height or frame rate keeps the
    /// default's, and the bitrate is capped for the rest.
    fn from(choice: ShareChoice) -> Self {
        let default = core_state::ShareChoice::default();
        let resolution = Resolution::ALL
            .into_iter()
            .find(|r| r.height() == choice.height)
            .unwrap_or(default.resolution);
        let fps = if FRAME_RATES.contains(&choice.fps) {
            choice.fps
        } else {
            default.fps
        };
        core_state::ShareChoice {
            kbps: choice.kbps,
            ..default
        }
        .with_resolution(resolution)
        .with_fps(fps)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShareResolution {
    pub height: u16,
    /// Such as "1080p".
    pub label: String,
}

#[uniffi::export]
pub fn default_share_choice() -> ShareChoice {
    core_state::ShareChoice::default().into()
}

#[uniffi::export]
pub fn share_resolutions() -> Vec<ShareResolution> {
    Resolution::ALL
        .into_iter()
        .map(|r| ShareResolution {
            height: r.height(),
            label: r.label().to_owned(),
        })
        .collect()
}

#[uniffi::export]
pub fn share_frame_rates() -> Vec<u8> {
    FRAME_RATES.to_vec()
}

/// The bitrates offered at `choice`'s height and frame rate, lowest first.
#[uniffi::export]
pub fn share_bitrates(choice: ShareChoice) -> Vec<u32> {
    core_state::ShareChoice::from(choice).bitrates()
}

/// `choice` as the core takes it, its bitrate capped for its height and frame rate.
#[uniffi::export]
pub fn fit_share_choice(choice: ShareChoice) -> ShareChoice {
    core_state::ShareChoice::from(choice).into()
}

/// A bitrate as the share settings show it, such as "6 Mbps".
#[uniffi::export]
pub fn mbps(kbps: u32) -> String {
    core_state::mbps(kbps)
}
