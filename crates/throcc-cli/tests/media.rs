mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_client_core::call::CallEvent;
use throcc_client_core::media::engine::{Kind, Outgoing};
use throcc_proto::messages::{Codec, Event, MediaState, Request, Response, ShareConfig};

use common::*;

fn synthetic(index: u32, len: usize) -> Bytes {
    let mut data = vec![0u8; len];
    data[..4].copy_from_slice(&index.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(4) {
        *b = (index as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    Bytes::from(data)
}

#[tokio::test]
#[cfg_attr(
    debug_assertions,
    ignore = "a debug build can't encrypt 20 Mbps on CI runners"
)]
async fn ten_thousand_units_at_20_mbps_arrive_intact() {
    const UNITS: u32 = 10_000;
    const LEN: usize = 2500;
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;

    let config = ShareConfig {
        audio: false,
        kbps: 20_000,
        codec: Codec::Av1,
        width: 1920,
        height: 1080,
        fps: 60,
    };
    let share = MediaState {
        mic: false,
        deafened: false,
        share: Some(config),
    };
    assert_eq!(
        alice.request(Request::SetMedia(share)).await.unwrap(),
        Response::Ok
    );
    let alice_video = {
        let keys = alice.engine.keys();
        let epoch = keys.keys.epoch();
        let tracks = bob
            .engine
            .keys()
            .keys
            .peer_tracks(alice.me())
            .cloned()
            .unwrap();
        (tracks.shares[0].video, epoch)
    };
    let (video_id, epoch) = alice_video;

    let mut units = bob.engine.subscribe(video_id, Kind::Video, UNITS as usize);
    assert_eq!(
        bob.request(Request::SetWatching(vec![video_id]))
            .await
            .unwrap(),
        Response::Ok
    );
    wait_for("bob's key for alice's video", || {
        bob.engine.keys().keys.recv_key(video_id, epoch).is_some()
    })
    .await;
    wait_for("both at one epoch", || {
        alice.engine.keys().keys.epoch() == bob.engine.keys().keys.epoch()
    })
    .await;

    let started = Instant::now();
    let sender = alice.engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        for batch in 0..UNITS / 10 {
            tick.tick().await;
            for i in 0..10 {
                let index = batch * 10 + i;
                let unit = Outgoing {
                    media_id: video_id,
                    data: synthetic(index, LEN),
                    timestamp: index * 90,
                    keyframe: index == 0,
                    discardable: false,
                    enqueued: Instant::now(),
                };
                sender.send(Kind::Video, unit);
            }
        }
    });

    let mut intact = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while intact < UNITS {
        let unit = tokio::time::timeout_at(deadline, units.recv())
            .await
            .expect("units stopped arriving")
            .unwrap();
        let index = u32::from_be_bytes(unit.data[..4].try_into().unwrap());
        assert_eq!(unit.data, synthetic(index, LEN), "unit {index} corrupted");
        assert_eq!(
            index,
            intact,
            "a unit went missing; sender dropped {} and queue-full {}, receiver overflowed {}",
            alice
                .engine
                .stats
                .send_dropped_units
                .load(Ordering::Relaxed),
            alice.engine.stats.send_queue_full.load(Ordering::Relaxed),
            bob.engine.stats.track_overflow.load(Ordering::Relaxed),
        );
        intact += 1;
    }
    let elapsed = started.elapsed();
    let mbps = f64::from(UNITS) * LEN as f64 * 8.0 / elapsed.as_secs_f64() / 1e6;
    assert!(mbps >= 19.0, "only {mbps:.1} Mbps");
    let stats = &bob.engine.stats;
    assert_eq!(stats.decrypt_failures.load(Ordering::Relaxed), 0);
    assert_eq!(
        alice
            .engine
            .stats
            .send_dropped_units
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        alice.engine.stats.send_queue_full.load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn watching_a_share_asks_its_sender_for_an_idr() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    let video_id = bob
        .engine
        .keys()
        .keys
        .peer_tracks(alice.me())
        .unwrap()
        .shares[0]
        .video;
    assert_eq!(
        bob.request(Request::SetWatching(vec![video_id]))
            .await
            .unwrap(),
        Response::Ok
    );
    let wanted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(CallEvent::Server(Event::KeyframeWanted { media_id, kind })) =
                alice.events.recv().await
            {
                return (media_id, kind);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(wanted, (video_id, throcc_proto::messages::Recovery::Idr));
}

#[tokio::test]
async fn mic_audio_reaches_peers_but_not_the_deafened() {
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;
    let mic = bob.engine.keys().keys.peer_tracks(alice.me()).unwrap().mic;
    let epoch = alice.engine.keys().keys.epoch();
    let mut heard = bob.engine.subscribe(mic, Kind::Audio, 64);
    wait_for("bob's key for alice's mic", || {
        bob.engine.keys().keys.recv_key(mic, epoch).is_some()
    })
    .await;

    let frame = |i: u32| Outgoing {
        media_id: mic,
        data: Bytes::from(vec![i as u8; 80]),
        timestamp: i * 960,
        keyframe: false,
        discardable: false,
        enqueued: Instant::now(),
    };
    alice.engine.send(Kind::Audio, frame(1));
    let got = tokio::time::timeout(Duration::from_secs(5), heard.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((got.timestamp, got.data), (960, vec![1u8; 80]));

    let deaf = MediaState {
        mic: false,
        deafened: true,
        share: None,
    };
    assert_eq!(
        bob.request(Request::SetMedia(deaf)).await.unwrap(),
        Response::Ok
    );
    alice.engine.send(Kind::Audio, frame(2));
    assert!(
        tokio::time::timeout(Duration::from_millis(500), heard.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn keyframes_arrive_whole_through_two_percent_loss() {
    const KEYFRAMES: u32 = 50;
    const LEN: usize = 60 * 1100;
    let server = start_server();
    let relay = lossy_relay(&server.addr, 50).await;
    let (alice, bob, _) = room_with_two_at(&server, &relay.addr).await;
    let config = ShareConfig {
        audio: false,
        kbps: 10_000,
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        fps: 60,
    };
    let share = MediaState {
        mic: false,
        deafened: false,
        share: Some(config),
    };
    assert_eq!(
        alice.request(Request::SetMedia(share)).await.unwrap(),
        Response::Ok
    );
    wait_for("bob to learn alice's tracks", || {
        bob.engine.keys().keys.peer_tracks(alice.me()).is_some()
    })
    .await;
    let video_id = bob
        .engine
        .keys()
        .keys
        .peer_tracks(alice.me())
        .unwrap()
        .shares[0]
        .video;
    let mut units = bob
        .engine
        .subscribe(video_id, Kind::Video, KEYFRAMES as usize);
    assert_eq!(
        bob.request(Request::SetWatching(vec![video_id]))
            .await
            .unwrap(),
        Response::Ok
    );
    wait_for("both at one epoch with keys", || {
        let epoch = alice.engine.keys().keys.epoch();
        epoch == bob.engine.keys().keys.epoch()
            && bob.engine.keys().keys.recv_key(video_id, epoch).is_some()
    })
    .await;

    relay.lossy.store(true, Ordering::Relaxed);
    for index in 0..KEYFRAMES {
        alice.engine.send(
            Kind::Video,
            Outgoing {
                media_id: video_id,
                data: synthetic(index, LEN),
                timestamp: index * 3000,
                keyframe: true,
                discardable: false,
                enqueued: Instant::now(),
            },
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut whole = std::collections::HashSet::new();
    while let Ok(Some(unit)) = tokio::time::timeout(Duration::from_secs(1), units.recv()).await {
        let index = u32::from_be_bytes(unit.data[..4].try_into().unwrap());
        assert_eq!(unit.data, synthetic(index, LEN));
        whole.insert(index);
    }
    let whole = whole.len() as u32;
    let dropped = relay.dropped.load(Ordering::Relaxed);
    let sent = alice.engine.stats.sent_datagrams.load(Ordering::Relaxed);
    eprintln!(
        "{whole} of {KEYFRAMES} keyframes whole, {dropped} packets dropped, {sent} datagrams sent"
    );
    assert!(dropped >= 30, "the relay dropped only {dropped} packets");
    assert!(
        whole >= KEYFRAMES * 9 / 10,
        "only {whole} of {KEYFRAMES} keyframes whole"
    );
}
