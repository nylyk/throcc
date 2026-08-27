use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bytes::{BufMut as _, Bytes, BytesMut};
use quinn::Connection;
use throcc_proto::{Epoch, FrameHeader, HEADER_BYTES, MediaId, frame};
use tokio::sync::mpsc;

use crate::media::fragment;

pub const QUEUE_DEPTH: usize = 64;

/// The one place datagrams are handed to quinn. `send_datagram` returning `Ok` is
/// not evidence of transmission, so what is counted here is the queue refusing.
pub async fn drain(connection: Connection, mut queue: mpsc::Receiver<Bytes>) {
    while let Some(datagram) = queue.recv().await {
        if let Err(e) = connection.send_datagram(datagram) {
            tracing::debug!(error = %e, "could not queue a datagram");
        }
    }
}

pub struct MediaSender {
    datagrams: mpsc::Sender<Bytes>,
    epoch: AtomicU32,
    next_seq: Mutex<HashMap<MediaId, u32>>,
    dropped_units: AtomicU64,
}

impl MediaSender {
    pub fn new(datagrams: mpsc::Sender<Bytes>, epoch: Epoch) -> Self {
        Self {
            datagrams,
            epoch: AtomicU32::new(epoch.0),
            next_seq: Mutex::new(HashMap::new()),
            dropped_units: AtomicU64::new(0),
        }
    }

    /// Senders stamp the latest epoch they have seen, and receivers ignore it.
    pub fn observe(&self, epoch: Epoch) {
        self.epoch.fetch_max(epoch.0, Ordering::Relaxed);
    }

    /// One access unit, fragmented and queued. A queue without room for all of it
    /// drops all of it, since half a frame is not decodable.
    pub fn send(&self, media_id: MediaId, unit: &[u8], timestamp: Option<u32>, keyframe: bool) {
        let payloads = fragment::fragment(unit, timestamp);
        if self.datagrams.capacity() < payloads.len() {
            self.dropped_units.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(%media_id, "dropping a unit the send queue has no room for");
            return;
        }

        let epoch = Epoch(self.epoch.load(Ordering::Relaxed));
        let flags = if keyframe { frame::KEYFRAME } else { 0 };
        for payload in payloads {
            let seq = self.take_seq(media_id);
            let header = FrameHeader {
                media_id,
                epoch,
                seq,
                flags,
            };

            let mut datagram = BytesMut::with_capacity(HEADER_BYTES + payload.len());
            datagram.put_slice(&header.encode());
            datagram.put_slice(&payload);
            if self.datagrams.try_send(datagram.freeze()).is_err() {
                self.dropped_units.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(%media_id, seq, "the send queue refused a fragment");
                return;
            }
        }
    }

    pub fn dropped_units(&self) -> u64 {
        self.dropped_units.load(Ordering::Relaxed)
    }

    fn take_seq(&self, media_id: MediaId) -> u32 {
        let mut counters = self.next_seq.lock().expect("sequence mutex poisoned");
        let next = counters.entry(media_id).or_insert(0);
        let seq = *next;
        *next = seq
            .checked_add(1)
            .expect("a media id has run out of sequence numbers");
        seq
    }
}
