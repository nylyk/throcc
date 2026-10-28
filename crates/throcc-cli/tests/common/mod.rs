#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use throcc_client_core::account;
use throcc_client_core::control::Link;
use throcc_client_core::keystore::Keystore;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Event, Request, Response};
use throcc_server::{Config, Congestion, Server, Shared};

pub struct TestServer {
    pub addr: String,
    pub bootstrap: InviteCode,
    pub shared: Option<Arc<Shared>>,
    pub dir: TempDir,
}

impl TestServer {
    pub fn shared(&self) -> &Shared {
        self.shared.as_deref().expect("this server runs elsewhere")
    }
}

pub fn start_server() -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::bind(&Config {
        data_dir: dir.path().to_owned(),
        listen: "127.0.0.1:0".parse().unwrap(),
        congestion: Congestion::Cubic,
    })
    .unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let bootstrap = server.bootstrap_invite().unwrap().clone();
    let shared = server.shared();
    tokio::spawn(server.run());
    TestServer {
        addr,
        bootstrap,
        shared: Some(shared),
        dir,
    }
}

pub struct TestClient {
    pub keystore: Keystore,
    pub link: Link,
    pub dir: TempDir,
}

impl TestClient {
    pub async fn request(&self, req: Request) -> Response {
        self.link.requester.request(req).await.unwrap()
    }

    /// Waits for the next event that `pick` accepts, skipping others.
    pub async fn event<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = self.link.events.recv().await.expect("event stream ended");
                if let Some(found) = pick(&event) {
                    return found;
                }
            }
        })
        .await
        .expect("timed out waiting for an event")
    }
}

pub async fn enroll(server: &TestServer, code: &InviteCode) -> TestClient {
    let dir = tempfile::tempdir().unwrap();
    let mut keystore = Keystore::open_in(dir.path()).unwrap();
    let link = account::enroll(&mut keystore, &server.addr, code)
        .await
        .unwrap();
    TestClient {
        keystore,
        link,
        dir,
    }
}

pub async fn admin(server: &TestServer) -> TestClient {
    enroll(server, &server.bootstrap).await
}

pub async fn invite(by: &TestClient) -> InviteCode {
    match by.request(Request::CreateInvite).await {
        Response::Invite { code, .. } => code.parse().unwrap(),
        other => panic!("expected an invite, got {other:?}"),
    }
}

pub async fn invited(server: &TestServer, by: &TestClient) -> TestClient {
    let code = invite(by).await;
    enroll(server, &code).await
}
