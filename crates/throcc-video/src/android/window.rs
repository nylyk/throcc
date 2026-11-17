use std::ffi::c_void;
use std::ptr::NonNull;

use ndk_sys as ndk;

use crate::frame::VideoFrame;

/// HAL_PIXEL_FORMAT_YV12: planar 4:2:0, Y then V then U, which every surface takes from
/// the CPU.
const YV12: i32 = 0x3231_5659;
/// Full-range BT.709: STANDARD_BT709 | TRANSFER_SMPTE_170M | RANGE_FULL, as every share
/// is encoded.
const FULL_RANGE_BT709: i32 = (1 << 16) | (3 << 22) | (1 << 27);

/// A surface the app handed over, as the `ANativeWindow` behind it. Clones share the window.
pub struct View(NonNull<ndk::ANativeWindow>);

// SAFETY: native windows are reference counted thread-safely, and buffers are queued to them
// from one thread at a time: the decoder's.
unsafe impl Send for View {}
// SAFETY: as above.
unsafe impl Sync for View {}

impl View {
    /// Takes over a reference to the `ANativeWindow` at `raw`, as `ANativeWindow_fromSurface`
    /// hands one out.
    ///
    /// # Safety
    /// `raw` must be a live `ANativeWindow*` whose reference the caller gives up, or null.
    pub unsafe fn from_raw(raw: u64) -> Option<Self> {
        NonNull::new(raw as usize as *mut ndk::ANativeWindow).map(Self)
    }

    pub(crate) fn as_ptr(&self) -> *mut ndk::ANativeWindow {
        self.0.as_ptr()
    }

    /// Copies `frame` into the next buffer of the window and shows it.
    pub(crate) fn show(&self, frame: &VideoFrame) -> bool {
        let window = self.as_ptr();
        let (width, height) = (frame.width as i32, frame.height as i32);
        // SAFETY: the window is live while `self` is.
        unsafe {
            if ndk::ANativeWindow_setBuffersGeometry(window, width, height, YV12) != 0 {
                return false;
            }
            ndk::ANativeWindow_setBuffersDataSpace(window, FULL_RANGE_BT709);
        }
        // SAFETY: zeroed is how the lock expects the struct, which it fills in.
        let mut buffer: ndk::ANativeWindow_Buffer = unsafe { std::mem::zeroed() };
        // SAFETY: the window is live, and a successful lock is posted below.
        if unsafe { ndk::ANativeWindow_lock(window, &mut buffer, std::ptr::null_mut()) } != 0 {
            return false;
        }
        let fits = buffer.width == width && buffer.height == height && buffer.format == YV12;
        if fits {
            // SAFETY: a locked YV12 buffer of the frame's size holds its planes at YV12's
            // strides.
            unsafe { write_yv12(frame, buffer.bits, buffer.stride as usize) };
        }
        // SAFETY: locked above.
        unsafe { ndk::ANativeWindow_unlockAndPost(window) };
        fits
    }
}

impl Clone for View {
    fn clone(&self) -> Self {
        // SAFETY: the window is live; the clone holds a reference of its own.
        unsafe { ndk::ANativeWindow_acquire(self.as_ptr()) };
        Self(self.0)
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: each `View` holds one reference.
        unsafe { ndk::ANativeWindow_release(self.as_ptr()) };
    }
}

/// Writes `frame` as YV12 into a buffer whose luma rows are `stride` pixels apart: chroma
/// rows are half that, rounded up to 16, and V comes before U.
///
/// # Safety
/// `bits` must hold a YV12 picture of the frame's size at that stride.
unsafe fn write_yv12(frame: &VideoFrame, bits: *mut c_void, stride: usize) {
    let (width, height) = (frame.width, frame.height);
    let chroma_stride = (stride / 2).next_multiple_of(16);
    let luma = bits.cast::<u8>();
    // SAFETY: the caller promises the layout.
    unsafe {
        let v_plane = luma.add(stride * height);
        let u_plane = v_plane.add(chroma_stride * height / 2);
        for (row, line) in frame.y.chunks_exact(width).take(height).enumerate() {
            std::ptr::copy_nonoverlapping(line.as_ptr(), luma.add(row * stride), width);
        }
        for (row, line) in frame.uv.chunks_exact(width).take(height / 2).enumerate() {
            let (v_row, u_row) = (
                v_plane.add(row * chroma_stride),
                u_plane.add(row * chroma_stride),
            );
            for (column, [u, v]) in line.as_chunks::<2>().0.iter().enumerate() {
                *u_row.add(column) = *u;
                *v_row.add(column) = *v;
            }
        }
    }
}
