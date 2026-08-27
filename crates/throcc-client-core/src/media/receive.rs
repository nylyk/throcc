use std::collections::HashMap;

use quinn::Connection;
use throcc_proto::{FrameHeader, HEADER_BYTES, MediaId};
use tokio::sync::broadcast;

use crate::client::Event;
use crate::media::fragment::Reassembler;
use crate::media::sequence::SequenceWindow;
use crate::media::transform::{FrameTransform, Passthrough};

#[derive(Default)]
struct Track {
    window: SequenceWindow,
    reassembler: Reassembler,
}

/// The shared receive path: it parses the header, dedups, and reassembles. It
/// must never do work that can block on one peer.
pub async fn receive(connection: Connection, events: broadcast::Sender<Event>) {
    let mut tracks: HashMap<MediaId, Track> = HashMap::new();
    let mut transform: Box<dyn FrameTransform> = Box::new(Passthrough);

    while let Ok(datagram) = connection.read_datagram().await {
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
