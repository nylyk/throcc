//! VP9 through libvpx: a realtime CBR encoder tuned for screen content, and a decoder.

use std::ffi::{CStr, c_int, c_uint};
use std::ptr;

use bytes::Bytes;
use vpx_sys as vpx;

use super::{Plane, i420_to_nv12, nv12_to_i420};
use crate::frame::VideoFrame;
use crate::{DecodeError, EncodeError, EncodedUnit, EncoderConfig, MaxSize};

/// A GOP cap of this many seconds bounds how long a picture stays broken without a request.
const GOP_SECONDS: u32 = 4;
/// libvpx's speed for realtime VP9: the fastest.
const CPU_USED: c_int = 8;
/// libvpx's `VP9E_CONTENT_SCREEN`.
const SCREEN_CONTENT: c_int = 1;

/// The message libvpx keeps for its last error on `ctx`.
fn last_error(ctx: &mut vpx::vpx_codec_ctx_t) -> String {
    // SAFETY: `ctx` is initialised; libvpx returns a static or context-owned C string.
    let message = unsafe { vpx::vpx_codec_error(ctx) };
    if message.is_null() {
        return "libvpx failed".into();
    }
    // SAFETY: checked non-null above, and NUL-terminated by libvpx.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

fn check(ctx: &mut vpx::vpx_codec_ctx_t, status: vpx::vpx_codec_err_t) -> Result<(), String> {
    if status == vpx::vpx_codec_err_t::VPX_CODEC_OK {
        Ok(())
    } else {
        Err(last_error(ctx))
    }
}

/// A realtime VP9 encoder: CBR, no lookahead, a buffer of about two frames, and an IDR on
/// request.
pub struct Encoder {
    ctx: Box<vpx::vpx_codec_ctx_t>,
    width: usize,
    height: usize,
    idr_next: bool,
    /// The picture split into I420, reused from frame to frame.
    planar: Vec<u8>,
}

// SAFETY: the context is only used through `&mut self`, from one thread at a time, and
// libvpx keeps no thread-bound state in it.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn open(config: EncoderConfig) -> Result<Self, EncodeError> {
        let failed = |e: String| EncodeError::Vpx(e);
        let (width, height) = (config.width, config.height);
        let fps = config.fps.max(1);
        let mut cfg = std::mem::MaybeUninit::<vpx::vpx_codec_enc_cfg_t>::uninit();
        // SAFETY: libvpx fills in the whole config, which is read only once it succeeded.
        let mut cfg = unsafe {
            let iface = vpx::vpx_codec_vp9_cx();
            let status = vpx::vpx_codec_enc_config_default(iface, cfg.as_mut_ptr(), 0);
            if status != vpx::vpx_codec_err_t::VPX_CODEC_OK {
                return Err(failed(format!("no default config: {status:?}")));
            }
            cfg.assume_init()
        };
        cfg.g_w = width as c_uint;
        cfg.g_h = height as c_uint;
        cfg.g_timebase = vpx::vpx_rational {
            num: 1,
            den: throcc_proto::frame::VIDEO_CLOCK_HZ as c_int,
        };
        cfg.g_threads =
            std::thread::available_parallelism().map_or(1, |n| n.get().min(8)) as c_uint;
        cfg.g_lag_in_frames = 0;
        cfg.g_error_resilient = 1;
        cfg.rc_end_usage = vpx::vpx_rc_mode::VPX_CBR;
        cfg.rc_target_bitrate = config.kbps;
        // A buffer of about two frames, as every other encoder holds.
        let buffer_ms = (2000 / fps).max(1);
        cfg.rc_buf_sz = buffer_ms;
        cfg.rc_buf_initial_sz = buffer_ms * 5 / 6;
        cfg.rc_buf_optimal_sz = buffer_ms * 5 / 6;
        cfg.rc_dropframe_thresh = 0;
        cfg.kf_mode = vpx::vpx_kf_mode::VPX_KF_AUTO;
        cfg.kf_max_dist = fps * GOP_SECONDS;
        let mut ctx: Box<vpx::vpx_codec_ctx_t> = Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: `ctx` and `cfg` are live and the ABI version is the one the bindings were
        // made for.
        let status = unsafe {
            vpx::vpx_codec_enc_init_ver(
                &mut *ctx,
                vpx::vpx_codec_vp9_cx(),
                &cfg,
                0,
                vpx::VPX_ENCODER_ABI_VERSION as c_int,
            )
        };
        if status != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(failed(format!("opening the encoder: {status:?}")));
        }
        let mut encoder = Self {
            ctx,
            width,
            height,
            idr_next: true,
            planar: Vec::new(),
        };
        use vpx::vp8e_enc_control_id::*;
        for (control, value) in [
            (VP8E_SET_CPUUSED, CPU_USED),
            (VP9E_SET_ROW_MT, 1),
            (VP9E_SET_TUNE_CONTENT, SCREEN_CONTENT),
            (
                VP9E_SET_COLOR_SPACE,
                vpx::vpx_color_space::VPX_CS_BT_709 as c_int,
            ),
            (
                VP9E_SET_COLOR_RANGE,
                vpx::vpx_color_range::VPX_CR_FULL_RANGE as c_int,
            ),
        ] {
            // SAFETY: each control takes one int.
            let status =
                unsafe { vpx::vpx_codec_control_(&mut *encoder.ctx, control as c_int, value) };
            check(&mut encoder.ctx, status).map_err(failed)?;
        }
        Ok(encoder)
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn force_keyframe(&mut self) {
        self.idr_next = true;
    }

    /// Encodes `picture`, which must be of the encoder's size.
    pub fn encode(&mut self, picture: &VideoFrame) -> Result<Vec<EncodedUnit>, EncodeError> {
        if (picture.width, picture.height) != (self.width, self.height) {
            return Err(EncodeError::OtherSize(picture.width, picture.height));
        }
        nv12_to_i420(picture, &mut self.planar);
        // SAFETY: zeroed is how vpx_img_wrap expects the struct, which it fills in.
        let mut image: vpx::vpx_image_t = unsafe { std::mem::zeroed() };
        // SAFETY: `planar` holds a tightly packed I420 picture of the encoder's size, and
        // outlives `image`, which is used only within this call.
        let wrapped = unsafe {
            vpx::vpx_img_wrap(
                &mut image,
                vpx::vpx_img_fmt::VPX_IMG_FMT_I420,
                self.width as c_uint,
                self.height as c_uint,
                1,
                self.planar.as_mut_ptr(),
            )
        };
        if wrapped.is_null() {
            return Err(EncodeError::Vpx("can't wrap the picture".into()));
        }
        image.range = vpx::vpx_color_range::VPX_CR_FULL_RANGE;
        image.cs = vpx::vpx_color_space::VPX_CS_BT_709;
        let flags = if std::mem::take(&mut self.idr_next) {
            vpx::VPX_EFLAG_FORCE_KF as vpx::vpx_enc_frame_flags_t
        } else {
            0
        };
        // SAFETY: `image` wraps live planes for the duration of the call.
        let status = unsafe {
            vpx::vpx_codec_encode(
                &mut *self.ctx,
                &image,
                i64::from(picture.timestamp),
                1,
                flags,
                vpx::VPX_DL_REALTIME.into(),
            )
        };
        check(&mut self.ctx, status).map_err(EncodeError::Vpx)?;
        let mut units = Vec::new();
        let mut iter: vpx::vpx_codec_iter_t = ptr::null();
        loop {
            // SAFETY: the packets libvpx hands out stay valid until the next encode.
            let packet = unsafe { vpx::vpx_codec_get_cx_data(&mut *self.ctx, &mut iter) };
            // SAFETY: a non-null packet is a live `vpx_codec_cx_pkt_t`.
            let Some(packet) = (unsafe { packet.as_ref() }) else {
                break;
            };
            if packet.kind != vpx::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                continue;
            }
            // SAFETY: a frame packet's union holds a frame, whose buffer has `sz` bytes.
            let frame = unsafe { packet.data.frame };
            let data = unsafe { std::slice::from_raw_parts(frame.buf.cast::<u8>(), frame.sz) };
            units.push(EncodedUnit {
                data: Bytes::copy_from_slice(data),
                keyframe: frame.flags & vpx::VPX_FRAME_IS_KEY != 0,
                timestamp: frame.pts as u32,
            });
        }
        Ok(units)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `open` and is destroyed once.
        unsafe { vpx::vpx_codec_destroy(&mut *self.ctx) };
    }
}

/// A VP9 decoder.
pub struct Decoder {
    ctx: Box<vpx::vpx_codec_ctx_t>,
    max_size: MaxSize,
}

// SAFETY: as for the encoder.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(max_size: MaxSize) -> Result<Self, DecodeError> {
        let mut ctx: Box<vpx::vpx_codec_ctx_t> = Box::new(unsafe { std::mem::zeroed() });
        let cfg = vpx::vpx_codec_dec_cfg_t {
            threads: std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as c_uint,
            w: 0,
            h: 0,
        };
        // SAFETY: `ctx` and `cfg` are live and the ABI version is the one the bindings were
        // made for.
        let status = unsafe {
            vpx::vpx_codec_dec_init_ver(
                &mut *ctx,
                vpx::vpx_codec_vp9_dx(),
                &cfg,
                0,
                vpx::VPX_DECODER_ABI_VERSION as c_int,
            )
        };
        if status != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(DecodeError::Vpx(format!("opening the decoder: {status:?}")));
        }
        Ok(Self { ctx, max_size })
    }

    /// Decodes one access unit into its newest picture, if any.
    pub fn decode(
        &mut self,
        unit: &[u8],
        timestamp: u32,
        spare: Option<VideoFrame>,
    ) -> Result<Option<VideoFrame>, DecodeError> {
        let len =
            c_uint::try_from(unit.len()).map_err(|_| DecodeError::Vpx("unit too large".into()))?;
        self.check_size(unit, len)?;
        // SAFETY: `unit` is live for the call.
        let status = unsafe {
            vpx::vpx_codec_decode(&mut *self.ctx, unit.as_ptr(), len, ptr::null_mut(), 0)
        };
        check(&mut self.ctx, status).map_err(DecodeError::Vpx)?;
        let mut iter: vpx::vpx_codec_iter_t = ptr::null();
        let mut newest = None;
        loop {
            // SAFETY: images stay valid until the next decode.
            let image = unsafe { vpx::vpx_codec_get_frame(&mut *self.ctx, &mut iter) };
            // SAFETY: a non-null image is a live `vpx_image_t`.
            let Some(image) = (unsafe { image.as_ref() }) else {
                break;
            };
            newest = Some(image);
        }
        let Some(image) = newest else {
            return Ok(None);
        };
        let (width, height) = (image.d_w as usize, image.d_h as usize);
        if !self.max_size.fits(width as i64, height as i64) {
            return Err(DecodeError::TooLarge(width as i32, height as i32));
        }
        if image.fmt != vpx::vpx_img_fmt::VPX_IMG_FMT_I420 {
            return Err(DecodeError::Format(image.fmt as i32));
        }
        let plane = |index: usize| Plane {
            data: image.planes[index],
            stride: image.stride[index] as usize,
        };
        // SAFETY: an I420 image holds its three planes at their strides, at its display size.
        let frame = unsafe {
            i420_to_nv12(
                [plane(0), plane(1), plane(2)],
                (width, height),
                timestamp,
                spare,
            )
        };
        Ok(Some(frame))
    }
}

impl Decoder {
    /// Refuses a keyframe larger than allowed before libvpx allocates for it. libvpx is built
    /// to refuse anything past [`crate::MAX_SIDE`] itself, which covers the size changes
    /// predicted frames can make.
    fn check_size(&self, unit: &[u8], len: c_uint) -> Result<(), DecodeError> {
        let mut info = vpx::vpx_codec_stream_info_t {
            sz: std::mem::size_of::<vpx::vpx_codec_stream_info_t>() as c_uint,
            w: 0,
            h: 0,
            is_kf: 0,
        };
        // SAFETY: `unit` and `info` are live for the call, and `info.sz` says its size.
        let status = unsafe {
            vpx::vpx_codec_peek_stream_info(vpx::vpx_codec_vp9_dx(), unit.as_ptr(), len, &mut info)
        };
        let (width, height) = (i64::from(info.w), i64::from(info.h));
        if status == vpx::vpx_codec_err_t::VPX_CODEC_OK && !self.max_size.fits(width, height) {
            return Err(DecodeError::TooLarge(width as i32, height as i32));
        }
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new` and is destroyed once.
        unsafe { vpx::vpx_codec_destroy(&mut *self.ctx) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::luma_psnr;

    #[test]
    fn vp9_round_trips_with_keyframes_on_request() {
        let config = EncoderConfig {
            width: 640,
            height: 360,
            fps: 30,
            kbps: 2000,
        };
        let mut encoder = Encoder::open(config).unwrap();
        let mut decoder = Decoder::new(MaxSize::ANY).unwrap();
        let (mut shown, mut keyframes) = (Vec::new(), Vec::new());
        for n in 0..40u16 {
            if n == 20 {
                encoder.force_keyframe();
            }
            let input = VideoFrame::test_pattern(640, 360, n, u32::from(n) * 3000);
            for unit in encoder.encode(&input).unwrap() {
                if unit.keyframe {
                    keyframes.push(unit.timestamp / 3000);
                }
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp, None).unwrap() {
                    let number = output.read_test_number();
                    let reference = VideoFrame::test_pattern(640, 360, number, 0);
                    shown.push((number, luma_psnr(&output, &reference)));
                }
            }
        }
        assert_eq!(shown.len(), 40);
        assert!(shown.windows(2).all(|w| w[1].0 > w[0].0), "{shown:?}");
        let worst = shown.iter().map(|(_, p)| *p).fold(f64::INFINITY, f64::min);
        assert!(worst > 28.0, "picture quality {worst:.1} dB");
        assert_eq!(keyframes, [0, 20]);
    }

    #[test]
    fn a_picture_larger_than_allowed_is_refused() {
        let config = EncoderConfig {
            width: 320,
            height: 180,
            fps: 30,
            kbps: 500,
        };
        let mut encoder = Encoder::open(config).unwrap();
        let mut decoder = Decoder::new(MaxSize::new(160, 180)).unwrap();
        let input = VideoFrame::test_pattern(320, 180, 0, 0);
        let unit = encoder.encode(&input).unwrap().remove(0);
        assert!(matches!(
            decoder.decode(&unit.data, 0, None),
            Err(DecodeError::TooLarge(320, 180))
        ));
    }
}
