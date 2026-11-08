use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::control;
use throcc_client_core::media::engine::{Kind, Outgoing, Received};
use throcc_client_core::media::fragment::{
    MAX_AUDIO_UNIT_FRAGMENTS, MAX_VIDEO_UNIT_FRAGMENTS, max_unit_bytes,
};
use throcc_proto::frame::{AUDIO_CLOCK_HZ, VIDEO_CLOCK_HZ};
use throcc_proto::ids::{MediaId, RoomId, UserId};
use throcc_proto::messages::{Codec, Event, MediaState, Presence, Request, ShareConfig};
use tokio::sync::mpsc;

use crate::unit::{is_unit, unit};

const AUDIO_FRAME: Duration = Duration::from_millis(20);
const AUDIO_FRAME_TICKS: u32 = AUDIO_CLOCK_HZ / 50;
const AUDIO_BYTES: usize = 80;
const QUEUE_UNITS: usize = 4096;
/// How long a found sender may take to become a trusted member of the room.
const TRUST_WAIT: Duration = Duration::from_secs(5);

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

    /// Refuses a unit size receivers would drop, before anything connects.
    pub fn check_size(self, size: usize) -> Result<()> {
        let most = max_unit_bytes(match self {
            Track::Mic => MAX_AUDIO_UNIT_FRAGMENTS,
            Track::Video => MAX_VIDEO_UNIT_FRAGMENTS,
        });
        if size > most {
            bail!("--size can be at most {most} bytes on this track; receivers drop longer units");
        }
        Ok(())
    }

    /// Refuses `--with-audio` beside `--track mic`, before anything connects.
    pub fn check_with_audio(self, with_audio: bool) -> Result<()> {
        if with_audio && matches!(self, Track::Mic) {
            bail!("--with-audio needs --track video");
        }
        Ok(())
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

fn outgoing(media_id: MediaId, index: u32, len: usize, timestamp: u32) -> Outgoing {
    Outgoing::new(media_id, unit(index, len), timestamp, index == 0)
}

pub async fn send(call: &mut Call, room: RoomId, options: SendOptions) -> Result<()> {
    let SendOptions {
        track,
        size,
        rate,
        count,
        with_audio,
    } = options;
    let (placement, _) = call.join(Some(room), None).await?;
    let tracks = placement.tracks.context("joined without tracks")?;
    let share_video = tracks
        .shares
        .first()
        .context("joined without a share track")?
        .video;
    let media_id = track.pick((tracks.mic, share_video));
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
    control::expect_ok(call.request(Request::SetMedia(media)).await?)
        .map_err(anyhow::Error::msg)?;
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
                call.engine.send(kind, outgoing(media_id, sent, size, sent.wrapping_mul(VIDEO_CLOCK_HZ / rate.max(1))));
                sent += 1;
                next_unit += unit_interval;
            }
            _ = tokio::time::sleep_until(next_audio), if with_audio && sent < count => {
                call.engine.send(Kind::Audio, outgoing(tracks.mic, audio_sent, AUDIO_BYTES, audio_sent.wrapping_mul(AUDIO_FRAME_TICKS)));
                audio_sent += 1;
                next_audio += AUDIO_FRAME;
            }
            event = call.events.recv() => match event {
                Some(CallEvent::Server(Event::KeyframeRequested { media_id: requested, .. })) if requested == media_id => {
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
    call.join(Some(room), None).await?;
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
    let (mut mic, video) = tokio::time::timeout(TRUST_WAIT, trusted_tracks(call, sender))
        .await
        .with_context(|| format!("user {} isn't a trusted member of this room", sender.0))??;
    let mut media_id = track.pick((mic, video));
    let (mut units, mut audio) = subscribe(call, (mic, video), track, with_audio).await?;
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
                if is_unit(&received.data) {
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
                Some(CallEvent::Server(Event::PeersChanged { .. })) => {
                    if let Some(current) = peer_tracks(call, sender)
                        && track.pick(current) != media_id
                    {
                        call.engine.unsubscribe(media_id);
                        if audio.is_some() {
                            call.engine.unsubscribe(mic);
                        }
                        mic = current.0;
                        media_id = track.pick(current);
                        (units, audio) = subscribe(call, current, track, with_audio).await?;
                        println!("the sender rejoined; now receiving track {}", media_id.0);
                    }
                }
                Some(CallEvent::Server(Event::ReceiveStats { tracks })) => {
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
    let count = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    println!(
        "RESULT role=receiver intact={intact} corrupt={corrupt} mbps={:.2} server_dropped_units={server_dropped} lost_datagrams={} decrypt_failures={} abandoned_units={} late_units={} duplicates={} too_old={} unknown_track={} malformed={} track_overflow={} audio_frames={audio_frames} audio_lost={} audio_server_dropped={audio_server_dropped}",
        bytes as f64 * 8.0 / secs / 1e6,
        call.engine.arrived(media_id).lost,
        count(&stats.decrypt_failures),
        count(&stats.abandoned_units),
        count(&stats.late_units),
        count(&stats.duplicates),
        count(&stats.too_old),
        count(&stats.unknown_track),
        count(&stats.malformed),
        count(&stats.track_overflow),
        call.engine.arrived(mic).lost,
    );
    Ok(())
}

/// Subscribes to `track` of the sender's `(mic, video)`, and to the mic too `with_audio`.
async fn subscribe(
    call: &Call,
    tracks: (MediaId, MediaId),
    track: Track,
    with_audio: bool,
) -> Result<(mpsc::Receiver<Received>, Option<mpsc::Receiver<Received>>)> {
    let media_id = track.pick(tracks);
    let units = call.engine.subscribe(media_id, track.kind(), QUEUE_UNITS);
    if let Track::Video = track {
        let watching = call.request(Request::SetWatched(vec![media_id])).await?;
        control::expect_ok(watching).map_err(anyhow::Error::msg)?;
    }
    let audio = with_audio.then(|| call.engine.subscribe(tracks.0, Kind::Audio, QUEUE_UNITS));
    Ok((units, audio))
}

fn find_user(presence: &[Presence], query: &str) -> Option<UserId> {
    presence
        .iter()
        .find(|p| crate::names(query, &p.name, p.user.0))
        .map(|p| p.user)
}

/// The mic and first video track of `user`, once this call trusts them.
fn peer_tracks(call: &Call, user: UserId) -> Option<(MediaId, MediaId)> {
    let keys = call.engine.keys();
    let tracks = keys.peer_tracks(user)?;
    Some((tracks.mic, tracks.shares.first()?.video))
}

/// Reads events until this call trusts `user`'s tracks. The call updates its keys from each
/// event before passing it on, so checking after every event misses nothing.
async fn trusted_tracks(call: &mut Call, user: UserId) -> Result<(MediaId, MediaId)> {
    loop {
        if let Some(tracks) = peer_tracks(call, user) {
            return Ok(tracks);
        }
        if call.events.recv().await.is_none() {
            bail!("disconnected");
        }
    }
}
