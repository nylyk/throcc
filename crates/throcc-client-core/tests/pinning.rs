use std::net::{Ipv4Addr, SocketAddr};

use tempfile::TempDir;
use throcc_client_core::{Connector, Error, Keystore};
use throcc_server::Server;

const SERVER_LABEL: &str = "server.test";

fn spawn_server(data_dir: &TempDir) -> (SocketAddr, throcc_proto::Fingerprint) {
    let any_loopback_port: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    let server = Server::bind(data_dir.path(), any_loopback_port).expect("binding the server");
    let address = server.local_addr().expect("local address");
    let fingerprint = server.fingerprint();
    tokio::spawn(async move {
        let _ = server.run().await;
    });
    (address, fingerprint)
}

#[tokio::test]
async fn pins_on_first_use_and_refuses_a_changed_key() {
    let server_dir = TempDir::new().unwrap();
    let client_dir = TempDir::new().unwrap();
    let keystore_path = client_dir.path().join("keystore.json");

    let (address, expected) = spawn_server(&server_dir);
    let mut connector = Connector::new(Keystore::open(Some(keystore_path)).unwrap()).unwrap();

    let connection = connector
        .connect(address, SERVER_LABEL)
        .await
        .expect("first connection should succeed");
    // The client's socket is dual-stack, so a v4 peer reports v4-mapped.
    let remote = connection.remote_address();
    assert_eq!(
        SocketAddr::new(remote.ip().to_canonical(), remote.port()),
        address
    );
    assert_eq!(
        connector.keystore().pinned(SERVER_LABEL),
        Some(expected),
        "a successful first connection must store the pin"
    );
    connection.close(0u32.into(), b"done");

    std::fs::remove_file(server_dir.path().join(throcc_server::identity::KEY_FILE)).unwrap();
    let (new_address, new_fingerprint) = spawn_server(&server_dir);
    assert_ne!(
        new_fingerprint, expected,
        "a regenerated identity must not hash to the old one"
    );

    let err = connector
        .connect(new_address, SERVER_LABEL)
        .await
        .expect_err("a changed server key must be refused");

    match err {
        Error::PinMismatch {
            server,
            pinned,
            presented,
        } => {
            assert_eq!(server, SERVER_LABEL);
            assert_eq!(pinned, expected.to_string());
            assert_eq!(presented, new_fingerprint.to_string());
        }
        other => panic!("expected a pin mismatch, got {other:?}"),
    }

    assert_eq!(
        connector.keystore().pinned(SERVER_LABEL),
        Some(expected),
        "a refused connection must not overwrite the stored pin"
    );
}

#[tokio::test]
async fn a_pin_survives_the_server_moving() {
    let server_dir = TempDir::new().unwrap();
    let client_dir = TempDir::new().unwrap();
    let (address, expected) = spawn_server(&server_dir);

    let mut connector =
        Connector::new(Keystore::open(Some(client_dir.path().join("keystore.json"))).unwrap())
            .unwrap();
    connector.connect(address, SERVER_LABEL).await.unwrap();

    let (moved, _) = spawn_server(&server_dir);
    assert_ne!(moved, address);
    connector
        .connect(moved, SERVER_LABEL)
        .await
        .expect("a server that changed port is still the same server");
    assert_eq!(connector.keystore().pinned(SERVER_LABEL), Some(expected));
}
