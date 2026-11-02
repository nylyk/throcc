use std::iter::{Chain, Copied};
use std::slice;

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
    /// Queues `frames` frames of interleaved stereo from `samples`, captured at `at`. Takes
    /// at most `frames` frames and pads with silence when `samples` runs short.
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
        // Filling every slot keeps the length in `buffers` true to the samples ring.
        slots.fill_from_iter(samples.into_iter().chain(std::iter::repeat(0.0)));
        self.buffers.push(Captured { at, frames }).is_ok()
    }
}

pub(super) struct AudioConsumer {
    buffers: rtrb::Consumer<Captured>,
    samples: rtrb::Consumer<f32>,
}

/// The interleaved stereo of one captured buffer.
pub(super) type Samples<'a> = Copied<Chain<slice::Iter<'a, f32>, slice::Iter<'a, f32>>>;

impl AudioConsumer {
    /// Hands the oldest captured buffer to `consume`, with the time it was captured at.
    /// The whole buffer leaves the ring however much of it `consume` reads.
    pub(super) fn pop<R>(&mut self, consume: impl FnOnce(i64, Samples<'_>) -> R) -> Option<R> {
        let Captured { at, frames } = self.buffers.pop().ok()?;
        let chunk = self.samples.read_chunk(frames * 2).ok()?;
        let (first, second) = chunk.as_slices();
        let consumed = consume(at, first.iter().chain(second).copied());
        chunk.commit_all();
        Some(consumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pop_all(consumer: &mut AudioConsumer) -> Option<(i64, Vec<f32>)> {
        consumer.pop(|at, samples| (at, samples.collect()))
    }

    #[test]
    fn buffers_arrive_whole_with_their_times() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(5, 3, (0..6).map(|i| i as f32)));
        assert!(producer.push(9, 1, [0.0; 2]));
        assert_eq!(
            pop_all(&mut consumer),
            Some((5, vec![0., 1., 2., 3., 4., 5.]))
        );
        assert_eq!(pop_all(&mut consumer), Some((9, vec![0.0; 2])));
        assert!(pop_all(&mut consumer).is_none());
    }

    #[test]
    fn a_full_ring_drops_the_newest_buffer() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(0, SAMPLE_RING / 2, std::iter::repeat(0.5)));
        assert!(!producer.push(1, 1, [0.0; 2]));
        assert_eq!(consumer.pop(|at, _| at), Some(0));
        assert!(consumer.pop(|at, _| at).is_none());
    }

    #[test]
    fn a_buffer_read_in_part_still_leaves_whole() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(1, 2, [1.0, 2.0, 3.0, 4.0]));
        assert!(producer.push(2, 1, [5.0, 6.0]));
        assert_eq!(
            consumer.pop(|_, mut samples| samples.next()),
            Some(Some(1.0))
        );
        assert_eq!(pop_all(&mut consumer), Some((2, vec![5.0, 6.0])));
    }

    #[test]
    fn a_short_buffer_is_padded_and_a_long_one_cut() {
        let (mut producer, mut consumer) = audio_ring();
        assert!(producer.push(1, 2, [1.0, 2.0, 3.0]));
        assert!(producer.push(2, 1, [5.0, 6.0, 7.0]));
        assert!(producer.push(3, 1, [8.0, 9.0]));
        assert_eq!(pop_all(&mut consumer), Some((1, vec![1.0, 2.0, 3.0, 0.0])));
        assert_eq!(pop_all(&mut consumer), Some((2, vec![5.0, 6.0])));
        assert_eq!(pop_all(&mut consumer), Some((3, vec![8.0, 9.0])));
    }
}
