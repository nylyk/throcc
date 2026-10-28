use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::media::engine::{Kind, Outgoing};
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Codec, Event, MediaState, Presence, Request, Response, ShareConfig};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Track {
    Mic,
    Video,
}

/// Fake access unit `index`: its index, then bytes derived from it.
pub fn unit(index: u32, len: usize) -> Bytes {
    let mut data = vec![0u8; len.max(4)];
    data[..4].copy_from_slice(&index.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(4) {
        *b = (index as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    Bytes::from(data)
}

pub async fn send(
    call: &Call,
    room: RoomId,
    track: Track,
    size: usize,
    rate: u32,
    count: u32,
) -> Result<()> {
    let (placed, _) = call.join(Some(room)).await?;
    let tracks = placed.tracks.context("joined without tracks")?;
    let (media_id, kind) = match track {
        Track::Mic => (tracks.mic, Kind::Audio),
        Track::Video => (tracks.shares[0].video, Kind::Video),
    };
    if let Track::Video = track {
        let kbps = (size as u64 * 8 * u64::from(rate) / 1000) as u32;
        let share = ShareConfig {
            audio: false,
            kbps,
            codec: Codec::Av1,
            width: 1920,
            height: 1080,
            fps: 60,
        };
        expect_ok(
            call.request(Request::SetMedia(MediaState {
                mic: false,
                deafened: false,
                share: Some(share),
            }))
            .await?,
        )?;
    }
    println!(
        "sending {count} units of {size} bytes at {rate}/s on track {}",
        media_id.0
    );
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    let per_tick = (rate / 100).max(1);
    let mut sent = 0;
    while sent < count {
        tick.tick().await;
        for _ in 0..per_tick.min(count - sent) {
            let outgoing = Outgoing {
                media_id,
                data: unit(sent, size),
                timestamp: sent.wrapping_mul(90_000 / rate.max(1)),
                keyframe: sent == 0,
                discardable: false,
                enqueued: Instant::now(),
            };
            call.engine.send(kind, outgoing);
            sent += 1;
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stats = &call.engine.stats;
    println!(
        "sent {sent} units in {:.1}s: {} datagrams, {} dropped locally, {} queue full",
        started.elapsed().as_secs_f64(),
        stats.sent_datagrams.load(Ordering::Relaxed),
        stats.send_dropped_units.load(Ordering::Relaxed),
        stats.send_queue_full.load(Ordering::Relaxed),
    );
    Ok(())
}

pub async fn recv(
    call: &mut Call,
    room: RoomId,
    from: &str,
    track: Track,
    count: u32,
) -> Result<()> {
    call.join(Some(room)).await?;
    let mut presence: Vec<Presence> = call.welcome.presence.clone();
    let sender = loop {
        if let Some(user) = find_user(&presence, from) {
            break user;
        }
        match call.events.recv().await {
            Some(CallEvent::Server(Event::PresenceChanged(p))) => presence.push(p),
            Some(_) => {}
            None => bail!("disconnected"),
        }
    };
    let tracks = wait_for_tracks(call, sender).await?;
    let (media_id, kind) = match track {
        Track::Mic => (tracks.0, Kind::Audio),
        Track::Video => (tracks.1, Kind::Video),
    };
    let mut units = call.engine.subscribe(media_id, kind, 4096);
    if let Track::Video = track {
        expect_ok(call.request(Request::SetWatching(vec![media_id])).await?)?;
    }
    println!("receiving track {} from user {}", media_id.0, sender.0);

    let (mut intact, mut corrupt, mut bytes) = (0u32, 0u32, 0usize);
    let mut started = None;
    while intact + corrupt < count {
        let Ok(Some(received)) = tokio::time::timeout(Duration::from_secs(10), units.recv()).await
        else {
            println!("no unit for 10 s; stopping");
            break;
        };
        started.get_or_insert_with(Instant::now);
        let index = received
            .data
            .get(..4)
            .map_or(u32::MAX, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        if received.data == unit(index, received.data.len()) {
            intact += 1;
        } else {
            corrupt += 1;
        }
        bytes += received.data.len();
    }
    let secs = started
        .map_or(0.0, |s| s.elapsed().as_secs_f64())
        .max(0.001);
    let stats = &call.engine.stats;
    println!(
        "{intact} intact, {corrupt} corrupt, {:.1} Mbps, {} lost datagrams, {} decrypt failures, {} abandoned units",
        bytes as f64 * 8.0 / secs / 1e6,
        call.engine.lost(media_id),
        stats.decrypt_failures.load(Ordering::Relaxed),
        stats.abandoned_units.load(Ordering::Relaxed),
    );
    Ok(())
}

fn find_user(presence: &[Presence], query: &str) -> Option<UserId> {
    presence
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(query) || p.user.0.to_string() == query)
        .map(|p| p.user)
}

async fn wait_for_tracks(call: &Call, user: UserId) -> Result<(MediaId, MediaId)> {
    for _ in 0..500 {
        if let Some(tracks) = call.engine.keys().keys.peer_tracks(user) {
            return Ok((tracks.mic, tracks.shares[0].video));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    bail!("user {} isn't a trusted member of this room", user.0)
}

fn expect_ok(response: Response) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Err { message, .. } => bail!("{message}"),
        other => bail!("unexpected reply {other:?}"),
    }
}
