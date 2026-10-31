#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::control::Link;
use throcc_client_core::keystore::Keystore;
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{ErrorCode, Event, Request, Response, Share};
use throcc_server::identity::ServerIdentity;
use throcc_server::{Config, Congestion, Server, Shared, tls};
use tokio::net::UdpSocket;

pub struct TestServer {
    pub addr: String,
    pub bootstrap: InviteCode,
    pub shared: Arc<Shared>,
    pub dir: TempDir,
}

pub fn bind_server(data_dir: &Path) -> Server {
    Server::bind(&Config {
        data_dir: data_dir.to_owned(),
        listen: "127.0.0.1:0".parse().unwrap(),
        congestion: Congestion::Cubic,
    })
    .unwrap()
}

pub fn start_server() -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let server = bind_server(dir.path());
    let addr = server.local_addr().unwrap().to_string();
    let bootstrap = server.bootstrap_invite().unwrap().clone();
    let shared = server.shared();
    tokio::spawn(server.run());
    TestServer {
        addr,
        bootstrap,
        shared,
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
    enroll_at(&server.addr, code).await
}

pub async fn enroll_at(addr: &str, code: &InviteCode) -> TestClient {
    let dir = tempfile::tempdir().unwrap();
    let mut keystore = Keystore::open_in(dir.path()).unwrap();
    let link = account::enroll(&mut keystore, addr, code).await.unwrap();
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
    invited_at(&server.addr, by).await
}

pub async fn invited_at(addr: &str, by: &TestClient) -> TestClient {
    let code = invite(by).await;
    enroll_at(addr, &code).await
}

#[track_caller]
pub fn assert_error(response: &Response, expected: ErrorCode) {
    assert!(
        matches!(response, Response::Err { code, .. } if *code == expected),
        "{response:?}"
    );
}

pub async fn create_room(client: &mut TestClient, name: &str) -> RoomId {
    assert_eq!(
        client
            .request(Request::CreateRoom { name: name.into() })
            .await,
        Response::Ok
    );
    client
        .event(|e| match e {
            Event::RoomCreated(room) if room.name == name => Some(room.id),
            _ => None,
        })
        .await
}

pub async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn room_with_two(server: &TestServer) -> (Call, Call, RoomId) {
    room_with_two_at(server, &server.addr).await
}

/// Alice talks to the server directly; Bob reaches it at `bob_addr`.
pub async fn room_with_two_at(server: &TestServer, bob_addr: &str) -> (Call, Call, RoomId) {
    let mut alice = admin(server).await;
    let room = create_room(&mut alice, "General").await;
    let code = invite(&alice).await;
    let bob = enroll_at(bob_addr, &code).await;
    let alice = Call::start(alice.link, &alice.keystore).unwrap();
    let bob = Call::start(bob.link, &bob.keystore).unwrap();
    alice.join(Some(room)).await.unwrap();
    bob.join(Some(room)).await.unwrap();
    (alice, bob, room)
}

/// Waits until `viewer` knows `peer`'s tracks, then returns `peer`'s first share.
pub async fn first_share(viewer: &Call, peer: &Call) -> Share {
    wait_for("the viewer to learn the peer's tracks", || {
        viewer.engine.keys().keys.peer_tracks(peer.me()).is_some()
    })
    .await;
    viewer
        .engine
        .keys()
        .keys
        .peer_tracks(peer.me())
        .unwrap()
        .shares[0]
}

/// A bare QUIC endpoint that presents the server identity kept in `identity_dir`.
pub fn bare_endpoint(identity_dir: &Path, transport: quinn::TransportConfig) -> quinn::Endpoint {
    let identity = ServerIdentity::load_or_create(identity_dir).unwrap();
    let config = tls::server_config(&identity, transport).unwrap();
    quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap()
}

/// A UDP relay in front of the server for one client. While `lossy` is set, it drops
/// one in `nth` packets on the way to the client, at random.
pub struct LossyRelay {
    pub addr: String,
    pub lossy: Arc<AtomicBool>,
    pub dropped: Arc<AtomicU64>,
}

pub async fn lossy_relay(server: &str, nth: u64) -> LossyRelay {
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap().to_string();
    let lossy = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let (relay_lossy, relay_dropped) = (lossy.clone(), dropped.clone());
    tokio::spawn(async move {
        let mut client = None;
        let mut up = vec![0u8; 65536];
        let mut down = vec![0u8; 65536];
        let mut random = 0x9e37_79b9_7f4a_7c15u64;
        loop {
            tokio::select! {
                Ok((n, from)) = front.recv_from(&mut up) => {
                    client = Some(from);
                    back.send(&up[..n]).await.ok();
                }
                Ok(n) = back.recv(&mut down) => {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    if relay_lossy.load(Ordering::Relaxed) && random.is_multiple_of(nth) {
                        relay_dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if let Some(client) = client {
                        front.send_to(&down[..n], client).await.ok();
                    }
                }
            }
        }
    });
    LossyRelay {
        addr,
        lossy,
        dropped,
    }
}
