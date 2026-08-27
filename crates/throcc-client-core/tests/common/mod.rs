use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tempfile::TempDir;
use throcc_client_core::{Event, Keystore};
use throcc_server::Server;
use tokio::runtime::Runtime;

pub const SERVER_LABEL: &str = "server.test";
const PATIENCE: Duration = Duration::from_secs(5);

pub struct TestServer {
    pub address: SocketAddr,
    bootstrap_invite: String,
    _data_dir: TempDir,
    _runtime: Runtime,
}

impl TestServer {
    pub fn start() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let data_dir = TempDir::new().unwrap();
        let runtime = Runtime::new().unwrap();
        let _inside = runtime.enter();
        let server =
            Server::bind(data_dir.path(), (Ipv4Addr::LOCALHOST, 0).into()).expect("binding");
        let address = server.local_addr().expect("local address");
        let bootstrap_invite = server
            .bootstrap_invite()
            .expect("a fresh server should mint a bootstrap invite")
            .to_string();
        runtime.spawn(server.run());

        drop(_inside);
        Self {
            address,
            bootstrap_invite,
            _data_dir: data_dir,
            _runtime: runtime,
        }
    }

    pub fn bootstrap_invite(&self) -> String {
        self.bootstrap_invite.clone()
    }
}

pub fn keystore(directory: &TempDir) -> Keystore {
    Keystore::open(Some(directory.path().join("keystore.json"))).unwrap()
}

pub fn next_event(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    let runtime = Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(PATIENCE, events.recv())
            .await
            .expect("the client should have produced an event")
            .expect("the event channel should still be open")
    })
}
