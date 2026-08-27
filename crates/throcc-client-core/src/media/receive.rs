use std::collections::HashMap;

use quinn::Connection;
use throcc_proto::{FrameHeader, HEADER_BYTES, MediaId, Tracks, UserId};
use tokio::sync::{broadcast, mpsc};

use crate::client::Event;
use crate::media::audio::jitter::{self, JitterBuffer};
use crate::media::audio::playout::{self, TrackFeed};
use crate::media::fragment::Reassembler;
use crate::media::sequence::SequenceWindow;
use crate::media::transform::{FrameTransform, Passthrough};

#[derive(Default)]
struct Track {
    window: SequenceWindow,
    reassembler: Reassembler,
}

/// The shared receive path. Each track's audio goes to that track's own jitter
/// buffer, so one stalled peer stalls only itself.
pub async fn receive(
    connection: Connection,
    events: broadcast::Sender<Event>,
    feeds: mpsc::Sender<TrackFeed>,
    peers: Vec<(UserId, Tracks)>,
) {
    let mut room = Room::new(peers);
    let mut membership = events.subscribe();
    let mut tracks: HashMap<MediaId, Track> = HashMap::new();
    let mut playing: HashMap<MediaId, Option<mpsc::Sender<neteq::AudioPacket>>> = HashMap::new();
    let mut transform: Box<dyn FrameTransform> = Box::new(Passthrough);

    loop {
        let datagram = tokio::select! {
            arrived = connection.read_datagram() => match arrived {
                Ok(datagram) => datagram,
                Err(_) => return,
            },
            event = membership.recv() => {
                match event {
                    Ok(event) => {
                        for gone in room.apply(&event) {
                            tracks.remove(&gone);
                            playing.remove(&gone);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "the receive path fell behind the roster");
                    }
                }
                continue;
            }
        };

        let Ok(header) = FrameHeader::decode(&datagram) else {
            tracing::debug!(bytes = datagram.len(), "dropping a truncated datagram");
            continue;
        };
        if header.has_unknown_flags() {
            tracing::debug!(flags = header.flags, "dropping a frame with unknown flags");
            continue;
        }

        let track = tracks.entry(header.media_id).or_default();
        if !track.window.accept(header.seq) {
            continue;
        }

        let mut payload = datagram[HEADER_BYTES..].to_vec();
        if let Err(e) = transform.inbound(&header, &mut payload) {
            tracing::debug!(error = %e, media_id = %header.media_id, "the transform refused a fragment");
            continue;
        }

        for unit in track.reassembler.push(header.seq, &payload) {
            if room.is_audio(header.media_id) {
                play(
                    &mut playing,
                    &feeds,
                    header.media_id,
                    unit.last_seq,
                    &unit.bytes,
                );
            }
            let delivered = events.send(Event::Media {
                media_id: header.media_id,
                timestamp: unit.timestamp,
                bytes: unit.bytes,
            });
            if delivered.is_err() {
                return;
            }
        }
    }
}

fn play(
    playing: &mut HashMap<MediaId, Option<mpsc::Sender<neteq::AudioPacket>>>,
    feeds: &mpsc::Sender<TrackFeed>,
    media_id: MediaId,
    seq: u32,
    frame: &[u8],
) {
    let sending = playing
        .entry(media_id)
        .or_insert_with(|| open_jitter_buffer(feeds, media_id));

    if let Some(sending) = sending
        && sending
            .try_send(jitter::packet(media_id, seq, frame))
            .is_err()
    {
        tracing::debug!(%media_id, "this track's jitter buffer is not keeping up");
    }
}

fn open_jitter_buffer(
    feeds: &mpsc::Sender<TrackFeed>,
    media_id: MediaId,
) -> Option<mpsc::Sender<neteq::AudioPacket>> {
    let jitter = match JitterBuffer::new() {
        Ok(jitter) => jitter,
        Err(e) => {
            tracing::warn!(error = %e, %media_id, "could not build a jitter buffer");
            return None;
        }
    };

    let (sending, packets) = mpsc::channel(playout::PACKET_QUEUE_DEPTH);
    let feed = TrackFeed {
        media_id,
        packets,
        jitter,
    };
    match feeds.try_send(feed) {
        Ok(()) => Some(sending),
        Err(_) => {
            tracing::debug!(%media_id, "nothing is playing audio out, so this track is not played");
            None
        }
    }
}

/// Which media ids belong to whom, which is what says an id carries audio and
/// what says a departure tears a track down.
struct Room {
    peers: HashMap<UserId, Tracks>,
}

impl Room {
    fn new(peers: Vec<(UserId, Tracks)>) -> Self {
        Self {
            peers: peers.into_iter().collect(),
        }
    }

    /// The media ids that have gone away with this event.
    fn apply(&mut self, event: &Event) -> Vec<MediaId> {
        match event {
            Event::Placed(placed) => {
                let departed = self.peers.values().flat_map(media_ids).collect();
                self.peers = placed
                    .peers
                    .iter()
                    .map(|peer| (peer.user, peer.tracks.clone()))
                    .collect();
                departed
            }
            Event::UserEntered { peer, .. } => {
                self.peers.insert(peer.user, peer.tracks.clone());
                Vec::new()
            }
            Event::UserExited { user, .. } => self
                .peers
                .remove(user)
                .as_ref()
                .map(media_ids)
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn is_audio(&self, media_id: MediaId) -> bool {
        self.peers.values().any(|tracks| {
            tracks.mic == media_id
                || tracks
                    .shares
                    .iter()
                    .any(|share| share.audio == Some(media_id))
        })
    }
}

fn media_ids(tracks: &Tracks) -> Vec<MediaId> {
    let mut ids = vec![tracks.mic];
    for share in &tracks.shares {
        ids.push(share.video);
        ids.extend(share.audio);
    }
    ids
}
