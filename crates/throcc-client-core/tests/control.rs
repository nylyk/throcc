use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;
use throcc_client_core::{Client, Cmd, Event, Keystore};
use throcc_server::Server;

const SERVER_LABEL: &str = "server.test";
const PATIENCE: Duration = Duration::from_secs(5);

fn next_event(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = sender.send(events.blocking_recv());
        });
        receiver
            .recv_timeout(PATIENCE)
            .expect("the client should have produced an event")
            .expect("the event channel should still be open")
    })
}

#[test]
fn a_request_is_answered_over_the_control_stream() {
    let server_dir = TempDir::new().unwrap();
    let client_dir = TempDir::new().unwrap();

    let server_runtime = tokio::runtime::Runtime::new().unwrap();
    let address: SocketAddr = {
        let _inside = server_runtime.enter();
        let server = Server::bind(server_dir.path(), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("binding the server");
        let address = server.local_addr().expect("local address");
        server_runtime.spawn(server.run());
        address
    };

    let keystore = Keystore::open(Some(client_dir.path().join("keystore.json"))).unwrap();
    let client = Client::connect(address, SERVER_LABEL, keystore).expect("connecting");

    let mut events = client.events();
    client.cmd(Cmd::SetRoom(None)).unwrap();

    match next_event(&mut events) {
        Event::Failed { message } => assert!(
            message.contains("Unimplemented"),
            "unexpected failure: {message}"
        ),
        other => panic!("expected the server's answer, got {other:?}"),
    }

    client.shutdown();
}

#[test]
fn closing_the_client_reports_a_disconnect() {
    let server_dir = TempDir::new().unwrap();
    let client_dir = TempDir::new().unwrap();

    let server_runtime = tokio::runtime::Runtime::new().unwrap();
    let address: SocketAddr = {
        let _inside = server_runtime.enter();
        let server = Server::bind(server_dir.path(), (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let address = server.local_addr().unwrap();
        server_runtime.spawn(server.run());
        address
    };

    let keystore = Keystore::open(Some(client_dir.path().join("keystore.json"))).unwrap();
    let client = Client::connect(address, SERVER_LABEL, keystore).unwrap();
    let mut events = client.events();

    client.cmd(Cmd::Disconnect).unwrap();
    assert!(matches!(
        next_event(&mut events),
        Event::Disconnected { .. }
    ));

    client.shutdown();
}
