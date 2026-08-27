mod common;

use common::{SERVER_LABEL, TestServer, keystore, next_event};
use tempfile::TempDir;
use throcc_client_core::{Client, Command, Event};

#[test]
fn a_request_is_answered_over_the_control_stream() {
    let server = TestServer::start();
    let client_dir = TempDir::new().unwrap();

    let client = Client::connect(
        server.address,
        SERVER_LABEL,
        keystore(&client_dir),
        Some(server.bootstrap_invite()),
    )
    .expect("connecting");

    let mut events = client.events();
    client.command(Command::SetRoom(None)).unwrap();

    match next_event(&mut events) {
        Event::Placed(placed) => assert_eq!(placed.room, None),
        other => panic!("expected the server's answer, got {other:?}"),
    }

    client.shutdown();
}

#[test]
fn closing_the_client_reports_a_disconnect() {
    let server = TestServer::start();
    let client_dir = TempDir::new().unwrap();

    let client = Client::connect(
        server.address,
        SERVER_LABEL,
        keystore(&client_dir),
        Some(server.bootstrap_invite()),
    )
    .unwrap();
    let mut events = client.events();

    client.command(Command::Disconnect).unwrap();
    assert!(matches!(
        next_event(&mut events),
        Event::Disconnected { .. }
    ));

    client.shutdown();
}
