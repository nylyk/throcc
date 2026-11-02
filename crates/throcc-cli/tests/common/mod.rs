#![allow(dead_code)]

#[path = "../../src/unit.rs"]
pub mod unit;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use tempfile::TempDir;
use throcc_client_core::account::{self, SignInError};
use throcc_client_core::call::Call;
use throcc_client_core::connection::{self, Connected};
use throcc_client_core::control::Link;
use throcc_client_core::keystore::Keystore;
use throcc_client_core::media::engine::{Kind, Received};
use throcc_crypto::identity::IdentityKey;
use throcc_crypto::pake::{ClientPake, Transcript};
use throcc_proto::framing;
use throcc_proto::ids::{MediaId, RoomId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    AuthResult, Codec, ErrorCode, Event, Hello, MediaState, PakeFinish, PakeReply, PakeStep,
    Placed, Request, Response, ServerHello, Share, ShareConfig, Tracks, Welcome,
};
use throcc_server::identity::ServerIdentity;
use throcc_server::{Config, Congestion, Server, Shared, tls};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

pub struct TestServer {
    pub addr: String,
    pub bootstrap: InviteCode,
    pub shared: Arc<Shared>,
    pub dir: TempDir,
}

pub fn bind_server(data_dir: &Path) -> Server {
    bind_server_at(data_dir, "127.0.0.1:0".parse().unwrap())
}

pub fn bind_server_at(data_dir: &Path, listen: SocketAddr) -> Server {
    Server::bind(&Config {
        data_dir: data_dir.to_owned(),
        listen,
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

/// A server on its own runtime thread, so stopping or dropping it ends everything like a
/// process exit, and blocking clients can reach it from a plain test thread.
pub struct ProcessLikeServer {
    pub addr: SocketAddr,
    pub bootstrap: Option<InviteCode>,
    stop: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
    dir: Option<TempDir>,
}

impl ProcessLikeServer {
    pub fn start(dir: &Path, listen: SocketAddr) -> Self {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop, stop_rx) = std::sync::mpsc::channel::<()>();
        let dir = dir.to_owned();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = bind_server_at(&dir, listen);
                ready_tx
                    .send((
                        server.local_addr().unwrap(),
                        server.bootstrap_invite().cloned(),
                    ))
                    .unwrap();
                tokio::spawn(server.run());
                tokio::task::spawn_blocking(move || stop_rx.recv().ok())
                    .await
                    .ok();
            });
        });
        let (addr, bootstrap) = ready_rx.recv().unwrap();
        Self {
            addr,
            bootstrap,
            stop,
            thread,
            dir: None,
        }
    }

    /// A fresh server keeping its data in a directory that goes away with it.
    pub fn start_temp() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let server = Self::start(dir.path(), "127.0.0.1:0".parse().unwrap());
        Self {
            dir: Some(dir),
            ..server
        }
    }

    pub fn stop(self) {
        self.stop.send(()).unwrap();
        self.thread.join().unwrap();
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

    pub async fn set_room(&self, room: Option<RoomId>, password: Option<&str>) -> Response {
        self.request(Request::SetRoom {
            room,
            password: password.map(str::to_owned),
        })
        .await
    }

    /// Moves to `room`, which must succeed.
    pub async fn join(&self, room: Option<RoomId>) -> Placed {
        match self.set_room(room, None).await {
            Response::Placed(placed) => placed,
            other => panic!("expected Placed, got {other:?}"),
        }
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

    pub fn disconnect(&self) {
        self.link.connected.connection.close(0u32.into(), b"");
    }

    /// Drops the current connection and signs in again at `addr`, asking for `room`.
    pub async fn reconnect(&mut self, addr: &str, room: Option<RoomId>) {
        self.disconnect();
        self.link = account::sign_in(&self.keystore, addr, room).await.unwrap();
    }
}

pub async fn enroll(server: &TestServer, code: &InviteCode) -> TestClient {
    enroll_at(&server.addr, code).await
}

pub async fn enroll_at(addr: &str, code: &InviteCode) -> TestClient {
    try_enroll_at(addr, code).await.unwrap()
}

pub async fn try_enroll_at(addr: &str, code: &InviteCode) -> Result<TestClient, SignInError> {
    let dir = tempfile::tempdir().unwrap();
    let mut keystore = Keystore::open_in(dir.path()).unwrap();
    let link = account::enroll(&mut keystore, addr, code).await?;
    Ok(TestClient {
        keystore,
        link,
        dir,
    })
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
            .request(Request::CreateRoom {
                name: name.into(),
                password: None,
            })
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
    alice.join(Some(room), None).await.unwrap();
    bob.join(Some(room), None).await.unwrap();
    (alice, bob, room)
}

/// Waits until `viewer` knows `peer`'s tracks, then returns them.
pub async fn peer_tracks(viewer: &Call, peer: &Call) -> Tracks {
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
        .clone()
}

pub async fn first_share(viewer: &Call, peer: &Call) -> Share {
    peer_tracks(viewer, peer).await.shares[0]
}

/// Subscribes `viewer` to `video` and tells the server it is watching.
pub async fn watch(viewer: &Call, video: MediaId, capacity: usize) -> mpsc::Receiver<Received> {
    let units = viewer.engine.subscribe(video, Kind::Video, capacity);
    assert_eq!(
        viewer
            .request(Request::SetWatching(vec![video]))
            .await
            .unwrap(),
        Response::Ok
    );
    units
}

pub const fn share_1080p60(codec: Codec, kbps: u32) -> ShareConfig {
    ShareConfig {
        audio: false,
        kbps,
        codec,
        width: 1920,
        height: 1080,
        fps: 60,
    }
}

/// Sharing `config` with the mic off.
pub fn sharing(config: ShareConfig) -> MediaState {
    MediaState {
        mic: false,
        deafened: false,
        share: Some(config),
    }
}

/// A bare QUIC endpoint that presents the server identity kept in `identity_dir`.
pub fn bare_endpoint(identity_dir: &Path, transport: quinn::TransportConfig) -> quinn::Endpoint {
    let identity = ServerIdentity::load_or_create(identity_dir).unwrap();
    let config = tls::server_config(&identity, transport).unwrap();
    quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap()
}

/// Which way a relayed packet travels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Server,
    Client,
}

/// A UDP relay in front of `server` for one client. Returns its address; it passes on each
/// packet that `forward` accepts.
pub async fn relay(
    server: &str,
    mut forward: impl FnMut(Toward, &[u8]) -> bool + Send + 'static,
) -> String {
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut client = None;
        let mut up = vec![0u8; 65536];
        let mut down = vec![0u8; 65536];
        loop {
            tokio::select! {
                Ok((n, from)) = front.recv_from(&mut up) => {
                    client = Some(from);
                    if forward(Toward::Server, &up[..n]) {
                        back.send(&up[..n]).await.ok();
                    }
                }
                Ok(n) = back.recv(&mut down) => {
                    if forward(Toward::Client, &down[..n])
                        && let Some(client) = client
                    {
                        front.send_to(&down[..n], client).await.ok();
                    }
                }
            }
        }
    });
    addr
}

/// A relay that, while `lossy` is set, drops one in `nth` packets on the way to the client,
/// at random.
pub struct LossyRelay {
    pub addr: String,
    pub lossy: Arc<AtomicBool>,
    pub dropped: Arc<AtomicU64>,
}

pub async fn lossy_relay(server: &str, nth: u64) -> LossyRelay {
    let lossy = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let (relay_lossy, relay_dropped) = (lossy.clone(), dropped.clone());
    let mut random = 0x9e37_79b9_7f4a_7c15u64;
    let addr = relay(server, move |toward, _| {
        if toward == Toward::Server {
            return true;
        }
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let drop = relay_lossy.load(Ordering::Relaxed) && random.is_multiple_of(nth);
        if drop {
            relay_dropped.fetch_add(1, Ordering::Relaxed);
        }
        !drop
    })
    .await;
    LossyRelay {
        addr,
        lossy,
        dropped,
    }
}

/// A relay that passes the client's packets on only until the server answers with anything
/// but a Retry, so the server never completes its TLS handshake.
pub async fn stalling_relay(server: &str) -> String {
    const RETRY_HEADER: u8 = 0xf0;
    let mut answered = false;
    relay(server, move |toward, packet| match toward {
        Toward::Server => !answered,
        Toward::Client => {
            answered |= packet.first().is_some_and(|b| b & 0xf0 != RETRY_HEADER);
            true
        }
    })
    .await
}

pub async fn read_message<T: DeserializeOwned>(recv: &mut quinn::RecvStream) -> T {
    let mut header = [0u8; framing::LEN_BYTES];
    recv.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; framing::body_len(header).unwrap()];
    recv.read_exact(&mut body).await.unwrap();
    framing::decode(&body).unwrap()
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
    raw_enroll_start(addr, code, transport).await.finish().await
}

/// An enrollment over a bare connection that has had the server's PAKE reply but not yet
/// sent its own finish.
pub struct PendingEnroll {
    connected: Connected,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    hello: ServerHello,
    code_id: String,
    identity: IdentityKey,
    client_nonce: [u8; 32],
    pake: ClientPake,
    pake_a: Vec<u8>,
    reply: PakeReply,
}

pub async fn raw_enroll_start(
    addr: &str,
    code: &InviteCode,
    transport: quinn::TransportConfig,
) -> PendingEnroll {
    let connected = connect_with_transport(addr, transport).await;
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let hello: ServerHello = read_message(&mut recv).await;
    let identity = IdentityKey::generate();
    let client_nonce = [7; 32];
    let (pake, pake_a) = ClientPake::start(&code.id, &code.secret);
    let enroll = Hello::Enroll {
        code_id: code.id.clone(),
        pubkey: identity.public(),
        client_nonce,
        pake_a: pake_a.clone(),
    };
    send.write_all(&framing::encode(&enroll).unwrap())
        .await
        .unwrap();
    let step: PakeStep = read_message(&mut recv).await;
    PendingEnroll {
        connected,
        send,
        recv,
        hello,
        code_id: code.id.clone(),
        identity,
        client_nonce,
        pake,
        pake_a,
        reply: step.unwrap(),
    }
}

impl PendingEnroll {
    /// Sends the PAKE finish; the server must let the member in.
    pub async fn finish(mut self) -> RawMember {
        let pubkey = self.identity.public();
        let transcript = Transcript {
            server_nonce: &self.hello.nonce,
            client_nonce: &self.client_nonce,
            code_id: &self.code_id,
            pubkey: &pubkey,
            spki_hash: &self.connected.fingerprint.0,
            exporter: &self.connected.exporter,
            pake_a: &self.pake_a,
            pake_b: &self.reply.pake_b,
        };
        let finish = PakeFinish {
            confirm_a: self
                .pake
                .finish(&transcript, &self.reply.confirm_b)
                .unwrap(),
            signature: self.identity.sign(&transcript.to_bytes()),
        };
        self.send
            .write_all(&framing::encode(&finish).unwrap())
            .await
            .unwrap();
        match read_message(&mut self.recv).await {
            AuthResult::Ok(welcome) => RawMember {
                connected: self.connected,
                welcome: *welcome,
                send: self.send,
                recv: self.recv,
            },
            refused => panic!("enrollment was refused: {refused:?}"),
        }
    }
}
