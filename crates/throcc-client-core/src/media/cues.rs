use std::f32::consts::PI;

use super::audio::SAMPLE_RATE;
use crate::state::MAX_VOLUME;

/// A short sound played on this machine when something happens, never sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Join,
    Leave,
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    ConnectionLost,
    Reconnected,
    ShareStart,
    ShareStop,
}

const CUES: [Cue; 10] = [
    Cue::Join,
    Cue::Leave,
    Cue::Mute,
    Cue::Unmute,
    Cue::Deafen,
    Cue::Undeafen,
    Cue::ConnectionLost,
    Cue::Reconnected,
    Cue::ShareStart,
    Cue::ShareStop,
];

const A4: f32 = 440.0;
const D5: f32 = 587.33;
const F4: f32 = 349.23;
const B4: f32 = 493.88;
const E5: f32 = 659.25;
const FS5: f32 = 739.99;
const B5: f32 = 987.77;
/// Each note's harmonics as multiples of its frequency, with their amplitudes.
const PARTIALS: [(f32, f32); 3] = [(1.0, 1.0), (2.0, 0.18), (3.0, 0.06)];
/// The loudest sample of a cue, -20 dBFS.
const PEAK: f32 = 0.1;
/// The loudest sample of a chord, -16.5 dBFS, as loud on average as join.
const CHORD_PEAK: f32 = 0.15;
const ATTACK: f32 = 0.005;
const FADE_OUT: f32 = 0.01;

/// Notes struck one after another, each `gap` seconds after the last, ringing for `length`
/// seconds and fading by `decay`, the time to fall to 1/e, scaled to `peak`.
struct Shape {
    notes: &'static [f32],
    gap: f32,
    length: f32,
    decay: f32,
    peak: f32,
}

impl Cue {
    fn shape(self) -> Shape {
        let chime = |notes| Shape {
            notes,
            gap: 0.08,
            length: 0.2,
            decay: 0.04,
            peak: PEAK,
        };
        let tap = |notes| Shape {
            notes,
            gap: 0.085,
            length: 0.12,
            decay: 0.035,
            peak: PEAK,
        };
        let switch = |notes| Shape {
            notes,
            gap: 0.0,
            length: 0.16,
            decay: 0.05,
            peak: PEAK,
        };
        let chord = |notes| Shape {
            notes,
            gap: 0.0,
            length: 0.2,
            decay: 0.04,
            peak: CHORD_PEAK,
        };
        match self {
            Cue::Join | Cue::Reconnected => chime(&[A4, D5]),
            Cue::Leave => chime(&[D5, A4]),
            Cue::Mute => switch(&[A4]),
            Cue::Unmute => switch(&[D5]),
            Cue::Deafen => tap(&[A4, A4]),
            Cue::Undeafen => tap(&[D5, D5]),
            Cue::ConnectionLost => chime(&[D5, A4, F4]),
            Cue::ShareStart => chord(&[E5, B5]),
            Cue::ShareStop => chord(&[B4, FS5]),
        }
    }

    fn render(self) -> Vec<f32> {
        let Shape {
            notes,
            gap,
            length,
            decay,
            peak: target,
        } = self.shape();
        let rate = SAMPLE_RATE as f32;
        let note_len = (length * rate) as usize;
        let offset = (gap * rate) as usize;
        let mut out = vec![0.0f32; offset * notes.len().saturating_sub(1) + note_len];
        for (k, freq) in notes.iter().enumerate() {
            for (i, sample) in out.iter_mut().skip(k * offset).take(note_len).enumerate() {
                let t = i as f32 / rate;
                let envelope = if t < ATTACK {
                    0.5 - 0.5 * (PI * t / ATTACK).cos()
                } else {
                    (-(t - ATTACK) / decay).exp()
                };
                let fade_out = ((note_len - i) as f32 / (FADE_OUT * rate)).min(1.0);
                let wave: f32 = PARTIALS
                    .iter()
                    .map(|(ratio, amplitude)| amplitude * (2.0 * PI * freq * ratio * t).sin())
                    .sum();
                *sample += wave * envelope * fade_out;
            }
        }
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 0.0 {
            for sample in &mut out {
                *sample *= target / peak;
            }
        }
        out
    }
}

/// Every cue rendered once at 48 kHz, and the one playing.
pub struct CuePlayer {
    sounds: Vec<(Cue, Vec<f32>)>,
    playing: Option<Playing>,
}

struct Playing {
    cue: Cue,
    position: usize,
    gain: f32,
}

impl Default for CuePlayer {
    fn default() -> Self {
        Self {
            sounds: CUES.iter().map(|cue| (*cue, cue.render())).collect(),
            playing: None,
        }
    }
}

impl CuePlayer {
    /// Starts `cue` at `gain`, cutting off a different one. A cue already playing carries
    /// on, so a burst of the same event sounds once.
    pub fn play(&mut self, cue: Cue, gain: f32) {
        if self.playing.as_ref().is_some_and(|p| p.cue == cue) {
            return;
        }
        self.playing = Some(Playing {
            cue,
            position: 0,
            gain: gain.clamp(0.0, MAX_VOLUME),
        });
    }

    /// Adds the playing cue's next samples into `out`.
    pub fn mix(&mut self, out: &mut [f32]) {
        let Some(playing) = &mut self.playing else {
            return;
        };
        let Some((_, samples)) = self.sounds.iter().find(|(c, _)| *c == playing.cue) else {
            return;
        };
        let rest = samples.iter().skip(playing.position);
        for (o, s) in out.iter_mut().zip(rest) {
            *o += s * playing.gain;
        }
        playing.position += out.len();
        if playing.position >= samples.len() {
            self.playing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cue_peaks_as_shaped_and_ends_silent() {
        for cue in CUES {
            let samples = cue.render();
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                (peak - cue.shape().peak).abs() < 1e-6,
                "{cue:?} peaks at {peak}"
            );
            assert_eq!(samples.first(), Some(&0.0), "{cue:?}");
            assert!(samples.last().is_some_and(|s| s.abs() < 1e-4), "{cue:?}");
        }
    }

    #[test]
    fn cues_last_as_designed() {
        let ms = |cue: Cue| cue.render().len() * 1000 / SAMPLE_RATE as usize;
        assert_eq!(ms(Cue::Join), 280);
        assert_eq!(ms(Cue::Mute), 160);
        assert_eq!(ms(Cue::Deafen), 205);
        assert_eq!(ms(Cue::ConnectionLost), 360);
        assert_eq!(ms(Cue::ShareStart), 200);
    }

    #[test]
    fn a_cue_plays_to_its_end_and_a_repeat_does_not_restart_it() {
        let mut player = CuePlayer::default();
        player.play(Cue::Join, 1.0);
        let mut pull = [0.0f32; 480];
        player.mix(&mut pull);
        assert!(pull.iter().any(|s| *s != 0.0));
        player.play(Cue::Join, 1.0);
        assert_eq!(player.playing.as_ref().map(|p| p.position), Some(480));
        for _ in 0..30 {
            player.mix(&mut pull);
        }
        assert!(player.playing.is_none());
        let mut silent = [0.0f32; 480];
        player.mix(&mut silent);
        assert!(silent.iter().all(|s| *s == 0.0));
    }
}
