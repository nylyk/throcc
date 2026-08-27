use std::collections::VecDeque;
use std::sync::Mutex;

use sonora::config::{EchoCanceller, GainController2, HighPassFilter, NoiseSuppression};
use sonora::{AudioProcessing, Config, StreamConfig};

use crate::media::audio::{SAMPLE_RATE, SAMPLES_PER_BLOCK};

const REFERENCE_BLOCKS: usize = 8;

/// The mixed playout, carried from the output callback to the capture callback.
/// The canceller has to estimate against what the device actually played.
#[derive(Default)]
pub struct RenderReference {
    blocks: Mutex<VecDeque<[f32; SAMPLES_PER_BLOCK]>>,
}

impl RenderReference {
    /// The oldest block is dropped rather than the newest: an estimate against
    /// stale playout is worse than one against a gap.
    pub fn played(&self, block: &[f32]) {
        let mut kept = [0.0f32; SAMPLES_PER_BLOCK];
        let filled = block.len().min(SAMPLES_PER_BLOCK);
        kept[..filled].copy_from_slice(&block[..filled]);

        let mut blocks = self.blocks.lock().expect("reference mutex poisoned");
        if blocks.len() == REFERENCE_BLOCKS {
            blocks.pop_front();
        }
        blocks.push_back(kept);
    }

    pub fn take(&self) -> Option<[f32; SAMPLES_PER_BLOCK]> {
        self.blocks
            .lock()
            .expect("reference mutex poisoned")
            .pop_front()
    }
}

/// Echo cancellation, noise suppression, gain control and the high-pass filter,
/// in the order the module itself applies them.
pub struct Cleanup {
    processing: AudioProcessing,
    processed: [f32; SAMPLES_PER_BLOCK],
}

impl Cleanup {
    pub fn new() -> Self {
        let config = Config {
            echo_canceller: Some(EchoCanceller::default()),
            noise_suppression: Some(NoiseSuppression::default()),
            gain_controller2: Some(GainController2::default()),
            high_pass_filter: Some(HighPassFilter::default()),
            ..Config::default()
        };

        let stream = StreamConfig::new(SAMPLE_RATE, 1);
        Self {
            processing: AudioProcessing::builder()
                .config(config)
                .capture_config(stream)
                .render_config(stream)
                .build(),
            processed: [0.0; SAMPLES_PER_BLOCK],
        }
    }

    /// One 10 ms block of what was played out. The chain's frame size is fixed,
    /// so a caller that batches two blocks silently processes only one of them.
    pub fn played(&mut self, block: &[f32; SAMPLES_PER_BLOCK]) {
        let mut discarded = [0.0f32; SAMPLES_PER_BLOCK];
        if let Err(e) = self
            .processing
            .process_render_f32(&[block], &mut [&mut discarded])
        {
            tracing::debug!(error = ?e, "the processing chain refused a played block");
        }
    }

    /// One 10 ms block of capture, cleaned up in place.
    pub fn captured(&mut self, block: &mut [f32; SAMPLES_PER_BLOCK]) {
        let mut processed = self.processed;
        match self
            .processing
            .process_capture_f32(&[block], &mut [&mut processed])
        {
            Ok(()) => block.copy_from_slice(&processed),
            Err(e) => tracing::debug!(error = ?e, "the processing chain refused a captured block"),
        }
    }
}

impl Default for Cleanup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_queue_keeps_the_newest_blocks() {
        let reference = RenderReference::default();
        for block in 0..REFERENCE_BLOCKS + 2 {
            reference.played(&[block as f32; SAMPLES_PER_BLOCK]);
        }

        let oldest = reference.take().expect("a block");
        assert_eq!(oldest[0], 2.0, "the two oldest blocks were dropped");
        assert_eq!(
            reference.blocks.lock().unwrap().len(),
            REFERENCE_BLOCKS - 1,
            "the queue never grows past its cap"
        );
    }

    #[test]
    fn silence_stays_silent_through_the_chain() {
        let mut cleanup = Cleanup::new();
        let mut block = [0.0f32; SAMPLES_PER_BLOCK];
        for _ in 0..10 {
            cleanup.played(&[0.0; SAMPLES_PER_BLOCK]);
            cleanup.captured(&mut block);
        }
        assert!(
            block.iter().all(|sample| sample.abs() < 0.01),
            "the chain must not invent signal"
        );
    }
}
