use std::collections::VecDeque;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use ndk_sys as ndk;
use throcc_proto::messages::Codec;

use super::View;
use crate::MaxSize;

/// How long a decode waits for an input buffer, in microseconds.
const INPUT_WAIT_US: i64 = 10_000;
/// Decoded pictures handed out and not yet shown or dropped, at most. Past it the oldest is
/// dropped, so pictures held back for the share audio can't starve the codec of buffers.
const OUTSTANDING: usize = 3;

fn mime(codec: Codec) -> &'static CStr {
    match codec {
        Codec::H264 => c"video/avc",
        Codec::Hevc => c"video/hevc",
        Codec::Av1 => c"video/av01",
        Codec::Vp9 => c"video/x-vnd.on2.vp9",
    }
}

/// A started MediaCodec decoder rendering into a view, shared with the pictures it handed
/// out so they can be shown or dropped after the decoder moved on.
pub struct MediaCodec {
    shared: Arc<Shared>,
    /// The view the codec renders into, kept alive while it does.
    view: View,
    /// Pictures handed out and still outstanding, oldest first.
    outstanding: VecDeque<Weak<Slot>>,
    /// The size of the pictures the codec puts out.
    size: (usize, usize),
}

struct Shared {
    codec: Mutex<*mut ndk::AMediaCodec>,
}

// SAFETY: the codec pointer is only used under the mutex.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, *mut ndk::AMediaCodec> {
        self.codec.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands output buffer `index` back, rendering it into the view if `render` holds,
    /// unless the slot was handed back already or the codec is gone.
    fn release(&self, slot: &Slot, render: bool) {
        let codec = self.lock();
        if codec.is_null() || slot.released.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: the codec is live while the lock holds it non-null, and the index came
        // from it since it last started.
        unsafe { ndk::AMediaCodec_releaseOutputBuffer(*codec, slot.index, render) };
    }
}

/// One decoded picture in an output buffer of the codec.
struct Slot {
    index: usize,
    released: AtomicBool,
}

/// A decoded picture still in the codec, shown by rendering it into the view. Dropping it
/// drops the picture.
pub struct CodecFrame {
    shared: Arc<Shared>,
    slot: Arc<Slot>,
    pub timestamp: u32,
    pub width: usize,
    pub height: usize,
}

impl CodecFrame {
    /// Renders the picture into the view the codec decodes into.
    pub fn show(&self) {
        self.shared.release(&self.slot, true);
    }
}

impl Drop for CodecFrame {
    fn drop(&mut self) {
        self.shared.release(&self.slot, false);
    }
}

/// What the codec put out for one unit.
pub enum Output {
    Frame(CodecFrame),
    TooLarge(i32, i32),
}

impl MediaCodec {
    /// A started decoder for `codec` rendering into `view`, refusing pictures larger than
    /// `max_size`. `None` if the device has none or it doesn't take that size.
    pub fn open(codec: Codec, max_size: MaxSize, view: View) -> Option<Self> {
        // SAFETY: the MIME type is a C string literal.
        let raw = unsafe { ndk::AMediaCodec_createDecoderByType(mime(codec).as_ptr()) };
        if raw.is_null() {
            return None;
        }
        let shared = Arc::new(Shared {
            codec: Mutex::new(raw),
        });
        let (width, height) = max_size.sides();
        // SAFETY: the format is live until deleted below, and the keys are the NDK's.
        let configured = unsafe {
            let format = ndk::AMediaFormat_new();
            ndk::AMediaFormat_setString(format, ndk::AMEDIAFORMAT_KEY_MIME, mime(codec).as_ptr());
            for (key, value) in [
                (ndk::AMEDIAFORMAT_KEY_WIDTH, width),
                (ndk::AMEDIAFORMAT_KEY_HEIGHT, height),
                (ndk::AMEDIAFORMAT_KEY_MAX_WIDTH, width),
                (ndk::AMEDIAFORMAT_KEY_MAX_HEIGHT, height),
            ] {
                ndk::AMediaFormat_setInt32(format, key, value as i32);
            }
            // Realtime priority, and a picture out per unit in: no reordering, no batching.
            ndk::AMediaFormat_setInt32(format, ndk::AMEDIAFORMAT_KEY_PRIORITY, 0);
            ndk::AMediaFormat_setInt32(format, ndk::AMEDIAFORMAT_KEY_LOW_LATENCY, 1);
            let status =
                ndk::AMediaCodec_configure(raw, format, view.as_ptr(), std::ptr::null_mut(), 0);
            ndk::AMediaFormat_delete(format);
            status == ndk::media_status_t::AMEDIA_OK
                && ndk::AMediaCodec_start(raw) == ndk::media_status_t::AMEDIA_OK
        };
        let decoder = Self {
            shared,
            view,
            outstanding: VecDeque::new(),
            size: (0, 0),
        };
        configured.then_some(decoder)
    }

    /// Has the codec render into `view` from now on, without starting over.
    pub fn retarget(&mut self, view: View) -> bool {
        let codec = self.shared.lock();
        // SAFETY: the codec is live, and the view outlives its use: it is kept in `self`.
        let status = unsafe { ndk::AMediaCodec_setOutputSurface(*codec, view.as_ptr()) };
        drop(codec);
        if status != ndk::media_status_t::AMEDIA_OK {
            return false;
        }
        self.view = view;
        true
    }

    /// Queues one access unit, marked with `sequence` to find its timestamp again, and
    /// returns the newest picture that came out. False in `queued` if no input buffer
    /// was free.
    pub fn decode(
        &mut self,
        unit: &[u8],
        sequence: u64,
        max_size: MaxSize,
    ) -> Result<(bool, Vec<(u64, Output)>), i32> {
        let codec = *self.shared.lock();
        // SAFETY: the codec is live while `self` is; the input buffer is ours until queued.
        let queued = unsafe {
            let index = ndk::AMediaCodec_dequeueInputBuffer(codec, INPUT_WAIT_US);
            if index >= 0 {
                let mut size = 0usize;
                let buffer = ndk::AMediaCodec_getInputBuffer(codec, index as usize, &mut size);
                let len = unit.len().min(size);
                if !buffer.is_null() {
                    std::ptr::copy_nonoverlapping(unit.as_ptr(), buffer, len);
                }
                let status =
                    ndk::AMediaCodec_queueInputBuffer(codec, index as usize, 0, len, sequence, 0);
                if status != ndk::media_status_t::AMEDIA_OK {
                    return Err(status.0);
                }
                true
            } else {
                false
            }
        };
        let mut out = Vec::new();
        loop {
            // SAFETY: zeroed is how the call expects the struct, which it fills in.
            let mut info: ndk::AMediaCodecBufferInfo = unsafe { std::mem::zeroed() };
            // SAFETY: the codec is live.
            let index = unsafe { ndk::AMediaCodec_dequeueOutputBuffer(codec, &mut info, 0) };
            match index {
                index if index >= 0 => {
                    let sequence = info.presentationTimeUs as u64;
                    out.push((sequence, self.hand_out(index as usize, max_size)));
                }
                index if index == ndk::AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED as isize => {
                    self.size = output_size(codec);
                }
                index if index == ndk::AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED as isize => {}
                index if index == ndk::AMEDIACODEC_INFO_TRY_AGAIN_LATER as isize => break,
                error => return Err(error as i32),
            }
        }
        Ok((queued, out))
    }

    /// Wraps output buffer `index`, dropping the oldest picture still out if too many are.
    fn hand_out(&mut self, index: usize, max_size: MaxSize) -> Output {
        let slot = Arc::new(Slot {
            index,
            released: AtomicBool::new(false),
        });
        let (width, height) = self.size;
        if !max_size.fits(width as i64, height as i64) {
            self.shared.release(&slot, false);
            return Output::TooLarge(width as i32, height as i32);
        }
        self.outstanding.retain(|slot| slot.strong_count() > 0);
        while self.outstanding.len() >= OUTSTANDING {
            if let Some(oldest) = self.outstanding.pop_front().and_then(|slot| slot.upgrade()) {
                self.shared.release(&oldest, false);
            }
        }
        self.outstanding.push_back(Arc::downgrade(&slot));
        Output::Frame(CodecFrame {
            shared: self.shared.clone(),
            slot,
            timestamp: 0,
            width,
            height,
        })
    }
}

impl Drop for MediaCodec {
    fn drop(&mut self) {
        let mut codec = self.shared.lock();
        // SAFETY: the codec is live until here, and no picture uses it once it is null.
        unsafe {
            ndk::AMediaCodec_stop(*codec);
            ndk::AMediaCodec_delete(*codec);
        }
        *codec = std::ptr::null_mut();
    }
}

/// The width and height of the pictures `codec` puts out.
fn output_size(codec: *mut ndk::AMediaCodec) -> (usize, usize) {
    // SAFETY: the codec is live; the format it returns is ours to delete.
    unsafe {
        let format = ndk::AMediaCodec_getOutputFormat(codec);
        if format.is_null() {
            return (0, 0);
        }
        let read = |key: *const c_char| {
            let mut value = 0;
            ndk::AMediaFormat_getInt32(format, key, &mut value).then_some(value)
        };
        let size = (
            read(ndk::AMEDIAFORMAT_KEY_WIDTH),
            read(ndk::AMEDIAFORMAT_KEY_HEIGHT),
        );
        ndk::AMediaFormat_delete(format);
        match size {
            (Some(width), Some(height)) => (width.max(0) as usize, height.max(0) as usize),
            _ => (0, 0),
        }
    }
}
