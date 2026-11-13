//! These run on CI's macOS runners, virtual machines without a hardware encoder, so they
//! open VideoToolbox's software encoders. A codec that doesn't open fails the test rather
//! than skipping it.

use throcc_proto::messages::Codec;

use super::{Decoder, Encoder, PixelBuffer};
use crate::frame::{VideoFrame, luma_psnr};
use crate::{DecodeError, EncoderConfig, Frame, MaxSize, Output};

fn config(width: usize, height: usize) -> EncoderConfig {
    EncoderConfig {
        width,
        height,
        fps: 30,
        kbps: 4000,
    }
}

fn software(codec: Codec, width: usize, height: usize) -> Encoder {
    Encoder::open_with(codec, config(width, height), false)
        .unwrap_or_else(|e| panic!("VideoToolbox doesn't open {codec:?} here: {e}"))
}

/// Encodes 60 test patterns from pixel buffers, asking for an IDR at 30, and decodes them.
fn round_trip(codec: Codec) {
    let mut encoder = software(codec, 1280, 720);
    assert_eq!(encoder.codec(), codec);
    assert!(encoder.takes_pixel_buffers());
    let mut decoder = Decoder::new(codec, MaxSize::ANY, Output::default()).unwrap();
    let (mut shown, mut keyframes) = (Vec::new(), Vec::new());
    let mut in_gpu_memory = true;
    for n in 0..60u16 {
        if n == 30 {
            encoder.recover(throcc_proto::messages::Recovery::Idr);
        }
        let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
        let picture = PixelBuffer::holding(&input).unwrap();
        for unit in encoder.encode_pixel_buffer(&picture).unwrap() {
            assert_eq!(unit.keyframe, crate::nal::is_keyframe(codec, &unit.data));
            if unit.keyframe {
                keyframes.push(unit.timestamp / 3000);
            }
            if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                in_gpu_memory &= matches!(output, Frame::PixelBuffer(_));
                assert_eq!(output.timestamp(), unit.timestamp);
                let output = output.copied_to_memory();
                let number = output.read_test_number();
                let reference = VideoFrame::test_pattern(1280, 720, number, 0);
                shown.push((number, luma_psnr(&output, &reference)));
            }
        }
    }
    eprintln!(
        "{codec:?}: {} frames shown, keyframes at {keyframes:?}",
        shown.len()
    );
    assert!(
        in_gpu_memory,
        "pictures were copied out of their pixel buffers"
    );
    assert!(
        shown.len() >= 58,
        "only {} of 60 frames came out",
        shown.len()
    );
    assert!(
        shown.windows(2).all(|w| w[1].0 > w[0].0),
        "frames out of order"
    );
    let worst = shown.iter().map(|(_, p)| *p).fold(f64::INFINITY, f64::min);
    assert!(worst > 28.0, "picture quality {worst:.1} dB");
    assert!(
        keyframes.contains(&0) && keyframes.contains(&30),
        "keyframes at {keyframes:?}"
    );
}

#[test]
fn hevc_round_trips_through_pixel_buffers() {
    round_trip(Codec::Hevc);
}

#[test]
fn h264_round_trips_through_pixel_buffers() {
    round_trip(Codec::H264);
}

#[test]
fn the_first_encoder_tried_opens_or_says_why() {
    match Encoder::open(config(1920, 1080)) {
        Ok(encoder) => eprintln!("opened {} ({})", encoder.name(), encoder.engine()),
        Err(e) => eprintln!("no hardware encoder here: {e}"),
    }
}

#[test]
fn pictures_of_other_sizes_are_fitted_with_black_bars() {
    let mut encoder = software(Codec::H264, 1280, 720);
    let mut decoder = Decoder::new(Codec::H264, MaxSize::ANY, Output::default()).unwrap();
    let mut last = None;
    for (n, (width, height)) in [(2560, 1440), (1000, 1000), (641, 359), (1000, 1000)]
        .into_iter()
        .enumerate()
    {
        let mut input = VideoFrame::blank(width & !1, height & !1);
        input.y.fill(200);
        input.timestamp = n as u32 * 3000;
        let units = if n % 2 == 0 {
            encoder.encode(&input).unwrap()
        } else {
            let picture = PixelBuffer::holding(&input).unwrap();
            encoder.encode_pixel_buffer(&picture).unwrap()
        };
        for unit in units {
            if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                last = Some(output.copied_to_memory());
            }
        }
    }
    let output = last.expect("a decoded picture");
    assert_eq!((output.width, output.height), (1280, 720));
    let luma = |x: usize, y: usize| output.y[y * output.width + x];
    assert!(luma(20, 360) < 30, "left bar is {}", luma(20, 360));
    assert!(luma(1260, 360) < 30, "right bar is {}", luma(1260, 360));
    assert!(
        luma(640, 360).abs_diff(200) < 20,
        "picture is {}",
        luma(640, 360)
    );
}

#[test]
fn a_picture_larger_than_allowed_is_refused() {
    let mut encoder = software(Codec::H264, 640, 360);
    let mut decoder = Decoder::new(Codec::H264, MaxSize::new(320, 360), Output::default()).unwrap();
    let input = VideoFrame::test_pattern(640, 360, 0, 0);
    let unit = encoder.encode(&input).unwrap().remove(0);
    assert!(matches!(
        decoder.decode(&unit.data, 0),
        Err(DecodeError::TooLarge(640, 360))
    ));
}

#[test]
fn a_decoder_waits_for_a_keyframe() {
    let mut encoder = software(Codec::Hevc, 640, 360);
    let mut decoder = Decoder::new(Codec::Hevc, MaxSize::ANY, Output::default()).unwrap();
    let mut units = Vec::new();
    for n in 0..3u16 {
        let input = VideoFrame::test_pattern(640, 360, n, u32::from(n) * 3000);
        units.extend(encoder.encode(&input).unwrap());
    }
    let predicted = units
        .iter()
        .rfind(|u| !u.keyframe)
        .expect("a predicted picture");
    assert!(matches!(
        decoder.decode(&predicted.data, predicted.timestamp),
        Err(DecodeError::NoKeyframe)
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn software_decoders_hand_out_pixel_buffers() {
    let mut encoder = Encoder::open_with(Codec::Vp9, config(640, 360), false).unwrap();
    assert!(!encoder.takes_pixel_buffers());
    let mut decoder = Decoder::new(Codec::Vp9, MaxSize::ANY, Output::default()).unwrap();
    assert_eq!(decoder.engine(), "libvpx (software)");
    let mut shown = 0;
    for n in 0..5u16 {
        let input = VideoFrame::test_pattern(640, 360, n, u32::from(n) * 3000);
        for unit in encoder.encode(&input).unwrap() {
            let output = decoder.decode(&unit.data, unit.timestamp).unwrap().unwrap();
            assert!(matches!(output, Frame::PixelBuffer(_)));
            assert_eq!(output.copied_to_memory().read_test_number(), n);
            shown += 1;
        }
    }
    assert_eq!(shown, 5);
}

#[test]
fn av1_decodes_in_dav1d_into_pixel_buffers() {
    let file = include_bytes!("../../testdata/pattern.ivf");
    let mut decoder = Decoder::new(Codec::Av1, MaxSize::ANY, Output::default()).unwrap();
    for (n, unit) in crate::software::dav1d::ivf_units(file)
        .into_iter()
        .enumerate()
    {
        let output = decoder.decode(unit, n as u32 * 3000).unwrap().unwrap();
        assert!(matches!(output, Frame::PixelBuffer(_)));
        assert_eq!(output.copied_to_memory().read_test_number(), n as u16);
    }
}
