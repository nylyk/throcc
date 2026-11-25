#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use throcc_video::View;
use throcc_video::frame::VideoFrame;

use super::clock::{ShareClock, clock_nanos};
use super::{Capture, CaptureConfig, CaptureError, pack_shape, unpack_shape};
use crate::media::share_audio::STEREO_FRAME;

/// 20 ms buffers of the screen's sound waiting for the share thread.
const AUDIO_RING: usize = 32;
const NANOS: i64 = 1_000_000_000;

/// The app's side of a screen share, which holds the screen the person agreed to share.
pub trait Screen: Send + Sync {
    /// Draws the screen into the `ANativeWindow` at `window`, `width`×`height` pixels, from
    /// now on, and starts recording its sound when the share has audio. Called again with a
    /// new window when the screen turned. The app takes a reference of its own to the window.
    /// False if it can't.
    fn show(&self, window: u64, width: u32, height: u32) -> bool;
    /// Stops drawing and recording, and gives up the screen.
    fn stop(&self);
}

/// The screen the next share captures, offered by the app.
struct Offer {
    screen: Arc<dyn Screen>,
    shape: (usize, usize),
}

static OFFERED: Mutex<Option<Offer>> = Mutex::new(None);

/// 20 ms of interleaved stereo, with when its first frame was recorded.
struct Sound {
    at: i64,
    samples: [f32; STEREO_FRAME],
}

/// Where the screen's sound goes while a share records it, and the count of what it dropped.
type SoundIn = (rtrb::Producer<Sound>, Arc<AtomicU64>);

static SOUND_IN: Mutex<Option<SoundIn>> = Mutex::new(None);

/// The shape of the screen a share shows, as the app last told, packed by [`pack_shape`].
static SHAPE: AtomicU64 = AtomicU64::new(0);

fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Offers `screen`, `width`×`height` pixels, to the next share. An offer no share took yet
/// is stopped.
pub fn offer(screen: Arc<dyn Screen>, width: usize, height: usize) {
    let offer = Offer {
        screen,
        shape: (width, height),
    };
    if let Some(old) = lock(&OFFERED).replace(offer) {
        old.screen.stop();
    }
}

/// Tells a running share that the screen turned to `width`×`height` pixels.
pub fn turned(width: usize, height: usize) {
    SHAPE.store(pack_shape((width, height)), Ordering::Relaxed);
}

/// Adds 20 ms of the screen's sound, whose first frame was recorded at `at`, in nanoseconds
/// on `CLOCK_MONOTONIC`. Drops it while no share records, or when the ring is full. Never
/// blocks or allocates.
pub fn push_sound(samples: &[f32; STEREO_FRAME], at: i64) {
    let Ok(mut slot) = SOUND_IN.try_lock() else {
        return;
    };
    let Some((ring, dropped)) = slot.as_mut() else {
        return;
    };
    let sound = Sound {
        at,
        samples: *samples,
    };
    if ring.push(sound).is_err() {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct AndroidCapture {
    screen: Arc<dyn Screen>,
    sound: Option<rtrb::Consumer<Sound>>,
    dropped: Arc<AtomicU64>,
    /// Counts from `CLOCK_MONOTONIC`, in nanoseconds.
    clock: ShareClock,
}

/// Takes the screen the app offered, which it draws once the share's encoder is open.
pub fn start(config: CaptureConfig) -> Result<AndroidCapture, CaptureError> {
    let offer = lock(&OFFERED).take().ok_or(CaptureError::Cancelled)?;
    SHAPE.store(pack_shape(offer.shape), Ordering::Relaxed);
    let dropped = Arc::new(AtomicU64::new(0));
    let sound = config.audio.then(|| {
        let (producer, consumer) = rtrb::RingBuffer::new(AUDIO_RING);
        *lock(&SOUND_IN) = Some((producer, dropped.clone()));
        consumer
    });
    Ok(AndroidCapture {
        screen: offer.screen,
        sound,
        dropped,
        clock: ShareClock::new(NANOS, || clock_nanos(libc::CLOCK_MONOTONIC)),
    })
}

impl Capture for AndroidCapture {
    fn next_frame(&mut self, _into: &mut VideoFrame) -> bool {
        false
    }

    fn shape(&self) -> Option<(usize, usize)> {
        unpack_shape(SHAPE.load(Ordering::Relaxed))
    }

    fn draw_into(
        &mut self,
        window: &View,
        (width, height): (usize, usize),
    ) -> Result<(), CaptureError> {
        if self
            .screen
            .show(window.as_raw(), width as u32, height as u32)
        {
            Ok(())
        } else {
            Err(CaptureError::Failed("the screen can't be drawn".into()))
        }
    }

    fn stamp(&mut self, at: i64) -> Option<u32> {
        self.clock.video(at)
    }

    fn repeat_timestamp(&mut self) -> Option<u32> {
        self.clock.repeat()
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn captures_audio(&self) -> bool {
        self.sound.is_some()
    }

    fn next_audio(&mut self, into: &mut [f32; STEREO_FRAME]) -> Option<u32> {
        let sound = self.sound.as_mut()?;
        self.clock.next_audio(into, |clock| {
            let Sound { at, samples } = sound.pop().ok()?;
            clock.push_audio(at, samples);
            Some(())
        })
    }
}

impl Drop for AndroidCapture {
    fn drop(&mut self) {
        let mut slot = lock(&SOUND_IN);
        if slot
            .as_ref()
            .is_some_and(|(_, dropped)| Arc::ptr_eq(dropped, &self.dropped))
        {
            *slot = None;
        }
        drop(slot);
        self.screen.stop();
    }
}
