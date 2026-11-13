//! The CoreFoundation dictionaries VideoToolbox and CoreVideo are configured with.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};

use super::FULL_RANGE_NV12;

pub type Dictionary = CFRetained<CFDictionary<CFString, CFType>>;

pub fn dictionary(entries: &[(&CFString, &CFType)]) -> Dictionary {
    let (keys, values): (Vec<&CFString>, Vec<&CFType>) = entries.iter().copied().unzip();
    CFDictionary::from_slices(&keys, &values)
}

pub fn number(value: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value)
}

pub fn boolean(value: bool) -> &'static CFBoolean {
    CFBoolean::new(value)
}

/// Full-range NV12 in IOSurface-backed GPU memory that Metal can draw, of `size` if given.
pub fn pixel_buffer(size: Option<(usize, usize)>) -> Dictionary {
    let format = number(i64::from(FULL_RANGE_NV12));
    let surface = dictionary(&[]);
    let size = size.map(|(width, height)| (number(width as i64), number(height as i64)));
    // SAFETY: CoreVideo's keys are immutable statics, set before main.
    let mut entries: Vec<(&CFString, &CFType)> = unsafe {
        vec![
            (kCVPixelBufferPixelFormatTypeKey, &format),
            (kCVPixelBufferIOSurfacePropertiesKey, &surface),
            (kCVPixelBufferMetalCompatibilityKey, boolean(true)),
        ]
    };
    if let Some((width, height)) = &size {
        // SAFETY: as above.
        unsafe {
            entries.push((kCVPixelBufferWidthKey, width));
            entries.push((kCVPixelBufferHeightKey, height));
        }
    }
    dictionary(&entries)
}
