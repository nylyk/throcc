use throcc_client_core::media::voice;
use throcc_client_core::settings::{
    self, DEFAULT_GATE_DBFS, QUIETEST_GATE_DBFS, Transmit, VoiceSettings,
};
use throcc_client_core::state::MAX_VOLUME;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NoiseSuppression {
    Off,
    Low,
    Moderate,
    High,
}

/// The voice settings a phone offers.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct Voice {
    /// Processed mic level (dBFS) above which the mic sends, from `quietest_gate_dbfs()`
    /// to 0.
    pub sensitivity_dbfs: f32,
    /// The mic sends only while the speech detector also hears speech.
    pub speech_only: bool,
    pub noise_suppression: NoiseSuppression,
    pub automatic_gain: bool,
    /// Gain on the mic before processing, where 1 keeps it as it is.
    pub input_volume: f32,
    /// Gain on everything played, where 1 plays it as it arrives.
    pub output_volume: f32,
}

impl Voice {
    pub(crate) fn of(settings: &VoiceSettings) -> Self {
        Self {
            sensitivity_dbfs: match settings.transmit {
                Transmit::VoiceActivity { threshold_dbfs } => threshold_dbfs,
                Transmit::PushToTalk => DEFAULT_GATE_DBFS,
            },
            speech_only: settings.speech_only,
            noise_suppression: match settings.noise_suppression {
                None => NoiseSuppression::Off,
                Some(settings::NoiseSuppression::Low) => NoiseSuppression::Low,
                Some(settings::NoiseSuppression::Moderate) => NoiseSuppression::Moderate,
                Some(settings::NoiseSuppression::High) => NoiseSuppression::High,
            },
            automatic_gain: settings.automatic_gain,
            input_volume: settings.input_volume,
            output_volume: settings.output_volume,
        }
    }

    /// Puts these choices into `settings`, with voice activation and every value in range. A
    /// value that is no number leaves the setting as it was.
    pub(crate) fn apply_to(self, settings: &mut VoiceSettings) {
        let kept = Self::of(settings);
        settings.transmit = Transmit::VoiceActivity {
            threshold_dbfs: in_range(
                self.sensitivity_dbfs,
                QUIETEST_GATE_DBFS..=0.0,
                kept.sensitivity_dbfs,
            ),
        };
        settings.speech_only = self.speech_only;
        settings.noise_suppression = match self.noise_suppression {
            NoiseSuppression::Off => None,
            NoiseSuppression::Low => Some(settings::NoiseSuppression::Low),
            NoiseSuppression::Moderate => Some(settings::NoiseSuppression::Moderate),
            NoiseSuppression::High => Some(settings::NoiseSuppression::High),
        };
        settings.automatic_gain = self.automatic_gain;
        settings.input_volume = in_range(self.input_volume, 0.0..=MAX_VOLUME, kept.input_volume);
        settings.output_volume = in_range(self.output_volume, 0.0..=MAX_VOLUME, kept.output_volume);
    }
}

fn in_range(value: f32, range: std::ops::RangeInclusive<f32>, kept: f32) -> f32 {
    if value.is_nan() {
        kept
    } else {
        value.clamp(*range.start(), *range.end())
    }
}

/// The mic level after processing, and whether the voice gate lets it through.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct MicLevel {
    pub dbfs: f32,
    pub gate_open: bool,
}

impl MicLevel {
    /// The level, unless the mic delivered nothing to measure.
    pub(crate) fn of(level: voice::MicLevel) -> Option<Self> {
        level.dbfs.is_finite().then_some(Self {
            dbfs: level.dbfs,
            gate_open: level.gate_open,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_round_trips_and_lands_in_range_as_voice_activation() {
        let mut settings = VoiceSettings {
            transmit: Transmit::PushToTalk,
            ..VoiceSettings::default()
        };
        let voice = Voice {
            sensitivity_dbfs: -42.0,
            speech_only: true,
            noise_suppression: NoiseSuppression::Off,
            automatic_gain: false,
            input_volume: 1.5,
            output_volume: 0.5,
        };
        voice.apply_to(&mut settings);
        assert_eq!(Voice::of(&settings), voice);

        let wild = Voice {
            sensitivity_dbfs: -200.0,
            input_volume: 99.0,
            output_volume: -1.0,
            ..voice
        };
        wild.apply_to(&mut settings);
        assert_eq!(
            settings.transmit,
            Transmit::VoiceActivity {
                threshold_dbfs: QUIETEST_GATE_DBFS
            }
        );
        assert_eq!(
            (settings.input_volume, settings.output_volume),
            (MAX_VOLUME, 0.0)
        );

        let no_number = Voice {
            sensitivity_dbfs: f32::NAN,
            input_volume: f32::NAN,
            output_volume: f32::NAN,
            ..voice
        };
        no_number.apply_to(&mut settings);
        assert_eq!(
            (
                Voice::of(&settings).sensitivity_dbfs,
                settings.input_volume,
                settings.output_volume
            ),
            (QUIETEST_GATE_DBFS, MAX_VOLUME, 0.0)
        );
    }
}
