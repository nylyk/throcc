use std::ffi::c_void;
use std::sync::Arc;

use jni::sys::{JNIEnv, jclass, jfloatArray, jlong, jobject};
use throcc_client_core::media::capture::android::{self as capture, Screen};
use throcc_client_core::media::share_audio::STEREO_FRAME;

use crate::{Command, Core, ShareChoice};

/// The screen the person agreed to share, which the Android app holds.
#[uniffi::export(with_foreign)]
pub trait ScreenSource: Send + Sync {
    /// Starts drawing the screen into the `ANativeWindow` at `window`, `width`×`height`
    /// pixels, and recording its sound when the share has audio. False if it can't.
    fn show(&self, window: u64, width: u32, height: u32) -> bool;
    /// Stops drawing and recording, and gives up the screen.
    fn stop(&self);
}

#[uniffi::export]
impl Core {
    /// Shares `screen`, `width`×`height` pixels, as `choice` asks, with its sound if `audio`.
    pub fn share_screen(
        &self,
        screen: Arc<dyn ScreenSource>,
        width: u32,
        height: u32,
        choice: ShareChoice,
        audio: bool,
    ) {
        capture::offer(Arc::new(AppScreen(screen)), width as usize, height as usize);
        self.command(Command::StartShare { choice, audio });
    }
}

struct AppScreen(Arc<dyn ScreenSource>);

impl Screen for AppScreen {
    fn show(&self, window: u64, width: u32, height: u32) -> bool {
        self.0.show(window, width, height)
    }

    fn stop(&self) {
        self.0.stop();
    }
}

/// `ScreenShare.surface` in the Android app: a `Surface` for the `ANativeWindow` that
/// `ScreenSource.show` was handed.
///
/// # Safety
/// Only the JVM calls this, with its environment and a live `ANativeWindow*`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_cc_thro_throcc_ScreenShare_surface(
    env: *mut JNIEnv,
    _class: jclass,
    window: jlong,
) -> jobject {
    #[link(name = "android")]
    unsafe extern "C" {
        fn ANativeWindow_toSurface(env: *mut JNIEnv, window: *mut c_void) -> jobject;
    }
    // SAFETY: the JVM calls this with its environment, and the window is live while the
    // share's encoder is, which outlasts this call.
    unsafe { ANativeWindow_toSurface(env, window as usize as *mut c_void) }
}

/// `ScreenShare.pushSound` in the Android app: 20 ms of the screen's sound, interleaved
/// stereo, whose first frame was recorded at `at`, in nanoseconds on `CLOCK_MONOTONIC`.
/// Never blocks or allocates.
///
/// # Safety
/// Only the JVM calls this, with its environment and a float array.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_cc_thro_throcc_ScreenShare_pushSound(
    env: *mut JNIEnv,
    _class: jclass,
    samples: jfloatArray,
    at: jlong,
) {
    let mut sound = [0.0f32; STEREO_FRAME];
    // SAFETY: the JVM calls this with its environment and a live float array, read only
    // when it holds exactly the samples `sound` has room for.
    unsafe {
        let jni = (**env).v1_1;
        let len = (jni.GetArrayLength)(env, samples);
        if usize::try_from(len) != Ok(STEREO_FRAME) {
            return;
        }
        (jni.GetFloatArrayRegion)(env, samples, 0, len, sound.as_mut_ptr());
    }
    capture::push_sound(&sound, at);
}
