use throcc_client_core::media::audio::SAMPLES_PER_BLOCK;
use throcc_client_core::media::audio::cleanup::Cleanup;

const BLOCKS_PER_SECOND: usize = 100;
/// Measured against this room at 53 dB converged, 16 dB through double talk, and
/// 2.2 dB of near-end loss. The floors sit below those, so a regression trips
/// them and an ordinary version bump does not.
const CONVERGED_ERLE_FLOOR: f32 = 35.0;
const DOUBLE_TALK_ERLE_FLOOR: f32 = 10.0;
const NEAR_END_LOSS_FLOOR: f32 = -6.0;

/// A loudspeaker in a room: a delay, a few decaying reflections, and a `tanh`
/// for the speaker's own nonlinearity, which is what separates a real canceller
/// from a least-squares toy.
struct Room {
    history: Vec<f32>,
}

impl Room {
    const DELAY_SAMPLES: usize = 48 * 35;
    const REFLECTIONS: [(usize, f32); 4] = [(0, 0.35), (480, 0.18), (1_100, 0.09), (2_400, 0.04)];

    fn new() -> Self {
        Self {
            history: vec![0.0; Self::DELAY_SAMPLES + 4_000],
        }
    }

    /// What the microphone hears of a block that was just played.
    fn echo_of(&mut self, played: &[f32; SAMPLES_PER_BLOCK]) -> [f32; SAMPLES_PER_BLOCK] {
        self.history.extend_from_slice(played);
        let heard_from = self.history.len() - SAMPLES_PER_BLOCK - Self::DELAY_SAMPLES;

        let mut echo = [0.0f32; SAMPLES_PER_BLOCK];
        for (sample, echoed) in echo.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (offset, gain) in Self::REFLECTIONS {
                if let Some(index) = (heard_from + sample).checked_sub(offset) {
                    sum += gain * self.history[index];
                }
            }
            *echoed = (sum * 1.6).tanh() * 0.7;
        }

        let keep = Self::DELAY_SAMPLES + 4_000 + SAMPLES_PER_BLOCK;
        if self.history.len() > keep {
            self.history.drain(..self.history.len() - keep);
        }
        echo
    }
}

fn block_of(shape: impl Fn(usize) -> f32, from: usize) -> [f32; SAMPLES_PER_BLOCK] {
    let mut block = [0.0f32; SAMPLES_PER_BLOCK];
    for (sample, value) in block.iter_mut().enumerate() {
        *value = shape(from + sample);
    }
    block
}

fn far_end(index: usize) -> f32 {
    let time = index as f32 / 48_000.0;
    0.5 * ((time * 320.0 * std::f32::consts::TAU).sin() * 0.6
        + (time * 780.0 * std::f32::consts::TAU).sin() * 0.3
        + (time * 1_900.0 * std::f32::consts::TAU).sin() * 0.1)
}

fn near_end(index: usize) -> f32 {
    let time = index as f32 / 48_000.0;
    0.35 * (time * 210.0 * std::f32::consts::TAU).sin() * (1.0 + (time * 3.0).sin() * 0.4)
}

fn power(block: &[f32]) -> f32 {
    block.iter().map(|sample| sample * sample).sum::<f32>() / block.len() as f32
}

fn decibels(ratio: f32) -> f32 {
    10.0 * ratio.max(1e-20).log10()
}

/// Runs the chain against the room for `seconds`, with the near-end talker
/// active over `talking`. Returns the echo return loss enhancement and how much
/// of the near-end survived.
fn measure(seconds: usize, talking: std::ops::Range<usize>) -> (f32, f32) {
    let mut cleanup = Cleanup::new();
    let mut room = Room::new();

    let mut echo_power = 0.0;
    let mut residual_power = 0.0;
    let mut near_in_power = 0.0;
    let mut near_out_power = 0.0;
    let mut measured_blocks = 0.0f32;
    let mut talking_blocks = 0.0f32;

    for block in 0..seconds * BLOCKS_PER_SECOND {
        let from = block * SAMPLES_PER_BLOCK;
        let played = block_of(far_end, from);
        cleanup.played(&played);

        let echo = room.echo_of(&played);
        let near = if talking.contains(&block) {
            block_of(near_end, from)
        } else {
            [0.0; SAMPLES_PER_BLOCK]
        };

        let mut captured = [0.0f32; SAMPLES_PER_BLOCK];
        for (sample, value) in captured.iter_mut().enumerate() {
            *value = echo[sample] + near[sample];
        }
        cleanup.captured(&mut captured);

        // The first half second is convergence, and is not what is being measured.
        if block < BLOCKS_PER_SECOND / 2 {
            continue;
        }
        if talking.contains(&block) {
            near_in_power += power(&near);
            near_out_power += power(&captured);
            talking_blocks += 1.0;
        } else {
            echo_power += power(&echo);
            residual_power += power(&captured);
            measured_blocks += 1.0;
        }
    }

    let erle = decibels(echo_power / measured_blocks.max(1.0))
        - decibels(residual_power / measured_blocks.max(1.0));
    let near_end_retained = decibels(near_out_power / talking_blocks.max(1.0))
        - decibels(near_in_power / talking_blocks.max(1.0));
    (erle, near_end_retained)
}

#[test]
fn the_echo_canceller_removes_the_room() {
    let (erle, _) = measure(3, 0..0);
    println!("converged ERLE: {erle:.1} dB");
    assert!(
        erle > CONVERGED_ERLE_FLOOR,
        "the canceller left {erle:.1} dB of echo return loss, floor is {CONVERGED_ERLE_FLOOR}"
    );
}

#[test]
fn the_near_end_talker_survives_double_talk() {
    let (erle, near_end_retained) = measure(4, 200..300);
    println!("ERLE {erle:.1} dB, near end retained {near_end_retained:.1} dB");
    assert!(
        erle > DOUBLE_TALK_ERLE_FLOOR,
        "double talk cost too much cancellation: {erle:.1} dB"
    );
    assert!(
        near_end_retained > NEAR_END_LOSS_FLOOR,
        "the near-end talker lost {near_end_retained:.1} dB, floor is {NEAR_END_LOSS_FLOOR}"
    );
}
