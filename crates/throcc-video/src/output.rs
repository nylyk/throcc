//! Where a decoder shows its pictures itself, on the phones: the app hands over a view, an
//! Android `Surface` or an iOS `AVSampleBufferDisplayLayer`, and the decoder presents each
//! picture due into it. Elsewhere the renderer draws pictures from the share's `Picture`
//! and an output stays empty.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use self::none::View;
#[cfg(target_os = "android")]
pub use crate::android::View;
#[cfg(target_os = "ios")]
pub use crate::apple::View;

/// A watched share's view, which the app may set, replace or take away at any time. Clones
/// share it.
#[derive(Clone, Default)]
pub struct Output(Arc<Shared>);

#[derive(Default)]
struct Shared {
    view: Mutex<Option<View>>,
    /// Counts the changes of `view`, so a decoder notices one without comparing views.
    changes: AtomicU64,
}

impl Output {
    /// Shows pictures in `view` from now on, or nowhere.
    pub fn set(&self, view: Option<View>) {
        *self.0.view.lock().unwrap_or_else(PoisonError::into_inner) = view;
        self.0.changes.fetch_add(1, Ordering::Release);
    }

    /// How many times the view changed, which a decoder compares with what it last saw.
    pub fn changes(&self) -> u64 {
        self.0.changes.load(Ordering::Acquire)
    }

    /// Shows a still picture in the view, as sample screens do. True if there was a view to
    /// show it in.
    pub fn show(&self, picture: &crate::frame::VideoFrame) -> bool {
        #[cfg(target_os = "android")]
        return self.view().is_some_and(|view| view.show(picture));
        #[cfg(target_os = "ios")]
        return self.view().is_some_and(|view| {
            crate::PixelBuffer::holding(picture).is_some_and(|buffer| view.show(&buffer))
        });
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let _ = picture;
            false
        }
    }

    /// The current view.
    pub fn view(&self) -> Option<View>
    where
        View: Clone,
    {
        self.0
            .view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod none {
    /// Desktops draw pictures themselves; no view is ever handed over.
    #[derive(Clone)]
    pub enum View {}

    impl View {
        /// There are no views here.
        ///
        /// # Safety
        /// None; the signature matches the phones'.
        pub unsafe fn from_raw(_raw: u64) -> Option<Self> {
            None
        }
    }
}
