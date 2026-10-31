use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::media::engine::{Kind, Outgoing};
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Codec, Event, MediaState, Presence, Request, Response, ShareConfig};

const AUDIO_FRAME: Duration = Duration::from_millis(20);
const AUDIO_BYTES: usize = 80;
const QUEUE_UNITS: usize = 4096;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Track {
    Mic,
    Video,
}

impl Track {
    fn kind(self) -> Kind {
        match self {
            Track::Mic => Kind::Audio,
            Track::Video => Kind::Video,
        }
    }

    fn pick(self, (mic, video): (MediaId, MediaId)) -> MediaId {
        match self {
            Track::Mic => mic,
            Track::Video => video,
        }
    }
}

pub struct SendOptions {
    pub track: Track,
    pub size: usize,
    pub rate: u32,
    pub count: u32,
    /// Also sends a 50 frames/s mic track while the main track runs.
    pub with_audio: bool,
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

fn outgoing(media_id: MediaId, index: u32, len: usize, timestamp: u32) -> Outgoing {
    Outgoing {
        media_id,
        data: unit(index, len),
        timestamp,
        keyframe: index == 0,
        discardable: false,
        enqueued: Instant::now(),
    }
}

pub async fn send(call: &mut Call, room: RoomId, options: SendOptions) -> Result<()> {
    let SendOptions {
        track,
        size,
        rate,
        count,
        with_audio,
    } = options;
    if with_audio && matches!(track, Track::Mic) {
        bail!("--with-audio needs --track video");
    }
    let (placed, _) = call.join(Some(room)).await?;
    let tracks = placed.tracks.context("joined without tracks")?;
    let media_id = track.pick((tracks.mic, tracks.shares[0].video));
    let kind = track.kind();
    let share = matches!(track, Track::Video).then(|| ShareConfig {
        audio: false,
        kbps: (size as u64 * 8 * u64::from(rate) / 1000) as u32,
        codec: Codec::Av1,
        width: 1920,
        height: 1080,
        fps: 60,
    });
    let media = MediaState {
        mic: with_audio || matches!(track, Track::Mic),
        deafened: false,
        share,
    };
    expect_ok(call.request(Request::SetMedia(media)).await?)?;
    println!(
        "sending {count} units of {size} bytes at {rate}/s on track {}",
        media_id.0
    );

    let started = Instant::now();
    let unit_interval = Duration::from_secs_f64(1.0 / f64::from(rate.max(1)));
    let mut next_unit = tokio::time::Instant::now();
    let mut next_audio = tokio::time::Instant::now();
    let (mut sent, mut audio_sent) = (0u32, 0u32);
    let mut last_keyframe: Option<Instant> = None;
    let mut min_keyframe_gap: Option<Duration> = None;
    let finish = tokio::time::Instant::now() + unit_interval * count + Duration::from_millis(500);
    while tokio::time::Instant::now() < finish {
        tokio::select! {
            _ = tokio::time::sleep_until(next_unit), if sent < count => {
                call.engine.send(kind, outgoing(media_id, sent, size, sent.wrapping_mul(90_000 / rate.max(1))));
                sent += 1;
                next_unit += unit_interval;
            }
            _ = tokio::time::sleep_until(next_audio), if with_audio && sent < count => {
                call.engine.send(Kind::Audio, outgoing(tracks.mic, audio_sent, AUDIO_BYTES, audio_sent.wrapping_mul(960)));
                audio_sent += 1;
                next_audio += AUDIO_FRAME;
            }
            event = call.events.recv() => match event {
                Some(CallEvent::Server(Event::KeyframeWanted { media_id: wanted, .. })) if wanted == media_id => {
                    let now = Instant::now();
                    if let Some(last) = last_keyframe {
                        let gap = now.duration_since(last);
                        min_keyframe_gap = Some(min_keyframe_gap.map_or(gap, |min| min.min(gap)));
                    }
                    last_keyframe = Some(now);
                }
                Some(_) => {}
                None => bail!("disconnected"),
            },
            _ = tokio::time::sleep_until(finish) => {}
        }
    }
    let stats = &call.engine.stats;
    println!(
        "RESULT role=sender units={sent} audio_frames={audio_sent} seconds={:.1} datagrams={} local_drops={} queue_full={} min_keyframe_gap_ms={}",
        started.elapsed().as_secs_f64(),
        stats.sent_datagrams.load(Ordering::Relaxed),
        stats.send_dropped_units.load(Ordering::Relaxed),
        stats.send_queue_full.load(Ordering::Relaxed),
        min_keyframe_gap.map_or("none".to_owned(), |gap| gap.as_millis().to_string()),
    );
    Ok(())
}

pub struct RecvOptions<'a> {
    pub from: &'a str,
    pub track: Track,
    pub count: u32,
    pub with_audio: bool,
    /// Stops after this long without a unit.
    pub idle: Duration,
}

pub async fn recv(call: &mut Call, room: RoomId, options: RecvOptions<'_>) -> Result<()> {
    let RecvOptions {
        from,
        track,
        count,
        with_audio,
        idle,
    } = options;
    if with_audio && matches!(track, Track::Mic) {
        bail!("--with-audio needs --track video");
    }
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
    let (mut mic, video) = wait_for_tracks(call, sender).await?;
    let mut media_id = track.pick((mic, video));
    let mut units = subscribe(call, media_id, track).await?;
    let mut audio = (with_audio && media_id != mic)
        .then(|| call.engine.subscribe(mic, Kind::Audio, QUEUE_UNITS));
    println!("receiving track {} from user {}", media_id.0, sender.0);

    let (mut intact, mut corrupt, mut bytes, mut audio_frames) = (0u32, 0u32, 0usize, 0u32);
    let (mut server_dropped, mut audio_server_dropped) = (0u64, 0u64);
    let mut started = None;
    let mut last = tokio::time::Instant::now();
    while intact + corrupt < count {
        tokio::select! {
            received = units.recv() => {
                let Some(received) = received else { break };
                started.get_or_insert_with(Instant::now);
                last = tokio::time::Instant::now();
                let index = received.data.first_chunk().map_or(u32::MAX, |b| u32::from_be_bytes(*b));
                if received.data == unit(index, received.data.len()) {
                    intact += 1;
                } else {
                    corrupt += 1;
                }
                bytes += received.data.len();
            }
            frame = async { audio.as_mut()?.recv().await }, if audio.is_some() => {
                if frame.is_some() {
                    audio_frames += 1;
                }
            }
            event = call.events.recv() => match event {
                Some(CallEvent::Server(Event::RoomState { .. })) => {
                    if let Some(current) = peer_tracks(call, sender)
                        && track.pick(current) != media_id
                    {
                        call.engine.unsubscribe(media_id);
                        if audio.take().is_some() {
                            call.engine.unsubscribe(mic);
                        }
                        mic = current.0;
                        media_id = track.pick(current);
                        units = subscribe(call, media_id, track).await?;
                        if with_audio && media_id != mic {
                            audio = Some(call.engine.subscribe(mic, Kind::Audio, QUEUE_UNITS));
                        }
                        println!("the sender rejoined; now receiving track {}", media_id.0);
                    }
                }
                Some(CallEvent::Server(Event::LinkStats { tracks })) => {
                    for t in tracks {
                        if t.media_id == media_id {
                            server_dropped += u64::from(t.dropped_units);
                        } else if t.media_id == mic {
                            audio_server_dropped += u64::from(t.dropped_units);
                        }
                    }
                }
                Some(_) => {}
                None => bail!("disconnected"),
            },
            _ = tokio::time::sleep_until(last + idle) => {
                println!("no unit for {idle:?}; stopping");
                break;
            }
        }
    }
    let secs = started
        .map_or(0.0, |s| s.elapsed().as_secs_f64())
        .max(0.001);
    let stats = &call.engine.stats;
    println!(
        "RESULT role=receiver intact={intact} corrupt={corrupt} mbps={:.2} server_dropped_units={server_dropped} lost_datagrams={} decrypt_failures={} abandoned_units={} late_units={} audio_frames={audio_frames} audio_lost={} audio_server_dropped={audio_server_dropped}",
        bytes as f64 * 8.0 / secs / 1e6,
        call.engine.lost(media_id),
        stats.decrypt_failures.load(Ordering::Relaxed),
        stats.abandoned_units.load(Ordering::Relaxed),
        stats.late_units.load(Ordering::Relaxed),
        call.engine.lost(mic),
    );
    Ok(())
}

async fn subscribe(
    call: &Call,
    media_id: MediaId,
    track: Track,
) -> Result<tokio::sync::mpsc::Receiver<throcc_client_core::media::engine::Received>> {
    let units = call.engine.subscribe(media_id, track.kind(), QUEUE_UNITS);
    if let Track::Video = track {
        expect_ok(call.request(Request::SetWatching(vec![media_id])).await?)?;
    }
    Ok(units)
}

fn find_user(presence: &[Presence], query: &str) -> Option<UserId> {
    presence
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(query) || p.user.0.to_string() == query)
        .map(|p| p.user)
}

/// The mic and first video track of `user`, once this call trusts them.
fn peer_tracks(call: &Call, user: UserId) -> Option<(MediaId, MediaId)> {
    let keys = call.engine.keys();
    let tracks = keys.keys.peer_tracks(user)?;
    Some((tracks.mic, tracks.shares[0].video))
}

async fn wait_for_tracks(call: &Call, user: UserId) -> Result<(MediaId, MediaId)> {
    for _ in 0..500 {
        if let Some(tracks) = peer_tracks(call, user) {
            return Ok(tracks);
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
