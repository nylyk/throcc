mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use throcc_client_core::connection::{self, ConnectError};
use throcc_proto::frame::DATAGRAM_PAYLOAD_FLOOR;
use throcc_proto::transport::{DatagramError, close};
use throcc_server::{CONNECTIONS, Congestion, tls};

use common::{bare_endpoint, bind_server};

fn start_server(data_dir: &Path) -> (SocketAddr, throcc_proto::Fingerprint) {
    let server = bind_server(data_dir);
    let addr = server.local_addr().unwrap();
    let fingerprint = server.fingerprint();
    tokio::spawn(server.run());
    (addr, fingerprint)
}

#[tokio::test]
async fn client_connects_and_pins() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, fingerprint) = start_server(dir.path());

    let first = connection::connect(&addr.to_string(), None).await.unwrap();
    assert_eq!(first.fingerprint, fingerprint);

    let pinned = connection::connect(&addr.to_string(), Some(fingerprint))
        .await
        .unwrap();
    assert_eq!(pinned.fingerprint, fingerprint);
    assert_ne!(pinned.exporter, first.exporter);
}

#[tokio::test]
async fn new_server_key_fails_with_both_fingerprints() {
    let dir = tempfile::tempdir().unwrap();
    let (_, old) = start_server(dir.path());
    std::fs::remove_file(dir.path().join("server_key")).unwrap();
    let (addr, new) = start_server(dir.path());

    let err = connection::connect(&addr.to_string(), Some(old))
        .await
        .err()
        .unwrap();
    match &err {
        ConnectError::PinMismatch { expected, actual } => {
            assert_eq!((*expected, *actual), (old, new));
        }
        other => panic!("expected a pin mismatch, got {other}"),
    }
    let message = err.to_string();
    assert!(message.contains(&old.to_string()) && message.contains(&new.to_string()));
}

#[tokio::test]
async fn server_refuses_client_without_datagrams() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _) = start_server(dir.path());

    let mut transport = connection::transport_config(0);
    transport.datagram_receive_buffer_size(None);
    let (config, verifier) = connection::client_config(None, transport).unwrap();
    let connected = connection::connect_with(&addr.to_string(), config, &verifier)
        .await
        .unwrap();

    match connected.connection.closed().await {
        quinn::ConnectionError::ApplicationClosed(close_frame) => {
            assert_eq!(close_frame.error_code, close::DATAGRAMS_REQUIRED.into());
            let reason = String::from_utf8_lossy(&close_frame.reason);
            assert!(
                reason.contains("does not support QUIC datagrams"),
                "{reason}"
            );
        }
        other => panic!("expected an application close, got {other}"),
    }
}

#[tokio::test]
async fn client_refuses_server_without_datagrams() {
    let dir = tempfile::tempdir().unwrap();
    let mut transport = tls::transport_config(Congestion::Cubic).unwrap();
    transport.datagram_receive_buffer_size(None);
    let endpoint = bare_endpoint(dir.path(), transport);
    let addr = endpoint.local_addr().unwrap();
    tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        connection.closed().await;
    });

    let err = connection::connect(&addr.to_string(), None)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, ConnectError::Datagrams(DatagramError::Unsupported)),
        "{err}"
    );
}

#[tokio::test]
async fn payload_floor_matches_quinn_at_1200_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _) = start_server(dir.path());

    let mut transport = connection::transport_config(0);
    transport
        .initial_mtu(1200)
        .min_mtu(1200)
        .mtu_discovery_config(None);
    let (config, verifier) = connection::client_config(None, transport).unwrap();
    let connected = connection::connect_with(&addr.to_string(), config, &verifier)
        .await
        .unwrap();

    assert_eq!(
        connected.connection.max_datagram_size(),
        Some(DATAGRAM_PAYLOAD_FLOOR)
    );
}

#[tokio::test]
async fn clients_cannot_open_streams() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _) = start_server(dir.path());
    let connected = connection::connect(&addr.to_string(), None).await.unwrap();
    let wait = Duration::from_millis(300);
    assert!(
        tokio::time::timeout(wait, connected.connection.open_bi())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(wait, connected.connection.open_uni())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_connection_that_never_says_hello_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _) = start_server(dir.path());
    let connected = connection::connect(&addr.to_string(), None).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(8), connected.connection.closed())
        .await
        .unwrap();
    match closed {
        quinn::ConnectionError::ApplicationClosed(frame) => {
            assert_eq!(frame.error_code, close::PROTOCOL_ERROR.into());
        }
        other => panic!("expected an application close, got {other}"),
    }
}

#[tokio::test]
async fn one_source_holds_a_bounded_number_of_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _) = start_server(dir.path());
    let mut held = Vec::new();
    for _ in 0..CONNECTIONS.per_source {
        held.push(connection::connect(&addr.to_string(), None).await.unwrap());
    }
    assert!(connection::connect(&addr.to_string(), None).await.is_err());
}
