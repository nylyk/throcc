use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use throcc_proto::frame::Header;
use throcc_proto::ids::MediaId;
use throcc_proto::messages::Recovery;

use crate::egress::{EgressHandle, Packet, TrackKind};

/// Keyframe requests for one track are forwarded at most this often.
pub const KEYFRAME_INTERVAL: Duration = Duration::from_secs(1);

pub struct Route {
    pub owner: u64,
    pub kind: TrackKind,
    pub subscribers: Vec<EgressHandle>,
}

/// Who may send on each track and who receives it. Rebuilt on membership and
/// subscription changes; read lock-free by every ingress loop.
#[derive(Default)]
pub struct ForwardingTable {
    pub routes: HashMap<MediaId, Route>,
}

#[derive(Default)]
pub struct Sfu {
    pub table: ArcSwap<ForwardingTable>,
    pub not_owned: AtomicU64,
    pub malformed: AtomicU64,
}

impl Sfu {
    /// Forwards datagrams from one connection until it closes. Never reads past the header.
    pub async fn ingress(self: Arc<Self>, conn_id: u64, connection: quinn::Connection) {
        while let Ok(datagram) = connection.read_datagram().await {
            let Ok(header) = Header::parse(&datagram) else {
                self.malformed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let table = self.table.load();
            match table.routes.get(&header.media_id) {
                Some(route) if route.owner == conn_id => {
                    for subscriber in &route.subscribers {
                        subscriber.offer(Packet {
                            kind: route.kind,
                            header,
                            bytes: datagram.clone(),
                        });
                    }
                }
                _ => {
                    self.not_owned.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

struct Pending {
    last_sent: Option<Instant>,
    wanted: Option<Recovery>,
}

/// Merges keyframe requests: at most one per track per [`KEYFRAME_INTERVAL`], and an
/// `Idr` beats a `Refresh` within one interval.
#[derive(Default)]
pub struct KeyframeAggregator {
    tracks: HashMap<MediaId, Pending>,
}

impl KeyframeAggregator {
    /// Records a request. Returns the request to forward now, if the track is due.
    pub fn request(&mut self, media_id: MediaId, kind: Recovery, now: Instant) -> Option<Recovery> {
        let pending = self.tracks.entry(media_id).or_insert(Pending {
            last_sent: None,
            wanted: None,
        });
        pending.wanted = pending.wanted.max(Some(kind));
        Self::take_if_due(pending, now)
    }

    /// Requests whose interval has passed since the last one was forwarded.
    pub fn due(&mut self, now: Instant) -> Vec<(MediaId, Recovery)> {
        self.tracks
            .iter_mut()
            .filter_map(|(media_id, pending)| Some((*media_id, Self::take_if_due(pending, now)?)))
            .collect()
    }

    pub fn forget(&mut self, media_id: MediaId) {
        self.tracks.remove(&media_id);
    }

    fn take_if_due(pending: &mut Pending, now: Instant) -> Option<Recovery> {
        let due = pending
            .last_sent
            .is_none_or(|t| now.duration_since(t) >= KEYFRAME_INTERVAL);
        if !due {
            return None;
        }
        let kind = pending.wanted.take()?;
        pending.last_sent = Some(now);
        Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_at_most_one_per_second_and_idr_wins() {
        let t = Instant::now();
        let mut agg = KeyframeAggregator::default();
        let id = MediaId(3);
        assert_eq!(
            agg.request(id, Recovery::Refresh, t),
            Some(Recovery::Refresh)
        );
        assert_eq!(
            agg.request(id, Recovery::Refresh, t + Duration::from_millis(10)),
            None
        );
        assert_eq!(
            agg.request(id, Recovery::Idr, t + Duration::from_millis(20)),
            None
        );
        assert_eq!(
            agg.request(id, Recovery::Refresh, t + Duration::from_millis(30)),
            None
        );
        assert!(agg.due(t + Duration::from_millis(999)).is_empty());
        assert_eq!(agg.due(t + KEYFRAME_INTERVAL), vec![(id, Recovery::Idr)]);
        assert!(agg.due(t + KEYFRAME_INTERVAL * 3).is_empty());
    }
}
