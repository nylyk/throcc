use std::time::{Duration, Instant};

use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::client::{Client, ClientApi, ClientOptions, Command};
use throcc_client_core::keystore::Keystore;
use throcc_client_core::state::{ConnectionStatus, State};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Codec, MediaState, Request, Response, ShareConfig};
use throcc_server::{Config, Congestion, Server};

/// The server takes one profile change per 10 s.
const PROFILE_INTERVAL: Duration = Duration::from_millis(10_500);

/// Runs a server on its own runtime thread, as the blocking client API expects.
fn server() -> (String, InviteCode) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let server = Server::bind(&Config {
                data_dir: dir.path().to_owned(),
                listen: "127.0.0.1:0".parse().unwrap(),
                congestion: Congestion::Cubic,
            })
            .unwrap();
            tx.send((
                server.local_addr().unwrap().to_string(),
                server.bootstrap_invite().unwrap().clone(),
            ))
            .unwrap();
            server.run().await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn client(address: &str, invite: &InviteCode) -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let keystore = Keystore::open_in(dir.path()).unwrap();
    let options = ClientOptions {
        audio: false,
        ..Default::default()
    };
    (
        Client::connect(address, keystore, Some(invite), options).unwrap(),
        dir,
    )
}

fn wait_for(client: &Client, what: &str, mut ready: impl FnMut(&State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready(&client.state()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            client.state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn invite(client: &Client) -> InviteCode {
    let mut events = client.events();
    client.command(Command::CreateInvite);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(throcc_client_core::client::Event::Invite { code, .. }) = events.try_recv() {
            return code.parse().unwrap();
        }
        assert!(Instant::now() < deadline, "no invite");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn two_clients_see_each_other_join_and_unmute() {
    let (address, bootstrap) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_for(&alice, "connected", |s| {
        s.connection == ConnectionStatus::Connected
    });
    alice.command(Command::CreateRoom("General".into()));
    wait_for(&alice, "the room", |s| !s.rooms.is_empty());
    let room = alice.state().rooms[0].id;

    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let bob_id = bob.state().me.unwrap();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    bob.command(Command::SetMic(true));
    wait_for(&alice, "bob's mic on", |s| {
        s.person(bob_id).is_some_and(|p| p.mic)
    });
    wait_for(&bob, "bob's own state", |s| {
        s.current.as_ref().is_some_and(|c| c.mic)
    });

    bob.command(Command::SetDeafened(true));
    wait_for(&alice, "bob deafened and muted", |s| {
        s.person(bob_id).is_some_and(|p| p.deafened && !p.mic)
    });

    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "a pinned key for alice", |_| {
        bob.safety_number(alice_id).is_some()
    });
    assert_eq!(bob.safety_number(alice_id), alice.safety_number(bob_id));

    bob.command(Command::Disconnect);
    wait_for(&alice, "bob gone", |s| s.person(bob_id).is_none());
}

#[test]
fn a_picture_chosen_in_the_lobby_reaches_others_and_survives_a_rename() {
    let (address, bootstrap) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_for(&alice, "connected", |s| {
        s.connection == ConnectionStatus::Connected
    });
    let picture: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
    alice.command(Command::SetAvatar(Some(picture.clone())));
    alice.command(Command::CreateRoom("General".into()));
    wait_for(&alice, "the room", |s| !s.rooms.is_empty());
    let room = alice.state().rooms[0].id;
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice in the room", |s| s.person(alice_id).is_some());

    wait_for(&bob, "alice's avatar hash", |s| {
        s.person(alice_id).is_some_and(|p| p.avatar.is_some())
    });
    let hash = bob.state().person(alice_id).unwrap().avatar.unwrap();
    wait_for(&bob, "alice's picture", |_| bob.avatar(hash).is_some());
    assert_eq!(&*bob.avatar(hash).unwrap(), picture.as_slice());

    std::thread::sleep(PROFILE_INTERVAL);
    alice.command(Command::SetName("Alice".into()));
    wait_for(&bob, "alice's new name", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Alice")
    });
    assert_eq!(bob.state().person(alice_id).unwrap().avatar, Some(hash));
}

#[test]
fn a_share_started_mid_call_shows_without_rejoining() {
    let (address, bootstrap) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_for(&alice, "connected", |s| {
        s.connection == ConnectionStatus::Connected
    });
    alice.command(Command::CreateRoom("General".into()));
    wait_for(&alice, "the room", |s| !s.rooms.is_empty());
    let room = alice.state().rooms[0].id;
    alice.command(Command::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());

    let code = invite(&alice);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bob = runtime.block_on(async {
        let mut keystore = Keystore::open_in(dir.path()).unwrap();
        let link = account::enroll(&mut keystore, &address, &code)
            .await
            .unwrap();
        let bob = Call::start(link, &keystore).unwrap();
        bob.join(Some(room)).await.unwrap();
        bob
    });
    let bob_id = bob.me();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    let share = ShareConfig {
        audio: false,
        kbps: 6000,
        codec: Codec::Vp9,
        width: 1920,
        height: 1080,
        fps: 60,
    };
    let media = MediaState {
        mic: true,
        deafened: false,
        share: Some(share),
    };
    let reply = runtime.block_on(bob.request(Request::SetMedia(media)));
    assert_eq!(reply.unwrap(), Response::Ok);
    wait_for(&alice, "bob's share", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.mic && p.sharing == Some(share))
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(alice.state().person(bob_id).unwrap().sharing, Some(share));
}
