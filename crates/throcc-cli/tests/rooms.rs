mod common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use throcc_client_core::account;
use throcc_proto::ids::{Epoch, RoomId, UserId};
use throcc_proto::messages::{
    ErrorCode, Event, MAX_AVATAR_BYTES, Placed, Presence, Request, Response, Welcome,
};
use throcc_server::{Config, Congestion, Server};
use tokio::net::UdpSocket;

use common::*;

async fn join(client: &TestClient, room: Option<RoomId>) -> Placed {
    match client.request(Request::SetRoom(room)).await {
        Response::Placed(placed) => placed,
        other => panic!("expected Placed, got {other:?}"),
    }
}

/// Presence as one client sees it, built from its welcome and events.
#[derive(Default, Debug, PartialEq)]
struct View(BTreeMap<UserId, Option<RoomId>>);

impl View {
    fn from_welcome(welcome: &Welcome) -> Self {
        Self(welcome.presence.iter().map(|p| (p.user, p.room)).collect())
    }

    fn apply(&mut self, event: &Event) {
        match event {
            Event::PresenceChanged(Presence { user, room, .. }) => {
                self.0.insert(*user, *room);
            }
            Event::PresenceEnded(user) => {
                self.0.remove(user);
            }
            _ => {}
        }
    }
}

async fn settle(client: &mut TestClient, view: &mut View) {
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), client.link.events.recv()).await
    {
        view.apply(&event);
    }
}

#[tokio::test]
async fn three_clients_see_consistent_presence() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let gaming = create_room(&mut alice, "Gaming").await;
    let mut bob = invited(&server, &alice).await;
    let mut carol = invited(&server, &alice).await;
    let (bob_id, carol_id) = (bob.link.welcome.me, carol.link.welcome.me);

    let mut views = [
        View::from_welcome(&alice.link.welcome),
        View::from_welcome(&bob.link.welcome),
        View::from_welcome(&carol.link.welcome),
    ];

    let bob_placed = join(&bob, Some(general)).await;
    assert_eq!(bob_placed.peers.len(), 1);
    let carol_placed = join(&carol, Some(general)).await;
    assert_eq!(carol_placed.peers.len(), 2);
    assert!(carol_placed.epoch > bob_placed.epoch);
    let bob_state = bob
        .event(|e| match e {
            Event::RoomState { room, epoch, peers } if *room == general => {
                Some((*epoch, peers.len()))
            }
            _ => None,
        })
        .await;
    assert_eq!(bob_state, (carol_placed.epoch, 2));

    join(&bob, Some(gaming)).await;
    for (client, view) in [&mut alice, &mut bob, &mut carol]
        .into_iter()
        .zip(views.iter_mut())
    {
        settle(client, view).await;
    }
    let expected = View(BTreeMap::from([
        (bob_id, Some(gaming)),
        (carol_id, Some(general)),
    ]));
    for view in &views {
        assert_eq!(view, &expected);
    }

    carol.link.connected.connection.close(0u32.into(), b"bye");
    for (client, view) in [&mut alice, &mut bob].into_iter().zip(views.iter_mut()) {
        client
            .event(|e| matches!(e, Event::PresenceEnded(u) if *u == carol_id).then_some(()))
            .await;
        view.apply(&Event::PresenceEnded(carol_id));
    }
    let expected = View(BTreeMap::from([(bob_id, Some(gaming))]));
    assert_eq!(views[0], expected);
    assert_eq!(views[1], expected);

    assert_eq!(
        alice.request(Request::DeleteRoom(gaming)).await,
        Response::Ok
    );
    bob.event(|e| matches!(e, Event::PresenceEnded(u) if *u == bob_id).then_some(()))
        .await;
    bob.event(|e| matches!(e, Event::RoomDeleted(r) if *r == gaming).then_some(()))
        .await;
}

#[tokio::test]
async fn failed_set_room_leaves_the_client_in_place() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let placed = join(&bob, Some(general)).await;
    let missing = bob.request(Request::SetRoom(Some(RoomId(999)))).await;
    assert_error(&missing, ErrorCode::NotFound);
    let again = join(&bob, Some(general)).await;
    assert_eq!(again.tracks, placed.tracks);
}

#[tokio::test]
async fn reconnect_returns_to_the_wanted_room_with_fresh_tracks() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let first = join(&bob, Some(general)).await;

    bob.link.connected.connection.close(0u32.into(), b"");
    let link = account::sign_in(&bob.keystore, &server.addr, Some(general))
        .await
        .unwrap();
    let placed = link.welcome.placed;
    assert_eq!(placed.room, Some(general));
    assert!(placed.epoch > first.epoch);
    let (old, new) = (first.tracks.unwrap(), placed.tracks.unwrap());
    assert!(new.mic > old.shares[0].audio);
}

#[tokio::test]
async fn profile_changes_are_rate_limited_and_avatars_capped() {
    let server = start_server();
    let alice = admin(&server).await;
    let profile = |name: &str| Request::SetProfile {
        name: name.into(),
        avatar: None,
    };
    assert_eq!(alice.request(profile("Alice")).await, Response::Ok);
    let second = alice.request(profile("Alicia")).await;
    assert_error(&second, ErrorCode::RateLimited);
    let big = alice
        .request(Request::SetAvatar(vec![0; MAX_AVATAR_BYTES + 1]))
        .await;
    assert_error(&big, ErrorCode::TooLarge);
    let empty_name = alice.request(profile("   ")).await;
    assert_error(&empty_name, ErrorCode::Invalid);
}

/// Runs a server on its own runtime, so stopping it drops everything like a process exit.
struct ProcessLikeServer {
    addr: SocketAddr,
    bootstrap: Option<throcc_proto::invite::InviteCode>,
    stop: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl ProcessLikeServer {
    fn start(dir: &std::path::Path, listen: SocketAddr) -> Self {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop, stop_rx) = std::sync::mpsc::channel::<()>();
        let dir = dir.to_owned();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = Server::bind(&Config {
                    data_dir: dir,
                    listen,
                    congestion: Congestion::Cubic,
                })
                .unwrap();
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
        }
    }

    fn stop(self) {
        self.stop.send(()).unwrap();
        self.thread.join().unwrap();
    }
}

#[tokio::test]
async fn media_ids_never_repeat_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(dir.path(), "127.0.0.1:0".parse().unwrap());
    let addr = first.addr;
    let mut alice = enroll_at(&addr.to_string(), first.bootstrap.as_ref().unwrap()).await;
    let general = create_room(&mut alice, "General").await;
    let before = join(&alice, Some(general)).await.tracks.unwrap();
    first.stop();

    let second = ProcessLikeServer::start(dir.path(), addr);
    assert!(second.bootstrap.is_none());
    let link = account::sign_in(&alice.keystore, &addr.to_string(), Some(general))
        .await
        .unwrap();
    let after = link.welcome.placed.tracks.unwrap();
    assert!(
        after.mic > before.shares[0].audio,
        "{before:?} then {after:?}"
    );
    assert!(link.welcome.placed.epoch > Epoch(1));
    second.stop();
}

/// Forwards UDP between a client and the server until `cut` is set.
async fn start_cuttable_relay(server: SocketAddr) -> (SocketAddr, Arc<AtomicBool>) {
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap();
    let cut = Arc::new(AtomicBool::new(false));
    let client: Arc<std::sync::Mutex<Option<SocketAddr>>> = Arc::default();
    {
        let (front, back, cut, client) = (front.clone(), back.clone(), cut.clone(), client.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, from)) = front.recv_from(&mut buf).await {
                *client.lock().unwrap() = Some(from);
                if !cut.load(Ordering::Relaxed) {
                    back.send(&buf[..n]).await.ok();
                }
            }
        });
    }
    {
        let cut = cut.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok(n) = back.recv(&mut buf).await {
                let to = *client.lock().unwrap();
                if let Some(to) = to
                    && !cut.load(Ordering::Relaxed)
                {
                    front.send_to(&buf[..n], to).await.ok();
                }
            }
        });
    }
    (addr, cut)
}

#[tokio::test]
async fn killed_connection_retracts_presence_within_the_idle_timeout() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let code = invite(&alice).await;

    let (relay, cut) = start_cuttable_relay(server.addr.parse().unwrap()).await;
    let bob = enroll_at(&relay.to_string(), &code).await;
    let bob_id = bob.link.welcome.me;
    join(&bob, Some(general)).await;
    alice
        .event(|e| matches!(e, Event::PresenceChanged(p) if p.user == bob_id).then_some(()))
        .await;

    cut.store(true, Ordering::Relaxed);
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if let Some(Event::PresenceEnded(user)) = alice.link.events.recv().await
                && user == bob_id
            {
                return;
            }
        }
    })
    .await
    .expect("presence was not retracted");
    let elapsed = started.elapsed();
    assert!(elapsed <= Duration::from_secs(31), "took {elapsed:?}");
    drop(bob);
}
