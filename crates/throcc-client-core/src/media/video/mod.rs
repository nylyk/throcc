#[cfg(feature = "ffmpeg")]
pub mod decode;
#[cfg(feature = "ffmpeg")]
pub mod encode;
pub mod frame;

use std::sync::Arc;

/// The newest picture of a share, shared with the renderer.
pub type Picture = tokio::sync::watch::Receiver<Option<Arc<frame::VideoFrame>>>;

#[cfg(all(test, feature = "ffmpeg"))]
mod tests {
    use throcc_proto::messages::Recovery;

    use super::decode::Decoder;
    use super::encode::{Encoder, EncoderConfig};
    use super::frame::{VideoFrame, luma_psnr};

    #[test]
    fn frames_survive_encode_and_decode() {
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            kbps: 4000,
        };
        let Ok(mut encoder) = Encoder::open(config) else {
            eprintln!("no hardware video encoder here; skipping");
            return;
        };
        let mut decoder = Decoder::new(encoder.codec()).unwrap();
        let mut shown = Vec::new();
        let mut keyframes = Vec::new();
        for n in 0..60u16 {
            if n == 30 {
                encoder.recover(Recovery::Idr);
            }
            let input = VideoFrame::test_pattern(1280, 720, n, u32::from(n) * 3000);
            for unit in encoder.encode(&input).unwrap() {
                if unit.keyframe {
                    keyframes.push(unit.timestamp / 3000);
                }
                if let Some(output) = decoder.decode(&unit.data, unit.timestamp).unwrap() {
                    let number = output.read_test_number();
                    let psnr = luma_psnr(&output, &VideoFrame::test_pattern(1280, 720, number, 0));
                    shown.push((number, psnr));
                }
            }
        }
        println!(
            "{}: {} frames shown, keyframes at {keyframes:?}",
            encoder.name(),
            shown.len()
        );
        assert!(
            shown.len() >= 55,
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
}
