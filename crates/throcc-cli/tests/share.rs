#![cfg(feature = "ffmpeg")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::media::capture::{self, Capture, CaptureConfig, TestPattern};
use throcc_client_core::media::engine::Kind;
use throcc_client_core::media::share::{self, ShareSender};
use throcc_client_core::media::share_audio::audio_to_video;
use throcc_client_core::media::video::frame::VideoFrame;
use throcc_proto::frame::VIDEO_CLOCK_HZ;
use throcc_proto::messages::{Codec, Event, Request, Response, ShareConfig, ShareTracks};
use tokio::sync::mpsc;

use common::*;

const WANTED: ShareConfig = share_1080p60(Codec::H264, 8000);
const WATCH_UNITS: usize = 256;

/// Starts Alice sharing a test pattern stamped with milliseconds since `epoch`, and
/// forwards the server's keyframe requests to her encoder. `None`, reported as a skip,
/// without an encoder.
async fn share_pattern(
    alice: &mut Call,
    bob: &Call,
    wanted: ShareConfig,
    epoch: Instant,
) -> Option<(Arc<ShareSender>, ShareTracks)> {
    let capture = Box::new(TestPattern::new(CaptureConfig::for_share(&wanted), epoch));
    share_capture(alice, bob, wanted, capture).await
}

/// Starts Alice sharing what `capture` delivers, and forwards the server's keyframe
/// requests to her encoder. `None`, reported as a skip, without an encoder.
async fn share_capture(
    alice: &mut Call,
    bob: &Call,
    wanted: ShareConfig,
    capture: Box<dyn Capture>,
) -> Option<(Arc<ShareSender>, ShareTracks)> {
    let tracks = first_share(bob, alice).await;
    let sender = match ShareSender::start(alice.engine.clone(), tracks, wanted, capture) {
        Ok(sender) => Arc::new(sender),
        Err(e) => {
            skip(format_args!("no encoder: {e}"));
            return None;
        }
    };
    eprintln!("encoder {} for {:?}", sender.encoder, sender.config.codec);
    assert_eq!(
        alice
            .request(Request::SetMedia(sharing(sender.config)))
            .await
            .unwrap(),
        Response::Ok
    );

    let (_, closed) = mpsc::channel(1);
    let mut events = std::mem::replace(&mut alice.events, closed);
    let forward = sender.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let CallEvent::Server(Event::KeyframeRequested { kind, .. }) = event {
                forward.recover(kind);
            }
        }
    });
    Some((sender, tracks))
}

/// A screen that shows `moving` frames of the test pattern, then stands still, so its
/// capture delivers nothing more.
struct StillAfter {
    pattern: TestPattern,
    moving: usize,
}

impl Capture for StillAfter {
    fn next_frame(&mut self, into: &mut VideoFrame) -> bool {
        if self.moving == 0 || !self.pattern.next_frame(into) {
            return false;
        }
        self.moving -= 1;
        true
    }
}

fn millis_since(epoch: Instant) -> u16 {
    epoch.elapsed().as_millis() as u16
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a hardware video encoder; virtual machines queue frames for seconds"]
async fn a_shared_picture_arrives_within_a_few_frames() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    let epoch = Instant::now();
    let Some((sender, tracks)) = share_pattern(&mut alice, &bob, WANTED, epoch).await else {
        return;
    };
    let units = watch(&bob, tracks.video, WATCH_UNITS).await;
    let (mut picture, _) = share::receive(units, sender.config, None, || {}).unwrap();

    let mut latencies = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while latencies.len() < 180 {
        tokio::time::timeout_at(deadline, picture.changed())
            .await
            .expect("pictures stopped arriving")
            .unwrap();
        let frame = picture
            .borrow_and_update()
            .as_ref()
            .unwrap()
            .copied_to_memory();
        assert_eq!((frame.width, frame.height), (1920, 1080));
        latencies.push(millis_since(epoch).wrapping_sub(frame.read_test_number()));
    }
    latencies.drain(..60);
    latencies.sort_unstable();
    let (median, p95) = (
        latencies[latencies.len() / 2],
        latencies[latencies.len() * 95 / 100],
    );
    eprintln!("encode to decoded picture: median {median} ms, p95 {p95} ms");
    assert!(p95 < 100, "p95 {p95} ms");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a hardware video encoder; virtual machines queue frames for seconds"]
async fn share_audio_and_video_arrive_on_one_clock() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    let epoch = Instant::now();
    let wanted = ShareConfig {
        audio: true,
        ..WANTED
    };
    let Some((sender, tracks)) = share_pattern(&mut alice, &bob, wanted, epoch).await else {
        return;
    };
    assert!(sender.config.audio, "the test pattern captures audio");
    let mut audio = bob.engine.subscribe(tracks.audio, Kind::Audio, 512);
    let mut video = bob.engine.subscribe(tracks.video, Kind::Video, 512);
    let watching = Request::SetWatched(vec![tracks.video, tracks.audio]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);

    let now_on_video_clock =
        || (epoch.elapsed().as_micros() * u128::from(VIDEO_CLOCK_HZ) / 1_000_000) as u32;
    let (mut audio_lags, mut video_lags) = (Vec::new(), Vec::new());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            Some(unit) = audio.recv() => {
                let captured = audio_to_video(unit.timestamp) + 1800;
                audio_lags.push(now_on_video_clock().wrapping_sub(captured) as i32 / 90);
            }
            Some(unit) = video.recv() => {
                video_lags.push(now_on_video_clock().wrapping_sub(unit.timestamp) as i32 / 90);
            }
            _ = tokio::time::sleep_until(deadline) => {}
        }
    }
    let median = |lags: &mut Vec<i32>| {
        lags.sort_unstable();
        lags[lags.len() / 2]
    };
    assert!(audio_lags.len() > 100 && video_lags.len() > 100);
    let (audio_ms, video_ms) = (median(&mut audio_lags), median(&mut video_lags));
    eprintln!("capture to arrival: audio {audio_ms} ms, video {video_ms} ms");
    assert!((0..100).contains(&audio_ms), "audio lag {audio_ms} ms");
    assert!((0..100).contains(&video_ms), "video lag {video_ms} ms");
    assert!(
        (audio_ms - video_ms).abs() < 40,
        "audio {audio_ms} ms, video {video_ms} ms"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_who_starts_watching_late_sees_a_still_screen() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    let wanted = ShareConfig {
        width: 1280,
        height: 720,
        ..WANTED
    };
    let pattern = TestPattern::new(CaptureConfig::for_share(&wanted), Instant::now());
    let capture = Box::new(StillAfter { pattern, moving: 5 });
    let Some((sender, tracks)) = share_capture(&mut alice, &bob, wanted, capture).await else {
        return;
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let units = watch(&bob, tracks.video, WATCH_UNITS).await;
    let (mut picture, _) = share::receive(units, sender.config, None, || {}).unwrap();
    tokio::time::timeout(Duration::from_secs(5), picture.changed())
        .await
        .expect("no picture of the still screen")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_still_screen_shows_after_its_keyframe_lost_a_packet() {
    let server = start_server();
    let relay = lossy_relay(&server.addr, Toward::Client, 4).await;
    let (mut alice, bob, _) = room_with_two_at(&server, &relay.addr).await;
    let wanted = ShareConfig {
        width: 1280,
        height: 720,
        ..WANTED
    };
    let pattern = TestPattern::new(CaptureConfig::for_share(&wanted), Instant::now());
    let capture = Box::new(StillAfter { pattern, moving: 5 });
    let Some((sender, tracks)) = share_capture(&mut alice, &bob, wanted, capture).await else {
        return;
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    relay.lossy.store(true, Ordering::Relaxed);
    let units = watch(&bob, tracks.video, WATCH_UNITS).await;
    let asks = Arc::new(AtomicUsize::new(0));
    let ask_idr = {
        let (asks, request) = (asks.clone(), bob.engine.idr_requester(tracks.video));
        move || {
            asks.fetch_add(1, Ordering::Relaxed);
            request();
        }
    };
    let (mut picture, _) = share::receive(units, sender.config, None, ask_idr).unwrap();
    wait_for("the keyframe on its way", || {
        bob.engine.arrived(tracks.video).datagrams > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    relay.lossy.store(false, Ordering::Relaxed);
    assert!(relay.dropped.load(Ordering::Relaxed) > 0);
    tokio::time::timeout(Duration::from_secs(10), picture.changed())
        .await
        .expect("no picture of the still screen")
        .unwrap();
    assert!(
        asks.load(Ordering::Relaxed) > 0,
        "the viewer never asked again"
    );
}

#[tokio::test]
async fn a_sixteen_by_ten_screen_is_sent_at_its_own_shape() {
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;
    let tracks = first_share(&bob, &alice).await;
    let screen = CaptureConfig {
        width: 2880,
        height: 1800,
        fps: 30,
        audio: false,
    };
    let capture = Box::new(TestPattern::new(screen, Instant::now()));
    let sender = match ShareSender::start(alice.engine.clone(), tracks, WANTED, capture) {
        Ok(sender) => sender,
        // Every macOS has VideoToolbox, and the encoder ladder ends in software libvpx,
        // which Homebrew's ffmpeg carries, so CI's macOS run always gets an encoder.
        Err(e) if cfg!(target_os = "macos") => panic!("no encoder started: {e}"),
        Err(e) => {
            skip(format_args!("no encoder: {e}"));
            return;
        }
    };
    assert_eq!((sender.config.width, sender.config.height), (1728, 1080));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a desktop session with xdg-desktop-portal and someone to pick a screen"]
async fn a_screen_share_runs_for_profiling() {
    let seconds = std::env::var("THROCC_PROFILE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;
    let tracks = first_share(&bob, &alice).await;
    let wanted = throcc_client_core::state::ShareChoice::default().config(Codec::Av1, false);
    let config = CaptureConfig::for_share(&wanted);
    let capture = tokio::task::spawn_blocking(move || capture::start(config))
        .await
        .unwrap()
        .unwrap();
    let sender = ShareSender::start(alice.engine.clone(), tracks, wanted, capture).unwrap();
    eprintln!(
        "pid {} sharing {}×{} with {}",
        std::process::id(),
        sender.config.width,
        sender.config.height,
        sender.encoder
    );
    if std::env::var_os("THROCC_PREVIEW").is_some() {
        let preview = sender.preview.clone();
        tokio::spawn(async move {
            loop {
                preview.drawn();
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        });
    }
    let mut units = watch(&bob, tracks.video, WATCH_UNITS).await;
    let (mut count, mut bytes) = (0usize, 0usize);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while let Ok(Some(unit)) = tokio::time::timeout_at(deadline, units.recv()).await {
        count += 1;
        bytes += unit.data.len();
    }
    eprintln!(
        "{count} units, {:.1} Mbps",
        bytes as f64 * 8.0 / seconds as f64 / 1e6
    );
    assert!(!sender.ended());
}
