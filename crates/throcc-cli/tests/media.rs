mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use throcc_client_core::call::Call;
use throcc_client_core::media::engine::{Kind, Outgoing};
use throcc_proto::frame::{DATAGRAM_PAYLOAD_FLOOR, Flags, Header, TAG_BYTES, VIDEO_CLOCK_HZ};
use throcc_proto::ids::{Epoch, MediaId, UserId};
use throcc_proto::messages::{Audience, Codec, Event, MediaState, Recovery, Request, Response};
use throcc_proto::transport::close;
use throcc_server::{AUDIO_BURST_PACKETS, AUDIO_PACKETS_PER_SECOND};

use common::unit::{unit, unit_index};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    debug_assertions,
    ignore = "a debug build can't encrypt 15 Mbps on CI runners"
)]
async fn ten_seconds_of_frames_at_15_mbps_arrive_intact() {
    const FPS: u32 = 60;
    const UNITS: u32 = 10 * FPS;
    const LEN: usize = 15_000_000 / 8 / FPS as usize;
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;

    let share = sharing(share_1080p60(Codec::Av1, 15_000));
    assert_eq!(
        alice.request(Request::SetMedia(share)).await.unwrap(),
        Response::Ok
    );
    let epoch = alice.engine.keys().epoch();
    let video_id = first_share(&bob, &alice).await.video;
    let mut units = watch(&bob, video_id, UNITS as usize).await;
    wait_for("bob's key for alice's video", || {
        bob.engine.keys().recv_key(video_id, epoch).is_some()
    })
    .await;
    wait_for("both at one epoch", || {
        alice.engine.keys().epoch() == bob.engine.keys().epoch()
    })
    .await;

    let started = Instant::now();
    let sender = alice.engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1) / FPS);
        for index in 0..UNITS {
            tick.tick().await;
            let timestamp = index * (VIDEO_CLOCK_HZ / FPS);
            let sent = Outgoing::new(video_id, unit(index, LEN), timestamp, index == 0);
            sender.send(Kind::Video, sent);
        }
    });

    let mut intact = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while intact < UNITS {
        let received = tokio::time::timeout_at(deadline, units.recv())
            .await
            .expect("units stopped arriving")
            .unwrap();
        let index = unit_index(&received.data).unwrap();
        assert_eq!(received.data, unit(index, LEN), "unit {index} corrupted");
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
    assert!(mbps >= 14.0, "only {mbps:.1} Mbps");
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
    let video_id = first_share(&bob, &alice).await.video;
    let watching = Request::SetWatched(vec![video_id]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);
    let requested = server_event(&mut alice, |e| match e {
        Event::KeyframeRequested { media_id, kind } => Some((media_id, kind)),
        _ => None,
    })
    .await;
    assert_eq!(requested, (video_id, Recovery::Idr));
}

#[tokio::test]
async fn the_room_learns_who_watches_a_share() {
    let server = start_server();
    let (mut alice, bob, room) = room_with_two(&server).await;
    start_sharing(&alice).await;
    let video = first_share(&bob, &alice).await.video;
    let watching = Request::SetWatched(vec![video]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);
    assert_eq!(audience(&mut alice, video).await, vec![bob.me()]);

    let carol = enroll(&server, &mint_admin_invite(server.dir.path())).await;
    let carol = Call::start(carol.link, &carol.keystore).unwrap();
    let (placement, _) = carol.join(Some(room), None).await.unwrap();
    let expected = Audience {
        video,
        receivers: vec![bob.me()],
    };
    assert_eq!(placement.audiences.shares, vec![expected]);

    bob.join(None, None).await.unwrap();
    assert_eq!(audience(&mut alice, video).await, vec![]);
}

#[tokio::test]
async fn a_share_that_ends_has_no_audience() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    start_sharing(&alice).await;
    let video = first_share(&bob, &alice).await.video;
    let watching = Request::SetWatched(vec![video]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);
    assert_eq!(audience(&mut alice, video).await, vec![bob.me()]);

    let stopped = Request::SetMedia(MediaState::default());
    assert_eq!(alice.request(stopped).await.unwrap(), Response::Ok);
    assert_eq!(audience(&mut alice, video).await, vec![]);
}

#[tokio::test]
async fn hearing_a_share_counts_as_watching_it() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    start_sharing(&alice).await;
    let share = first_share(&bob, &alice).await;
    let listening = Request::SetWatched(vec![share.audio]);
    assert_eq!(bob.request(listening).await.unwrap(), Response::Ok);
    assert_eq!(audience(&mut alice, share.video).await, vec![bob.me()]);

    let deafened = MediaState {
        deafened: true,
        ..MediaState::default()
    };
    assert_eq!(
        bob.request(Request::SetMedia(deafened)).await.unwrap(),
        Response::Ok
    );
    assert_eq!(audience(&mut alice, share.video).await, vec![]);
}

#[tokio::test]
async fn nobody_outside_the_room_learns_who_watches() {
    let server = start_server();
    let (mut alice, bob, _) = room_with_two(&server).await;
    let mut carol = enroll(&server, &mint_admin_invite(server.dir.path())).await;
    let elsewhere = create_room(&mut carol, "Elsewhere").await;
    let mut carol = Call::start(carol.link, &carol.keystore).unwrap();
    let (placement, _) = carol.join(Some(elsewhere), None).await.unwrap();
    let carol_seq = placement.audiences.seq;
    let dave = enroll(&server, &mint_admin_invite(server.dir.path())).await;
    let mut dave = Call::start(dave.link, &dave.keystore).unwrap();

    start_sharing(&alice).await;
    let video = first_share(&bob, &alice).await.video;
    let watching = Request::SetWatched(vec![video]);
    assert_eq!(bob.request(watching).await.unwrap(), Response::Ok);
    assert_eq!(audience(&mut alice, video).await, vec![bob.me()]);

    // Everyone hears of bob's mic after the audience changed, so whoever hears of it first
    // was never told who watches.
    let talking = MediaState {
        mic: true,
        ..MediaState::default()
    };
    assert_eq!(
        bob.request(Request::SetMedia(talking)).await.unwrap(),
        Response::Ok
    );
    for outsider in [&mut carol, &mut dave] {
        server_event(outsider, |e| match e {
            Event::AudiencesChanged(_) => panic!("an outsider learned who watches"),
            Event::PresenceChanged(p) if p.user == bob.me() => Some(()),
            _ => None,
        })
        .await;
    }
    let (placement, _) = carol.join(Some(elsewhere), None).await.unwrap();
    assert_eq!(
        placement.audiences.seq,
        carol_seq + 1,
        "carol's seq counted changes in another room"
    );
}

async fn start_sharing(call: &Call) {
    let share = Request::SetMedia(sharing(share_1080p60(Codec::H264, 8000)));
    assert_eq!(call.request(share).await.unwrap(), Response::Ok);
}

/// Waits for the next audiences in our room, and returns who receives `video`.
async fn audience(call: &mut Call, video: MediaId) -> Vec<UserId> {
    server_event(call, |e| match e {
        Event::AudiencesChanged(audiences) => Some(
            audiences
                .shares
                .into_iter()
                .find(|a| a.video == video)
                .map_or_else(Vec::new, |a| a.receivers),
        ),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn mic_audio_reaches_peers_but_not_the_deafened() {
    let server = start_server();
    let (alice, bob, _) = room_with_two(&server).await;
    let mic = peer_tracks(&bob, &alice).await.mic;
    let epoch = alice.engine.keys().epoch();
    let mut heard = bob.engine.subscribe(mic, Kind::Audio, 64);
    wait_for("bob's key for alice's mic", || {
        bob.engine.keys().recv_key(mic, epoch).is_some()
    })
    .await;

    let frame = |i: u32| Outgoing::new(mic, Bytes::from(vec![i as u8; 80]), i * 960, false);
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
    let relay = lossy_relay(&server.addr, Toward::Client, 50).await;
    let (alice, bob, _) = room_with_two_at(&server, &relay.addr).await;
    let share = sharing(share_1080p60(Codec::H264, 10_000));
    assert_eq!(
        alice.request(Request::SetMedia(share)).await.unwrap(),
        Response::Ok
    );
    let video_id = first_share(&bob, &alice).await.video;
    let mut units = watch(&bob, video_id, KEYFRAMES as usize).await;
    wait_for("both at one epoch with keys", || {
        let epoch = alice.engine.keys().epoch();
        epoch == bob.engine.keys().epoch() && bob.engine.keys().recv_key(video_id, epoch).is_some()
    })
    .await;

    relay.lossy.store(true, Ordering::Relaxed);
    for index in 0..KEYFRAMES {
        let keyframe = Outgoing::new(video_id, unit(index, LEN), index * 3000, true);
        alice.engine.send(Kind::Video, keyframe);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut whole = std::collections::HashSet::new();
    while whole.len() < KEYFRAMES as usize
        && let Ok(Some(received)) = tokio::time::timeout(Duration::from_secs(1), units.recv()).await
    {
        let index = unit_index(&received.data).unwrap();
        assert_eq!(received.data, unit(index, LEN));
        whole.insert(index);
    }
    let whole = whole.len() as u32;
    let dropped = relay.dropped.load(Ordering::Relaxed);
    let sent = alice.engine.stats.sent_datagrams.load(Ordering::Relaxed);
    let stats = &bob.engine.stats;
    eprintln!(
        "{whole} of {KEYFRAMES} keyframes whole, {dropped} packets dropped, {sent} datagrams sent; \
         bob: {} duplicates, {} abandoned, {} overflowed, {} too old, {} lost",
        stats.duplicates.load(Ordering::Relaxed),
        stats.abandoned_units.load(Ordering::Relaxed),
        stats.track_overflow.load(Ordering::Relaxed),
        stats.too_old.load(Ordering::Relaxed),
        bob.engine.arrived(video_id).lost,
    );
    assert!(dropped >= 30, "the relay dropped only {dropped} packets");
    assert!(
        whole >= KEYFRAMES * 8 / 10,
        "only {whole} of {KEYFRAMES} keyframes whole"
    );
}

#[tokio::test]
async fn a_unit_lost_on_the_way_asks_its_sender_for_a_refresh() {
    const UNITS: u32 = 3_000;
    let server = start_server();
    let relay = lossy_relay(&server.addr, Toward::Client, 5).await;
    let (mut alice, bob, _) = room_with_two_at(&server, &relay.addr).await;
    start_sharing(&alice).await;
    let video_id = first_share(&bob, &alice).await.video;
    let _units = watch(&bob, video_id, UNITS as usize).await;
    wait_for("both at one epoch with keys", || {
        let epoch = alice.engine.keys().epoch();
        epoch == bob.engine.keys().epoch() && bob.engine.keys().recv_key(video_id, epoch).is_some()
    })
    .await;

    relay.lossy.store(true, Ordering::Relaxed);
    let abandoned = || bob.engine.stats.abandoned_units.load(Ordering::Relaxed);
    let mut index = 0;
    while abandoned() == 0 {
        assert!(index < UNITS, "no unit was abandoned in {UNITS} sent");
        let sent = Outgoing::new(video_id, unit(index, 20 * 1100), index * 1500, false);
        alice.engine.send(Kind::Video, sent);
        tokio::time::sleep(Duration::from_millis(10)).await;
        index += 1;
    }
    let refresh = Event::KeyframeRequested {
        media_id: video_id,
        kind: Recovery::Refresh,
    };
    server_event(&mut alice, |e| (e == refresh).then_some(())).await;
}

#[tokio::test]
async fn a_member_who_sends_too_fast_is_disconnected() {
    let server = start_server();
    let alice = admin(&server).await;
    let code = invite(&alice).await;
    let mut config = throcc_client_core::connection::transport_config();
    config.datagram_send_buffer_size(512 * 1024);
    let bob = raw_enroll(&server.addr, &code, config).await;
    let connection = &bob.connected.connection;

    let datagram = Bytes::from(vec![0u8; 1100]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while connection.close_reason().is_none() && Instant::now() < deadline {
        if connection
            .send_datagram_wait(datagram.clone())
            .await
            .is_err()
        {
            break;
        }
    }

    let reason = tokio::time::timeout(Duration::from_secs(5), connection.closed())
        .await
        .expect("bob was never disconnected");
    let quinn::ConnectionError::ApplicationClosed(closed) = reason else {
        panic!("bob's connection ended with {reason:?}");
    };
    assert_eq!(closed.error_code, close::PROTOCOL_ERROR.into());
    assert_eq!(&closed.reason[..], b"sent media too fast");
}

#[tokio::test]
async fn a_datagram_larger_than_any_client_seals_is_not_forwarded() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let mic = alice.join(Some(general)).await.tracks.unwrap().mic;
    bob.join(Some(general)).await;

    let connection = &alice.link.connected.connection;
    let too_large = DATAGRAM_PAYLOAD_FLOOR + 1;
    wait_for("alice's path to carry a larger datagram", || {
        connection
            .max_datagram_size()
            .is_some_and(|max| max >= too_large)
    })
    .await;
    for (seq, len) in [(0, too_large), (1, DATAGRAM_PAYLOAD_FLOOR)] {
        let header = Header {
            media_id: mic,
            epoch: Epoch(0),
            seq,
            flags: Flags::FIRST | Flags::LAST,
        };
        let mut datagram = header.encode().to_vec();
        datagram.resize(len, 0);
        connection
            .send_datagram_wait(datagram.into())
            .await
            .unwrap();
    }

    let forwarded = bob
        .event(|event| match event {
            Event::ReceiveStats { tracks } => tracks
                .iter()
                .find(|t| t.media_id == mic)
                .map(|t| t.forwarded),
            _ => None,
        })
        .await;
    assert_eq!(forwarded, 1);
}

#[tokio::test]
async fn a_member_flooding_their_mic_is_held_to_its_allowance_and_stays_connected() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let mic = alice.join(Some(general)).await.tracks.unwrap().mic;
    bob.join(Some(general)).await;

    let connection = &alice.link.connected.connection;
    let flooding = Instant::now();
    for seq in 0..20_000 {
        let header = Header {
            media_id: mic,
            epoch: Epoch(0),
            seq,
            flags: Flags::FIRST | Flags::LAST,
        };
        let mut datagram = header.encode().to_vec();
        datagram.extend([0; TAG_BYTES]);
        connection
            .send_datagram_wait(datagram.into())
            .await
            .unwrap();
    }
    let flooded_for = flooding.elapsed().as_secs_f64();

    let mut forwarded = 0;
    // The server reports receive stats every 2 s, so a second more without one means it is done.
    let quiet = Duration::from_secs(3);
    while let Ok(Some(event)) = tokio::time::timeout(quiet, bob.link.events.recv()).await {
        if let Event::ReceiveStats { tracks } = event {
            forwarded += tracks
                .iter()
                .filter(|t| t.media_id == mic)
                .map(|t| t.forwarded)
                .sum::<u32>();
        }
    }
    let allowed = AUDIO_BURST_PACKETS + AUDIO_PACKETS_PER_SECOND * (flooded_for + 1.0);
    assert!(
        f64::from(forwarded) >= AUDIO_BURST_PACKETS * 0.9 && f64::from(forwarded) <= allowed,
        "forwarded {forwarded} of 20000 in {flooded_for:.1} s"
    );
    assert!(connection.close_reason().is_none());
    assert_eq!(alice.join(Some(general)).await.room, Some(general));
}
