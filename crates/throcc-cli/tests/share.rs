#![cfg(feature = "ffmpeg")]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::media::capture::{CaptureConfig, TestPattern};
use throcc_client_core::media::engine::Kind;
use throcc_client_core::media::reorder::REORDER_WINDOW;
use throcc_client_core::media::share::{self, ShareSender};
use throcc_client_core::media::share_audio::audio_to_video;
use throcc_client_core::media::video::decode::{Decoder, MAX_PIXELS};
use throcc_proto::frame::Flags;
use throcc_proto::ids::MediaId;
use throcc_proto::messages::{Codec, Event, MediaState, Request, Response, Share, ShareConfig};
use tokio::sync::{Mutex, mpsc};

use common::*;

const WANTED: ShareConfig = ShareConfig {
    audio: false,
    kbps: 8000,
    codec: Codec::H264,
    width: 1920,
    height: 1080,
    fps: 60,
};

/// Starts Alice sharing a test pattern stamped with milliseconds since `epoch`, and
/// forwards the server's keyframe requests to her encoder. `None` without an encoder.
async fn share_pattern(
    alice: &mut Call,
    bob: &Call,
    wanted: ShareConfig,
    epoch: Instant,
) -> Option<(Arc<ShareSender>, Share, Arc<Mutex<Vec<Instant>>>)> {
    let tracks = first_share(bob, alice).await;
    let capture = Box::new(TestPattern::new(CaptureConfig::for_share(&wanted), epoch));
    let sender = match ShareSender::start(alice.engine.clone(), tracks, wanted, capture) {
        Ok(sender) => Arc::new(sender),
        Err(e) => {
            eprintln!("skipped: {e}");
            return None;
        }
    };
    eprintln!("encoder {} for {:?}", sender.encoder, sender.config.codec);
    let media = MediaState {
        mic: false,
        deafened: false,
        share: Some(sender.config),
    };
    assert_eq!(
        alice.request(Request::SetMedia(media)).await.unwrap(),
        Response::Ok
    );

    let requests = Arc::new(Mutex::new(Vec::new()));
    let (_, closed) = mpsc::channel(1);
    let mut events = std::mem::replace(&mut alice.events, closed);
    let (forward, log) = (sender.clone(), requests.clone());
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let CallEvent::Server(Event::KeyframeWanted { kind, .. }) = event {
                log.lock().await.push(Instant::now());
                forward.recover(kind);
            }
        }
    });
    Some((sender, tracks, requests))
}

async fn watch(
    bob: &Call,
    video: MediaId,
) -> mpsc::Receiver<throcc_client_core::media::engine::Received> {
    let units = bob.engine.subscribe(video, Kind::Video, 256);
    assert_eq!(
        bob.request(Request::SetWatching(vec![video]))
            .await
            .unwrap(),
        Response::Ok
    );
    units
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
    let Some((sender, tracks, _)) = share_pattern(&mut alice, &bob, WANTED, epoch).await else {
        return;
    };
    let units = watch(&bob, tracks.video).await;
    let mut picture = share::receive(units, sender.config, None, || {}).unwrap();

    let mut latencies = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while latencies.len() < 180 {
        tokio::time::timeout_at(deadline, picture.changed())
            .await
            .expect("pictures stopped arriving")
            .unwrap();
        let frame = picture.borrow_and_update().clone().unwrap();
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
async fn two_percent_loss_brings_a_refresh_within_a_second() {
    let server = start_server();
    let relay = lossy_relay(&server.addr, 50).await;
    let (mut alice, bob, _) = room_with_two_at(&server, &relay.addr).await;
    let epoch = Instant::now();
    let Some((sender, tracks, wanted)) = share_pattern(&mut alice, &bob, WANTED, epoch).await
    else {
        return;
    };
    let mut units = watch(&bob, tracks.video).await;
    let mut decoder = Decoder::new(sender.config.codec, MAX_PIXELS).unwrap();

    let mut keyframes = Vec::new();
    let mut pictures = Vec::new();
    let lossy_from = Instant::now() + Duration::from_secs(1);
    let lossy_until = lossy_from + Duration::from_secs(6);
    while Instant::now() < lossy_until + Duration::from_secs(1) {
        relay.lossy.store(
            Instant::now() >= lossy_from && Instant::now() < lossy_until,
            Ordering::Relaxed,
        );
        let unit = tokio::time::timeout(Duration::from_secs(1), units.recv())
            .await
            .expect("units stopped arriving")
            .unwrap();
        let now = Instant::now();
        if unit.flags.contains(Flags::KEYFRAME) {
            keyframes.push(now);
        }
        if let Ok(Some(_)) = decoder.decode(&unit.data, unit.timestamp) {
            pictures.push(now);
        }
    }
    let wanted = wanted.lock().await.clone();
    let lost = relay.dropped.load(Ordering::Relaxed);
    let abandoned = bob.engine.stats.abandoned_units.load(Ordering::Relaxed);
    let duplicates = bob.engine.stats.duplicates.load(Ordering::Relaxed);
    eprintln!(
        "dropped {lost} packets, {abandoned} units abandoned, {} keyframe requests, {} keyframes, {duplicates} duplicates",
        wanted.len(),
        keyframes.len()
    );
    assert!(lost > 0 && abandoned > 0, "the relay caused no loss");
    let late: Vec<_> = wanted
        .iter()
        .filter(|w| **w < lossy_until)
        .filter(|w| {
            !keyframes
                .iter()
                .any(|k| *k > **w && *k - **w <= Duration::from_secs(1))
        })
        .collect();
    assert!(
        late.is_empty(),
        "{} requests waited over a second for a keyframe",
        late.len()
    );
    let longest_freeze = pictures.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    let aggregation = Duration::from_secs(1);
    let allowed = aggregation + REORDER_WINDOW + Duration::from_millis(200);
    assert!(longest_freeze < allowed, "frozen for {longest_freeze:?}");
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
    let Some((sender, tracks, _)) = share_pattern(&mut alice, &bob, wanted, epoch).await else {
        return;
    };
    assert!(sender.config.audio, "the test pattern captures audio");
    let mut audio = bob.engine.subscribe(tracks.audio, Kind::Audio, 512);
    let mut video = bob.engine.subscribe(tracks.video, Kind::Video, 512);
    let watching = Request::SetWatching(vec![tracks.video, tracks.audio]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);

    let now_on_video_clock = || (epoch.elapsed().as_micros() * 90_000 / 1_000_000) as u32;
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
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    assert_eq!((sender.config.width, sender.config.height), (1728, 1080));
}
