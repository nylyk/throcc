#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::connection::{self, Connected};
use throcc_client_core::control::Link;
use throcc_client_core::keystore::Keystore;
use throcc_crypto::identity::IdentityKey;
use throcc_crypto::pake::{ClientPake, Transcript};
use throcc_proto::framing;
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    AuthResult, ErrorCode, Event, Hello, PakeFinish, PakeStep, Request, Response, ServerHello,
    Share, Welcome,
};
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

pub async fn read_frame(recv: &mut quinn::RecvStream) -> Vec<u8> {
    let mut header = [0u8; 4];
    recv.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; framing::body_len(header).unwrap()];
    recv.read_exact(&mut body).await.unwrap();
    body
}

/// Client transport settings with stream and connection receive windows of `window` bytes.
pub fn small_window_transport(window: u32) -> quinn::TransportConfig {
    let mut transport = connection::transport_config(0);
    transport
        .stream_receive_window(window.into())
        .receive_window(window.into());
    transport
}

pub async fn connect_with_transport(addr: &str, transport: quinn::TransportConfig) -> Connected {
    let (config, verifier) = connection::client_config(None, transport).unwrap();
    connection::connect_with(addr, config, &verifier)
        .await
        .unwrap()
}

/// A member enrolled over a bare connection, so the test reads and writes its control
/// stream itself.
pub struct RawMember {
    pub connected: Connected,
    pub welcome: Welcome,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
}

pub async fn raw_enroll(
    addr: &str,
    code: &InviteCode,
    transport: quinn::TransportConfig,
) -> RawMember {
    let connected = connect_with_transport(addr, transport).await;
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let hello: ServerHello = framing::decode(&read_frame(&mut recv).await).unwrap();
    let identity = IdentityKey::generate();
    let pubkey = identity.public();
    let client_nonce = [7; 32];
    let (pake, pake_a) = ClientPake::start(&code.id, &code.secret);
    let enroll = Hello::Enroll {
        code_id: code.id.clone(),
        pubkey,
        client_nonce,
        pake_a: pake_a.clone(),
    };
    send.write_all(&framing::encode(&enroll).unwrap())
        .await
        .unwrap();
    let step: PakeStep = framing::decode(&read_frame(&mut recv).await).unwrap();
    let reply = step.unwrap();
    let transcript = Transcript {
        server_nonce: &hello.nonce,
        client_nonce: &client_nonce,
        code_id: &code.id,
        pubkey: &pubkey,
        spki_hash: &connected.fingerprint.0,
        exporter: &connected.exporter,
        pake_a: &pake_a,
        pake_b: &reply.pake_b,
    };
    let finish = PakeFinish {
        confirm_a: pake.finish(&transcript, &reply.confirm_b).unwrap(),
        signature: identity.sign(&transcript.to_bytes()),
    };
    send.write_all(&framing::encode(&finish).unwrap())
        .await
        .unwrap();
    let AuthResult::Ok(welcome) = framing::decode(&read_frame(&mut recv).await).unwrap() else {
        panic!("enrollment was refused");
    };
    RawMember {
        connected,
        welcome: *welcome,
        send,
        recv,
    }
}

/// A UDP relay in front of the server that passes the client's packets on only until the
/// server answers with anything but a Retry, so the server never completes its TLS handshake.
pub async fn stalling_relay(server: &str) -> String {
    const RETRY_HEADER: u8 = 0xf0;
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut client = None;
        let mut answered = false;
        let mut up = vec![0u8; 65536];
        let mut down = vec![0u8; 65536];
        loop {
            tokio::select! {
                Ok((n, from)) = front.recv_from(&mut up) => {
                    client = Some(from);
                    if !answered {
                        back.send(&up[..n]).await.ok();
                    }
                }
                Ok(n) = back.recv(&mut down) => {
                    answered |= n > 0 && down[0] & 0xf0 != RETRY_HEADER;
                    if let Some(client) = client {
                        front.send_to(&down[..n], client).await.ok();
                    }
                }
            }
        }
    });
    addr
}
