//! AV1 through dav1d.

use std::collections::VecDeque;
use std::mem::MaybeUninit;

use dav1d_sys as dav1d;
use dav1d_sys::{DAV1D_PIXEL_LAYOUT_I420, Dav1dContext, Dav1dData, Dav1dPicture, Dav1dSettings};

use super::{Plane, i420_to_nv12};
use crate::frame::VideoFrame;
use crate::{DecodeError, MaxSize};

/// dav1d's `-EAGAIN`: it wants pictures taken before it takes more data, or has none yet.
const AGAIN: i32 = -(libc::EAGAIN);

/// An AV1 decoder with frame threads, handing out its pictures in order.
pub struct Decoder {
    ctx: *mut Dav1dContext,
    max_size: MaxSize,
    /// Pictures taken out of dav1d and not yet handed out, oldest first.
    ready: VecDeque<VideoFrame>,
}

// SAFETY: the context is used through `&mut self` only, one thread at a time; dav1d's own
// threads synchronise inside it.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(max_size: MaxSize) -> Result<Self, DecodeError> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: dav1d_default_settings writes every field.
        let mut settings = unsafe {
            dav1d::dav1d_default_settings(settings.as_mut_ptr());
            settings.assume_init()
        };
        settings.n_threads =
            std::thread::available_parallelism().map_or(1, |n| n.get().min(8)) as i32;
        settings.frame_size_limit = u32::try_from(max_size.pixels()).unwrap_or(u32::MAX);
        let mut ctx = std::ptr::null_mut();
        // SAFETY: both pointers are to live locals.
        let result = unsafe { dav1d::dav1d_open(&mut ctx, &settings) };
        if result != 0 || ctx.is_null() {
            return Err(DecodeError::Dav1d(result));
        }
        Ok(Self {
            ctx,
            max_size,
            ready: VecDeque::new(),
        })
    }

    /// Decodes one temporal unit and hands out the oldest picture ready, if any. Frame
    /// threads keep a few pictures in flight; [`Decoder::drain`] brings them out.
    pub fn decode(
        &mut self,
        unit: &[u8],
        timestamp: u32,
        mut spare: Option<VideoFrame>,
    ) -> Result<Option<VideoFrame>, DecodeError> {
        // SAFETY: an all-zero Dav1dData is dav1d's empty data.
        let mut data: Dav1dData = unsafe { std::mem::zeroed() };
        // SAFETY: dav1d_data_create allocates `len` bytes for `data` and returns them.
        let buffer = unsafe { dav1d::dav1d_data_create(&mut data, unit.len()) };
        if buffer.is_null() {
            return Err(DecodeError::Dav1d(-libc::ENOMEM));
        }
        // SAFETY: `buffer` has room for `unit`.
        unsafe { std::ptr::copy_nonoverlapping(unit.as_ptr(), buffer, unit.len()) };
        data.m.timestamp = i64::from(timestamp);
        let sent = self.send(&mut data, &mut spare);
        if data.sz > 0 {
            // SAFETY: what dav1d didn't take is still ours to release.
            unsafe { dav1d::dav1d_data_unref(&mut data) };
        }
        sent?;
        // Only after this call does the next one wait for pictures in flight, which
        // `drain` relies on.
        if let Some(frame) = self.take_picture(&mut spare)? {
            self.ready.push_back(frame);
        }
        Ok(self.ready.pop_front())
    }

    /// Hands `data` to dav1d. While dav1d still holds data from before, pictures come out
    /// first, waited for if none is ready.
    fn send(
        &mut self,
        data: &mut Dav1dData,
        spare: &mut Option<VideoFrame>,
    ) -> Result<(), DecodeError> {
        let mut waited = false;
        loop {
            // SAFETY: the context is open and `data` is live.
            match unsafe { dav1d::dav1d_send_data(self.ctx, data) } {
                0 => return Ok(()),
                AGAIN => match self.take_picture(spare)? {
                    Some(frame) => self.ready.push_back(frame),
                    None if waited => return Err(DecodeError::Dav1d(AGAIN)),
                    None => waited = true,
                },
                error => return Err(DecodeError::Dav1d(error)),
            }
        }
    }

    /// Hands out the next picture, waiting for one still in flight. `None` once there are
    /// none.
    pub fn drain(
        &mut self,
        mut spare: Option<VideoFrame>,
    ) -> Result<Option<VideoFrame>, DecodeError> {
        match self.ready.pop_front() {
            Some(frame) => Ok(Some(frame)),
            None => self.take_picture(&mut spare),
        }
    }

    /// Takes one picture: a ready one on the first call after data went in, and on later
    /// calls the next one in flight, waiting for it. Uses the planes of `spare` if it gets
    /// one.
    fn take_picture(
        &mut self,
        spare: &mut Option<VideoFrame>,
    ) -> Result<Option<VideoFrame>, DecodeError> {
        // SAFETY: an all-zero Dav1dPicture is dav1d's empty picture.
        let mut picture: Dav1dPicture = unsafe { std::mem::zeroed() };
        // SAFETY: the context is open and `picture` is a live, empty picture.
        let got = unsafe { dav1d::dav1d_get_picture(self.ctx, &mut picture) };
        match got {
            0 => {}
            AGAIN => return Ok(None),
            error => return Err(DecodeError::Dav1d(error)),
        }
        let copied = self.copy_out(&picture, spare.take());
        // SAFETY: the picture came from dav1d_get_picture and is released once.
        unsafe { dav1d::dav1d_picture_unref(&mut picture) };
        copied.map(Some)
    }

    fn copy_out(
        &self,
        picture: &Dav1dPicture,
        spare: Option<VideoFrame>,
    ) -> Result<VideoFrame, DecodeError> {
        let timestamp = picture.m.timestamp as u32;
        let (width, height) = (picture.p.w, picture.p.h);
        if !self.max_size.fits(width.into(), height.into()) {
            return Err(DecodeError::TooLarge(width, height));
        }
        if picture.p.layout != DAV1D_PIXEL_LAYOUT_I420 || picture.p.bpc != 8 {
            return Err(DecodeError::Format(picture.p.layout as i32));
        }
        let plane = |index: usize, stride: isize| {
            let data = picture.data[index].cast::<u8>().cast_const();
            (!data.is_null()).then_some(Plane {
                data,
                stride: stride as usize,
            })
        };
        let (Some(y), Some(u), Some(v)) = (
            plane(0, picture.stride[0]),
            plane(1, picture.stride[1]),
            plane(2, picture.stride[1]),
        ) else {
            return Err(DecodeError::Format(picture.p.layout as i32));
        };
        let size = (width as usize, height as usize);
        // SAFETY: an 8-bit I420 picture holds its luma plane at `stride[0]` and both chroma
        // planes at `stride[1]`, for its whole size.
        Ok(unsafe { i420_to_nv12([y, u, v], size, timestamp, spare) })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context came from dav1d_open and is closed once.
        unsafe { dav1d::dav1d_close(&mut self.ctx) };
    }
}

/// The temporal units of an IVF file.
#[cfg(test)]
pub(crate) fn ivf_units(file: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut rest = &file[32..];
    while rest.len() >= 12 {
        let size = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        units.push(&rest[12..12 + size]);
        rest = &rest[12 + size..];
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::luma_psnr;

    /// Twelve 320×180 test patterns in AV1, made with aomenc, a keyframe every six.
    const PATTERN: &[u8] = include_bytes!("../../testdata/pattern.ivf");

    #[test]
    fn av1_pictures_come_out_in_order_with_their_timestamps() {
        let mut decoder = Decoder::new(MaxSize::ANY).unwrap();
        let mut shown = Vec::new();
        for (n, unit) in ivf_units(PATTERN).into_iter().enumerate() {
            let timestamp = n as u32 * 3000;
            let decoded = decoder.decode(unit, timestamp, None).unwrap();
            let output = decoder.drain(None).unwrap().or(decoded).unwrap();
            assert_eq!(
                (output.width, output.height, output.timestamp),
                (320, 180, timestamp)
            );
            let number = output.read_test_number();
            let reference = VideoFrame::test_pattern(320, 180, number, 0);
            shown.push((number, luma_psnr(&output, &reference)));
        }
        let numbers: Vec<u16> = shown.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, (0..12).collect::<Vec<_>>());
        let worst = shown.iter().map(|(_, p)| *p).fold(f64::INFINITY, f64::min);
        assert!(worst > 25.0, "picture quality {worst:.1} dB");
    }

    #[test]
    fn every_picture_comes_out_of_units_sent_back_to_back() {
        let mut decoder = Decoder::new(MaxSize::ANY).unwrap();
        let mut timestamps = Vec::new();
        for (n, unit) in ivf_units(PATTERN).into_iter().enumerate() {
            let decoded = decoder.decode(unit, n as u32 * 3000, None).unwrap();
            timestamps.extend(decoded.map(|frame| frame.timestamp));
        }
        while let Some(frame) = decoder.drain(None).unwrap() {
            timestamps.push(frame.timestamp);
        }
        assert_eq!(timestamps, (0..12).map(|n| n * 3000).collect::<Vec<u32>>());
    }

    #[test]
    fn a_picture_larger_than_allowed_is_refused() {
        let mut decoder = Decoder::new(MaxSize::new(320, 100)).unwrap();
        let unit = ivf_units(PATTERN)[0];
        assert!(matches!(
            decoder.decode(unit, 0, None),
            Err(DecodeError::TooLarge(320, 180) | DecodeError::Dav1d(_))
        ));
    }
}
