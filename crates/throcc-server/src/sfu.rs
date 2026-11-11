use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use bytes::Bytes;
use throcc_proto::frame::{DATAGRAM_PAYLOAD_FLOOR, Header};
use throcc_proto::ids::MediaId;
use throcc_proto::messages::Recovery;
use throcc_proto::transport::close;
use tracing::warn;

use crate::egress::{EgressHandle, Packet, TrackKind};
use crate::limits::TokenBucket;

/// Keyframe requests for one track are forwarded at most this often.
pub const KEYFRAME_INTERVAL: Duration = Duration::from_secs(1);
/// The most a connection may send in datagrams, voice and share together: 25 Mbps.
pub const MAX_INGRESS_BYTES_PER_SECOND: f64 = 25_000_000.0 / 8.0;
const INGRESS_BURST_BYTES: f64 = MAX_INGRESS_BYTES_PER_SECOND;
/// Datagrams a connection may send per second, over all its tracks. 25 Mbps in full-size
/// datagrams is about 2,600 a second; this bounds the per-packet work of tiny ones.
pub const MAX_INGRESS_PACKETS_PER_SECOND: f64 = 6000.0;
const INGRESS_BURST_PACKETS: f64 = 2.0 * MAX_INGRESS_PACKETS_PER_SECOND;
/// One audio track's allowance. A sender makes 50 packets a second, a few datagrams each at
/// most, and Opus tops out at 510 kbps, which with headers split over two datagrams is
/// about 65 KB/s. The burst covers a flush of a client's whole 64 KiB datagram send buffer
/// after a stall, even in the smallest datagrams.
pub const AUDIO_PACKETS_PER_SECOND: f64 = 150.0;
pub const AUDIO_BURST_PACKETS: f64 = 2500.0;
pub const AUDIO_BYTES_PER_SECOND: f64 = 80.0 * 1024.0;
pub const AUDIO_BURST_BYTES: f64 = 256.0 * 1024.0;
/// How often a connection's policed datagrams are logged.
const DROP_REPORT_INTERVAL: Duration = Duration::from_secs(60);
/// How often allowances of tracks that went quiet are forgotten.
const ALLOWANCE_SWEEP: Duration = Duration::from_secs(10);

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
}

impl Sfu {
    /// Forwards datagrams from one connection until it closes. Never reads past the header.
    ///
    /// A connection that sends more bytes than [`MAX_INGRESS_BYTES_PER_SECOND`] is closed.
    /// Datagrams over [`MAX_INGRESS_PACKETS_PER_SECOND`], over an audio track's
    /// [`AudioAllowance`] or larger than [`DATAGRAM_PAYLOAD_FLOOR`] are dropped and counted: admission policing of a sender that
    /// doesn't conform, which a well-behaved client never reaches. Egress itself never
    /// drops audio by policy.
    pub async fn ingress(self: Arc<Self>, conn_id: u64, connection: quinn::Connection) {
        let start = Instant::now();
        let mut bytes = TokenBucket::full(INGRESS_BURST_BYTES, MAX_INGRESS_BYTES_PER_SECOND, start);
        let mut packets =
            TokenBucket::full(INGRESS_BURST_PACKETS, MAX_INGRESS_PACKETS_PER_SECOND, start);
        let mut audio = AudioPolicer::default();
        let mut policed = DropReport::new(start);
        while let Ok(datagram) = connection.read_datagram().await {
            let now = Instant::now();
            if !bytes.take(datagram.len() as f64, now) {
                warn!(conn_id, "closing a connection that sent media too fast");
                connection.close(close::PROTOCOL_ERROR.into(), b"sent media too fast");
                break;
            }
            // No client seals a larger one, and a receiver whose path carries no more than
            // the floor could not be sent it.
            if !packets.take(1.0, now) || datagram.len() > DATAGRAM_PAYLOAD_FLOOR {
                policed.count(conn_id, now);
                continue;
            }
            let Ok(header) = Header::parse(&datagram) else {
                continue;
            };
            let table = self.table.load();
            let Some(route) = table.routes.get(&header.media_id) else {
                continue;
            };
            if route.owner != conn_id {
                continue;
            }
            if route.kind == TrackKind::Audio && !audio.admit(header.media_id, datagram.len(), now)
            {
                policed.count(conn_id, now);
                continue;
            }
            // A datagram is a slice of the buffer its whole UDP receive landed in. A copy
            // keeps only its own bytes alive in the receivers' queues, which budget by length.
            let datagram = Bytes::copy_from_slice(&datagram);
            for subscriber in &route.subscribers {
                subscriber.offer(Packet {
                    kind: route.kind,
                    header,
                    bytes: datagram.clone(),
                });
            }
        }
        policed.finish(conn_id);
    }
}

/// One audio track's packet and byte allowance at ingress.
pub struct AudioAllowance {
    packets: TokenBucket,
    bytes: TokenBucket,
}

impl AudioAllowance {
    fn full(now: Instant) -> Self {
        Self {
            packets: TokenBucket::full(AUDIO_BURST_PACKETS, AUDIO_PACKETS_PER_SECOND, now),
            bytes: TokenBucket::full(AUDIO_BURST_BYTES, AUDIO_BYTES_PER_SECOND, now),
        }
    }

    /// Takes one packet of `len` bytes if both buckets hold enough.
    fn admit(&mut self, len: usize, now: Instant) -> bool {
        let len = len as f64;
        if !(self.packets.holds(1.0, now) && self.bytes.holds(len, now)) {
            return false;
        }
        self.packets.take(1.0, now) && self.bytes.take(len, now)
    }

    fn is_full(&mut self, now: Instant) -> bool {
        self.packets.is_full(now) && self.bytes.is_full(now)
    }
}

/// The allowances of one connection's audio tracks. Only tracks the connection owns get
/// here, and full allowances are forgotten, so it stays as small as the tracks in use.
#[derive(Default)]
pub struct AudioPolicer {
    tracks: HashMap<MediaId, AudioAllowance>,
    next_sweep: Option<Instant>,
}

impl AudioPolicer {
    pub fn admit(&mut self, media_id: MediaId, len: usize, now: Instant) -> bool {
        if self.next_sweep.is_none_or(|t| now >= t) {
            self.tracks.retain(|_, allowance| !allowance.is_full(now));
            self.next_sweep = Some(now + ALLOWANCE_SWEEP);
        }
        self.tracks
            .entry(media_id)
            .or_insert_with(|| AudioAllowance::full(now))
            .admit(len, now)
    }
}

/// Counts a connection's policed datagrams and logs them at most once per
/// [`DROP_REPORT_INTERVAL`].
struct DropReport {
    unreported: u64,
    next_report: Instant,
}

impl DropReport {
    fn new(now: Instant) -> Self {
        Self {
            unreported: 0,
            next_report: now,
        }
    }

    fn count(&mut self, conn_id: u64, now: Instant) {
        self.unreported += 1;
        if now >= self.next_report {
            self.finish(conn_id);
            self.next_report = now + DROP_REPORT_INTERVAL;
        }
    }

    fn finish(&mut self, conn_id: u64) {
        if self.unreported > 0 {
            warn!(
                conn_id,
                dropped = self.unreported,
                "dropped datagrams over the sender's rate"
            );
            self.unreported = 0;
        }
    }
}

#[derive(Default)]
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
        let pending = self.tracks.entry(media_id).or_default();
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

    /// The smallest datagram a sender could make: the header and an AEAD tag.
    const TINY: usize = 13 + 16;

    #[test]
    fn a_well_behaved_audio_sender_is_never_policed() {
        let start = Instant::now();
        let mut policer = AudioPolicer::default();
        let mic = MediaId(1);
        let mut now = start;
        // A stall, then a flush of a whole 64 KiB send buffer of the smallest datagrams.
        for _ in 0..64 * 1024 / TINY {
            assert!(policer.admit(mic, TINY, now));
        }
        // Then ten minutes at Opus's ceiling, 510 kbps in 20 ms packets, split in two.
        for _ in 0..10 * 60 * 50 {
            now += Duration::from_millis(20);
            for _ in 0..2 {
                assert!(policer.admit(mic, 1275 / 2 + TINY, now));
            }
        }
    }

    #[test]
    fn a_flooding_audio_track_is_held_to_its_allowance_alone() {
        let start = Instant::now();
        let mut policer = AudioPolicer::default();
        let (flood, mic) = (MediaId(1), MediaId(2));
        let mut admitted = 0;
        for i in 0..100_000u32 {
            let now = start + Duration::from_micros(u64::from(i) * 10);
            admitted += usize::from(policer.admit(flood, TINY, now));
            if i % 2000 == 0 {
                assert!(policer.admit(mic, 200, now));
            }
        }
        let allowed = AUDIO_BURST_PACKETS + AUDIO_PACKETS_PER_SECOND;
        assert!(admitted as f64 <= allowed, "{admitted}");
        let mut bytes = AudioPolicer::default();
        let admitted = (0..1000)
            .filter(|_| bytes.admit(flood, 1200, start))
            .count();
        assert_eq!(admitted, (AUDIO_BURST_BYTES / 1200.0) as usize);
    }

    #[test]
    fn quiet_tracks_are_forgotten() {
        let start = Instant::now();
        let mut policer = AudioPolicer::default();
        for id in 0..100 {
            policer.admit(MediaId(id), 100, start);
        }
        policer.admit(MediaId(0), 100, start + Duration::from_secs(60));
        assert_eq!(policer.tracks.len(), 1);
    }
}
