use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use throcc_proto::messages::valid_name;

use crate::json_file;
use crate::profile::Profile;
use crate::state::MAX_VOLUME;

const FILE: &str = "settings.json";
/// Processed mic level (dBFS) above which voice activation opens, unless configured otherwise.
pub const DEFAULT_GATE_DBFS: f32 = -50.0;

/// Everything the user sets for the app as a whole, kept in one file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `None` until onboarding keeps a name.
    pub profile: Option<Profile>,
    pub appearance: Appearance,
    pub audio: AudioSettings,
    /// Whether the app asks GitHub for a newer release when it starts, unset for the build's
    /// default.
    pub check_for_updates: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// The picked theme's name: a preset's, or `Custom`.
    pub theme: Option<String>,
    /// The Custom theme's eight colours, as `#RRGGBB`.
    pub palette: Option<Vec<String>>,
    /// Whether the sidebar is see-through, unset for the platform's default.
    pub translucent_sidebar: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// The mic by name, `None` for the system default.
    pub input_device: Option<String>,
    /// The speakers by name, `None` for the system default.
    pub output_device: Option<String>,
    pub voice: VoiceSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transmit {
    VoiceActivity { threshold_dbfs: f32 },
    PushToTalk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseSuppression {
    Low,
    Moderate,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub echo_cancellation: bool,
    pub noise_suppression: Option<NoiseSuppression>,
    pub automatic_gain: bool,
    pub transmit: Transmit,
    /// Voice activation opens only while the speech detector also hears speech.
    pub speech_only: bool,
    /// Gain on the mic before processing, 0 to [`MAX_VOLUME`].
    pub input_volume: f32,
    /// Gain on everything played, 0 to [`MAX_VOLUME`].
    pub output_volume: f32,
    /// Gain on cues before the output volume, 0 to [`MAX_VOLUME`].
    pub cue_volume: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            echo_cancellation: true,
            noise_suppression: Some(NoiseSuppression::Moderate),
            automatic_gain: true,
            transmit: Transmit::VoiceActivity {
                threshold_dbfs: DEFAULT_GATE_DBFS,
            },
            speech_only: false,
            input_volume: 1.0,
            output_volume: 1.0,
            cue_volume: 1.0,
        }
    }
}

impl json_file::Format for Settings {
    const MIGRATIONS: &'static [json_file::Migration] = &[|v1| {
        if v1.get("check_for_updates") == Some(&true.into()) {
            v1.remove("check_for_updates");
        }
    }];
}

/// The eight colours of the default theme, in the order [`Appearance::palette`] keeps them.
pub const DEFAULT_PALETTE: [u32; 8] = [
    0x0C0D11, 0x14161D, 0xECEDF3, 0x8E92A6, 0xD07A93, 0xFFB86B, 0x5BE3A4, 0xFF6B85,
];
/// Where the accent sits in a palette.
pub const ACCENT: usize = 4;
/// The preset themes by name with their accents. The first is the default.
pub const PRESET_ACCENTS: [(&str, u32); 7] = [
    ("Ink", 0xD07A93),
    ("Moss", 0x7FD1A8),
    ("Dusk", 0xFF8FB8),
    ("Tide", 0x5CC8E8),
    ("Ember", 0xFF9A5C),
    ("Paper", 0x5B4BDB),
    ("Sand", 0x1E7F86),
];
/// The name [`Appearance::theme`] has while [`Appearance::palette`] holds the colours.
pub const CUSTOM_THEME: &str = "Custom";

impl Settings {
    /// The settings kept in `dir`, the defaults where there are none. A name a server would
    /// refuse counts as no profile, and volumes are kept in range.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut settings: Self = json_file::load(&dir.join(FILE))?;
        settings.profile = settings.profile.and_then(|profile| {
            Some(Profile {
                name: valid_name(&profile.name)?.to_owned(),
                ..profile
            })
        });
        let voice = &mut settings.audio.voice;
        for volume in [
            &mut voice.input_volume,
            &mut voice.output_volume,
            &mut voice.cue_volume,
        ] {
            *volume = volume.clamp(0.0, MAX_VOLUME);
        }
        Ok(settings)
    }

    /// Replaces the settings kept in `dir` with these. A file that can't be read, or is from
    /// a newer throcc, is left as it is.
    pub fn save(&self, dir: &Path) -> Result<()> {
        json_file::update(&dir.join(FILE), |kept: &mut Self| *kept = self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn everything_changed() -> Settings {
        Settings {
            profile: Some(Profile {
                name: "Ilse".into(),
                picture: Some(Arc::from(&b"\x89PNG picture"[..])),
            }),
            appearance: Appearance {
                theme: Some("Custom".into()),
                palette: Some(vec!["#101010".into(); 8]),
                translucent_sidebar: Some(false),
            },
            audio: AudioSettings {
                input_device: Some("USB Mic".into()),
                output_device: Some("Headphones".into()),
                voice: VoiceSettings {
                    echo_cancellation: false,
                    noise_suppression: Some(NoiseSuppression::High),
                    automatic_gain: false,
                    transmit: Transmit::VoiceActivity {
                        threshold_dbfs: -42.0,
                    },
                    speech_only: true,
                    input_volume: 1.5,
                    output_volume: 0.25,
                    cue_volume: 0.0,
                },
            },
            check_for_updates: Some(false),
        }
    }

    #[test]
    fn every_setting_loads_back_as_saved() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());
        let settings = everything_changed();
        settings.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), settings);

        let push_to_talk = Settings {
            profile: None,
            audio: AudioSettings {
                voice: VoiceSettings {
                    transmit: Transmit::PushToTalk,
                    noise_suppression: None,
                    ..VoiceSettings::default()
                },
                ..AudioSettings::default()
            },
            ..settings
        };
        push_to_talk.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), push_to_talk);
    }

    #[test]
    fn a_version_1_update_check_follows_the_build_unless_it_was_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        for (v1, loaded) in [
            (r#"{"version": 1}"#, None),
            (r#"{"version": 1, "check_for_updates": true}"#, None),
            (r#"{"version": 1, "check_for_updates": false}"#, Some(false)),
        ] {
            std::fs::write(dir.path().join(FILE), v1).unwrap();
            assert_eq!(
                Settings::load(dir.path()).unwrap().check_for_updates,
                loaded
            );
        }
    }

    #[test]
    fn a_damaged_or_newer_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        for kept in ["{ not json", r#"{"version": 99}"#] {
            std::fs::write(&path, kept).unwrap();
            assert!(Settings::load(dir.path()).is_err());
            assert!(everything_changed().save(dir.path()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
        }
    }

    #[test]
    fn a_refused_name_counts_as_no_profile_and_volumes_stay_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = everything_changed();
        settings.profile.as_mut().unwrap().name = "   ".into();
        settings.audio.voice.output_volume = 100.0;
        settings.audio.voice.cue_volume = -1.0;
        settings.save(dir.path()).unwrap();
        let loaded = Settings::load(dir.path()).unwrap();
        assert_eq!(loaded.profile, None);
        assert_eq!(loaded.audio.voice.output_volume, MAX_VOLUME);
        assert_eq!(loaded.audio.voice.cue_volume, 0.0);
    }
}
