use rtrb::chunks::ReadChunk;

use crate::media::audio::SAMPLE_RATE;

/// Captured samples waiting for the share thread: one second of stereo.
const SAMPLE_RING: usize = SAMPLE_RATE as usize * 2;
/// Captured buffers waiting for the share thread.
const BUFFER_RING: usize = 64;

/// A ring that carries timestamped buffers of interleaved stereo from a capture thread to
/// the share thread. The samples go in one ring and each buffer's time and length in
/// another, so a buffer of any length crosses without allocating.
pub(super) fn audio_ring() -> (AudioProducer, AudioConsumer) {
    let (buffers_tx, buffers) = rtrb::RingBuffer::new(BUFFER_RING);
    let (samples_tx, samples) = rtrb::RingBuffer::new(SAMPLE_RING);
    (
        AudioProducer {
            buffers: buffers_tx,
            samples: samples_tx,
        },
        AudioConsumer { buffers, samples },
    )
}

/// One buffer in the sample ring and the time it was captured at, on the capture's clock.
#[derive(Clone, Copy)]
struct Captured {
    at: i64,
    frames: usize,
}

pub(super) struct AudioProducer {
    buffers: rtrb::Producer<Captured>,
    samples: rtrb::Producer<f32>,
}

impl AudioProducer {
    /// Queues `frames` frames of interleaved stereo from `samples`, captured at `at`.
    /// Never blocks or allocates; false when the rings are full and the buffer is dropped.
    pub(super) fn push(
        &mut self,
        at: i64,
        frames: usize,
        samples: impl IntoIterator<Item = f32>,
    ) -> bool {
        if frames == 0 {
            return true;
        }
        let slots = match self.samples.write_chunk_uninit(frames * 2) {
            Ok(slots) if !self.buffers.is_full() => slots,
            _ => return false,
        };
        let written = slots.fill_from_iter(samples);
        self.buffers
            .push(Captured {
                at,
                frames: written / 2,
            })
            .is_ok()
    }
}

pub(super) struct AudioConsumer {
    buffers: rtrb::Consumer<Captured>,
    samples: rtrb::Consumer<f32>,
}

impl AudioConsumer {
    /// The oldest captured buffer, interleaved stereo, with the time it was captured at.
    pub(super) fn pop(&mut self) -> Option<(i64, ReadChunk<'_, f32>)> {
        let Captured { at, frames } = self.buffers.pop().ok()?;
        let samples = self.samples.read_chunk(frames * 2).ok()?;
        Some((at, samples))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_arrive_whole_with_their_times() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(5, 3, (0..6).map(|i| i as f32)));
        assert!(producer.push(9, 1, [0.0; 2]));
        let (at, samples) = consumer.pop().unwrap();
        assert_eq!(at, 5);
        assert_eq!(
            samples.into_iter().collect::<Vec<_>>(),
            [0., 1., 2., 3., 4., 5.]
        );
        assert_eq!(consumer.pop().map(|(at, s)| (at, s.len())), Some((9, 2)));
        assert!(consumer.pop().is_none());
    }

    #[test]
    fn a_full_ring_drops_the_newest_buffer() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(0, SAMPLE_RING / 2, std::iter::repeat(0.5)));
        assert!(!producer.push(1, 1, [0.0; 2]));
        assert_eq!(consumer.pop().map(|(at, _)| at), Some(0));
        assert!(consumer.pop().is_none());
    }
}
