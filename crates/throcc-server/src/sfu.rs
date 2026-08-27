use bytes::Bytes;
use quinn::{Connection, SendDatagramError};
use throcc_proto::{FrameHeader, Tracks};
use tokio::sync::watch;

/// One connection's media picture: the ids it may send on, and the connections
/// its datagrams go to. Rebuilt on every membership change.
#[derive(Default, Clone)]
pub struct Route {
    pub tracks: Option<Tracks>,
    pub subscribers: Vec<Connection>,
}

pub async fn forward(connection: Connection, route: watch::Receiver<Route>) {
    while let Ok(datagram) = connection.read_datagram().await {
        let Ok(header) = FrameHeader::decode(&datagram) else {
            tracing::debug!(bytes = datagram.len(), "dropping a truncated datagram");
            continue;
        };

        let current = route.borrow();
        let owned = current
            .tracks
            .as_ref()
            .is_some_and(|tracks| tracks.owns(header.media_id));
        if !owned {
            tracing::debug!(media_id = %header.media_id, "dropping a datagram the sender does not own");
            continue;
        }

        for subscriber in &current.subscribers {
            deliver(subscriber, datagram.clone());
        }
    }
}

fn deliver(subscriber: &Connection, datagram: Bytes) {
    match subscriber.send_datagram(datagram) {
        Ok(()) => {}
        Err(SendDatagramError::ConnectionLost(_)) => {}
        Err(e) => tracing::debug!(error = %e, "could not forward a datagram"),
    }
}
