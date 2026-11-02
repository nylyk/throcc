mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use throcc_client_core::account;
use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::client::{Client, ClientApi, ClientOptions, Command, Event};
use throcc_client_core::keystore::Keystore;
use throcc_client_core::state::{ConnectionStatus, KeyWarning, State};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    Codec, Event as ServerEvent, MediaState, PublicKey, Request, Response,
};
use throcc_server::identity::KEY_FILE;
use tokio::runtime::Runtime;

use common::{ProcessLikeServer, share_1080p60, sharing};

/// The server takes one profile change per 10 s.
const PROFILE_INTERVAL: Duration = Duration::from_millis(10_500);

/// A server's address and an admin invite. It runs until the returned handle drops.
fn server() -> (String, InviteCode, ProcessLikeServer) {
    let server = ProcessLikeServer::start_temp();
    let bootstrap = server.admin_invite();
    (server.addr.to_string(), bootstrap, server)
}

fn client(address: &str, invite: &InviteCode) -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (client_in(dir.path(), address, Some(invite)), dir)
}

/// A client keeping its keys and profile in `dir`, enrolling with `invite` if given.
fn client_in(dir: &Path, address: &str, invite: Option<&InviteCode>) -> Client {
    let keystore = Keystore::open_in(dir).unwrap();
    let options = ClientOptions {
        audio: false,
        ..Default::default()
    };
    Client::connect(address, keystore, invite, options).unwrap()
}

fn wait_for(client: &Client, what: &str, ready: impl FnMut(&State) -> bool) {
    wait_for_within(Duration::from_secs(5), client, what, ready);
}

fn wait_for_within(
    timeout: Duration,
    client: &Client,
    what: &str,
    mut ready: impl FnMut(&State) -> bool,
) {
    let deadline = Instant::now() + timeout;
    while !ready(&client.state()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            client.state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until `ready` holds, for checks on a bare call.
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn identity_in(dir: &Path) -> PublicKey {
    Keystore::open_in(dir).unwrap().identity().public()
}

/// A bare call keeping its keys in `dir`, enrolled with `invite`, placed in `room`.
fn call_in(
    runtime: &Runtime,
    dir: &Path,
    address: &str,
    invite: &InviteCode,
    room: RoomId,
) -> Call {
    runtime.block_on(async {
        let mut keystore = Keystore::open_in(dir).unwrap();
        let link = account::enroll(&mut keystore, address, invite)
            .await
            .unwrap();
        let call = Call::start(link, &keystore).unwrap();
        call.join(Some(room), None).await.unwrap();
        call
    })
}

/// Whether `viewer` holds the key `sender` sends their mic with in the current epoch.
fn holds_key_of(viewer: &Call, sender: UserId) -> bool {
    let mut keys = viewer.engine.keys();
    let Some(mic) = keys.peer_tracks(sender).map(|t| t.mic) else {
        return false;
    };
    let epoch = keys.epoch();
    keys.recv_key(mic, epoch).is_some()
}

/// Puts `alice` and `bob`, enrolled in `alice_dir` and `bob_dir`, in a room at `address`,
/// and has alice verify bob. Returns bob's user id.
fn verify_in_a_room(alice_dir: &Path, bob_dir: &Path, address: &str, admin: &InviteCode) -> UserId {
    let alice = client_in(alice_dir, address, Some(admin));
    wait_connected(&alice, "alice connected");
    let room = create_room(&alice);
    let bob = client_in(bob_dir, address, Some(&invite(&alice)));
    let bob_id = bob.state().me.unwrap();
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let key = key_of(&alice, bob_id);
    alice.command(verify(bob_id, key));
    wait_for(&alice, "bob verified", |s| {
        s.person(bob_id).is_some_and(|p| p.verified)
    });
    bob_id
}

/// Waits for the identity key `user` presents to `viewer`.
fn key_of(viewer: &Client, user: UserId) -> PublicKey {
    wait_for(viewer, "their key", |s| {
        s.person(user).is_some_and(|p| p.identity_key.is_some())
    });
    viewer.state().person(user).unwrap().identity_key.unwrap()
}

fn verify(user: UserId, key: PublicKey) -> Command {
    Command::SetVerified {
        user,
        key: Fingerprint(key),
        verified: true,
    }
}

fn wait_connected(client: &Client, what: &str) {
    wait_for(client, what, |s| {
        s.connection == ConnectionStatus::Connected
    });
}

/// Sends `command`, then waits for the first event that `pick` accepts.
fn command_then<T>(
    client: &Client,
    command: Command,
    what: &str,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    let mut events = client.events();
    client.command(command);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(event) = events.try_recv()
            && let Some(found) = pick(event)
        {
            return found;
        }
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn invite(client: &Client) -> InviteCode {
    command_then(client, Command::CreateInvite, "no invite", |e| match e {
        Event::Invite { code, .. } => Some(code.parse().unwrap()),
        _ => None,
    })
}

fn create_room(client: &Client) -> RoomId {
    client.command(Command::CreateRoom("General".into(), None));
    wait_for(client, "the room", |s| !s.rooms.is_empty());
    client.state().rooms[0].id
}

#[test]
fn mic_and_deafen_set_outside_a_room_carry_into_it() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    wait_connected(&bob, "bob connected");
    let bob_id = bob.state().me.unwrap();

    bob.command(Command::SetDeafened(true));
    wait_for(&bob, "bob deafened in the lobby", |s| s.deafened && !s.mic);
    bob.command(Command::Join(Some(room)));
    wait_for(&alice, "bob in the room, deafened", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.room == Some(room) && p.deafened && !p.mic)
    });

    bob.command(Command::Join(None));
    bob.command(Command::SetDeafened(false));
    bob.command(Command::Join(Some(room)));
    wait_for(&alice, "bob back in the room, hearing and talking", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.room == Some(room) && !p.deafened && p.mic)
    });
}

#[test]
fn two_clients_see_each_other_join_and_unmute() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);

    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let bob_id = bob.state().me.unwrap();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    wait_for(&alice, "bob's mic on", |s| {
        s.person(bob_id).is_some_and(|p| p.mic)
    });
    bob.command(Command::SetMic(false));
    wait_for(&alice, "bob's mic off", |s| {
        s.person(bob_id).is_some_and(|p| !p.mic)
    });
    wait_for(&bob, "bob's own state", |s| !s.mic);

    bob.command(Command::SetDeafened(true));
    wait_for(&alice, "bob deafened and muted", |s| {
        s.person(bob_id).is_some_and(|p| p.deafened && !p.mic)
    });

    let alice_id = alice.state().me.unwrap();
    assert_eq!(
        bob.safety_number(&key_of(&bob, alice_id)),
        alice.safety_number(&key_of(&alice, bob_id))
    );

    bob.command(Command::Disconnect);
    wait_for(&alice, "bob gone", |s| s.person(bob_id).is_none());
}

#[test]
fn a_picture_chosen_in_the_lobby_reaches_others_and_survives_a_rename() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let picture: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
    alice.command(Command::SetProfile {
        name: None,
        picture: Some(Some(picture.clone())),
    });
    let room = create_room(&alice);
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
    alice.command(Command::SetProfile {
        name: Some("Alice".into()),
        picture: None,
    });
    wait_for(&bob, "alice's new name", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Alice")
    });
    assert_eq!(bob.state().person(alice_id).unwrap().avatar, Some(hash));
}

#[test]
fn a_share_started_mid_call_shows_without_rejoining() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
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
        bob.join(Some(room), None).await.unwrap();
        bob
    });
    let bob_id = bob.me();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    let share = share_1080p60(Codec::Vp9, 6000);
    let media = MediaState {
        mic: true,
        ..sharing(share)
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

#[test]
fn a_sharer_changing_their_mic_never_shows_as_not_sharing() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
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
        bob.join(Some(room), None).await.unwrap();
        bob
    });
    let bob_id = bob.me();
    let share = share_1080p60(Codec::Vp9, 6000);
    let reply = runtime.block_on(bob.request(Request::SetMedia(sharing(share))));
    assert_eq!(reply.unwrap(), Response::Ok);
    wait_for(&alice, "bob's share", |s| {
        s.person(bob_id).is_some_and(|p| p.sharing == Some(share))
    });

    let mut events = alice.events();
    let media = MediaState {
        mic: true,
        ..sharing(share)
    };
    let reply = runtime.block_on(bob.request(Request::SetMedia(media)));
    assert_eq!(reply.unwrap(), Response::Ok);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "bob's mic never came on");
        let Ok(Event::State(state)) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let bob = state.person(bob_id).unwrap();
        assert_eq!(bob.sharing, Some(share), "bob shown as not sharing");
        if bob.mic {
            break;
        }
    }
}

#[test]
fn a_request_too_large_to_send_fails_alone() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let command = Command::SetServerProfile {
        name: None,
        picture: Some(Some(vec![0; 2 << 20])),
    };
    command_then(&alice, command, "no refusal", |e| match e {
        Event::ServerProfileRefused(_) => Some(()),
        _ => None,
    });
    invite(&alice);
}

#[test]
fn a_dropped_client_leaves_at_once() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    let bob_id = bob.state().me.unwrap();
    bob.command(Command::Join(Some(room)));
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    drop(bob);
    wait_for(&alice, "bob gone well before the idle timeout", |s| {
        s.person(bob_id).is_none()
    });
}

#[test]
fn the_profile_follows_us_to_the_next_server() {
    let dir = tempfile::tempdir().unwrap();
    let (first, first_invite, _first) = server();
    let alice = client_in(dir.path(), &first, Some(&first_invite));
    wait_connected(&alice, "connected");
    let picture: Vec<u8> = (0..2000u32).map(|i| (i * 3) as u8).collect();
    alice.command(Command::SetProfile {
        name: Some("Ilse".into()),
        picture: Some(Some(picture.clone())),
    });
    wait_for(&alice, "the profile kept", |s| {
        s.profile_name.as_deref() == Some("Ilse") && s.profile_picture.is_some()
    });
    alice.command(Command::Disconnect);

    let (second, second_invite, _second) = server();
    let alice = client_in(dir.path(), &second, Some(&second_invite));
    wait_connected(&alice, "connected again");
    assert_eq!(alice.state().profile_name.as_deref(), Some("Ilse"));
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&second, &code);
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice's profile on the second server", |s| {
        s.person(alice_id)
            .is_some_and(|p| p.name == "Ilse" && p.avatar.is_some())
    });
    let hash = bob.state().person(alice_id).unwrap().avatar.unwrap();
    wait_for(&bob, "alice's picture", |_| bob.avatar(hash).is_some());
    assert_eq!(&*bob.avatar(hash).unwrap(), picture.as_slice());
}

#[test]
fn only_admins_name_the_server_and_everyone_sees_it() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    alice.command(Command::SetServerProfile {
        name: Some(Some("  Moss Co-op ".into())),
        picture: None,
    });
    wait_for(&alice, "the server's name", |s| {
        s.server.as_ref().and_then(|s| s.name.as_deref()) == Some("Moss Co-op")
    });

    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    assert_eq!(bob.state().server.clone().unwrap().title(), "Moss Co-op");

    let picture: Vec<u8> = (0..1500u32).map(|i| (i * 3) as u8).collect();
    alice.command(Command::SetServerProfile {
        name: None,
        picture: Some(Some(picture.clone())),
    });
    wait_for(&bob, "the server's picture", |s| {
        s.server.as_ref().is_some_and(|s| s.avatar.is_some())
    });
    let hash = bob.state().server.clone().unwrap().avatar.unwrap();
    wait_for(&bob, "the picture's bytes", |_| bob.avatar(hash).is_some());
    assert_eq!(&*bob.avatar(hash).unwrap(), picture.as_slice());
    assert_eq!(bob.state().server.clone().unwrap().title(), "Moss Co-op");

    let rename = Command::SetServerProfile {
        name: Some(Some("Bob's".into())),
        picture: None,
    };
    command_then(&bob, rename, "bob's rename wasn't refused", |e| {
        matches!(e, Event::ServerProfileRefused(_)).then_some(())
    });
    assert_eq!(alice.state().server.clone().unwrap().title(), "Moss Co-op");

    alice.command(Command::SetServerProfile {
        name: Some(None),
        picture: None,
    });
    wait_for(&bob, "the name cleared", |s| {
        s.server
            .as_ref()
            .is_some_and(|s| s.name.is_none() && s.avatar == Some(hash))
    });

    let other: Vec<u8> = (0..900u32).map(|i| (i * 5) as u8).collect();
    alice.command(Command::SetServerProfile {
        name: Some(Some("Fern".into())),
        picture: Some(Some(other)),
    });
    wait_for(&bob, "both changes", |s| {
        s.server.as_ref().is_some_and(|s| {
            s.name.as_deref() == Some("Fern") && s.avatar.is_some_and(|a| a != hash)
        })
    });
}

#[test]
fn a_verified_contact_stays_verified_in_the_next_session() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let bob_dir = tempfile::tempdir().unwrap();
    let bob = client_in(bob_dir.path(), &address, Some(&code));
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    bob.command(verify(alice_id, key_of(&bob, alice_id)));
    wait_for(&bob, "alice verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
    drop(bob);

    let bob = client_in(bob_dir.path(), &address, None);
    wait_connected(&bob, "bob back");
    assert!(
        bob.state()
            .person(alice_id)
            .is_some_and(|p| !p.verified && p.identity_key.is_none()),
        "verified before alice presented her key"
    );
    bob.command(Command::Join(Some(room)));
    wait_for(&bob, "alice still verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
}

#[test]
fn only_the_key_whose_number_was_compared_is_verified() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let (bob, _b) = client(&address, &invite(&alice));
    let bob_id = bob.state().me.unwrap();
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    let presented = key_of(&alice, bob_id);

    let compared = [9; 32];
    command_then(&alice, verify(bob_id, compared), "no notice", refused);
    assert!(alice.state().verified.is_empty());
    assert!(!alice.state().person(bob_id).unwrap().verified);

    alice.command(verify(bob_id, presented));
    wait_for(&alice, "bob verified", |s| {
        s.person(bob_id).is_some_and(|p| p.verified)
    });
    let unverify = Command::SetVerified {
        user: bob_id,
        key: Fingerprint(compared),
        verified: false,
    };
    command_then(&alice, unverify, "no notice", refused);
    assert!(alice.state().person(bob_id).unwrap().verified);
}

fn refused(event: Event) -> Option<()> {
    matches!(event, Event::Notice(n) if n.contains("before you confirmed")).then_some(())
}

#[test]
fn a_verified_key_is_verified_on_every_server_it_shows_up_on() {
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (first, first_admin, _first) = server();
    verify_in_a_room(alice_dir.path(), bob_dir.path(), &first, &first_admin);

    let (second, second_admin, _second) = server();
    let alice = client_in(alice_dir.path(), &second, Some(&second_admin));
    wait_connected(&alice, "alice on the second server");
    let room = create_room(&alice);
    let bob = client_in(bob_dir.path(), &second, Some(&invite(&alice)));
    let bob_id = bob.state().me.unwrap();
    alice.command(Command::Join(Some(room)));
    bob.command(Command::Join(Some(room)));
    wait_for(&alice, "bob verified on the second server", |s| {
        s.person(bob_id).is_some_and(|p| p.verified) && s.key_warnings.is_empty()
    });
    let verified = alice.state().verified.clone();
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].key, Fingerprint(identity_in(bob_dir.path())));
    assert_eq!(verified[0].servers.len(), 2);
}

#[test]
fn a_new_key_for_a_verified_persons_user_id_is_warned_of_and_still_hears_us() {
    let first_dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(first_dir.path(), "127.0.0.1:0".parse().unwrap());
    let (addr, address) = (first.addr, first.addr.to_string());
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let admin = first.admin_invite();
    let bob_id = verify_in_a_room(alice_dir.path(), bob_dir.path(), &address, &admin);
    first.stop();

    // The same key with a fresh database, as a rebuilt server: its user ids start over.
    let rebuilt_dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        first_dir.path().join(KEY_FILE),
        rebuilt_dir.path().join(KEY_FILE),
    )
    .unwrap();
    let rebuilt = ProcessLikeServer::start(rebuilt_dir.path(), addr);
    let alice = client_in(alice_dir.path(), &address, Some(&rebuilt.admin_invite()));
    wait_connected(&alice, "alice on the rebuilt server");
    let room = create_room(&alice);
    alice.command(Command::CreateRoom("Elsewhere".into(), None));
    wait_for(&alice, "the other room", |s| s.rooms.len() == 2);
    let elsewhere = alice
        .state()
        .rooms
        .iter()
        .find(|r| r.id != room)
        .unwrap()
        .id;
    alice.command(Command::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    let alice_id = alice.state().me.unwrap();
    let code = invite(&alice);
    let runtime = Runtime::new().unwrap();
    let carol_dir = tempfile::tempdir().unwrap();
    let carol = call_in(&runtime, carol_dir.path(), &address, &code, elsewhere);
    assert_eq!(carol.me(), bob_id, "carol didn't get bob's old user id");

    wait_for(&alice, "carol in the other room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(elsewhere))
    });
    let shown = alice.state().person(bob_id).cloned().unwrap();
    assert!(
        !shown.verified && shown.identity_key.is_none(),
        "carol shown with bob's verified key: {shown:?}"
    );
    assert!(alice.state().key_warnings.is_empty());

    runtime.block_on(carol.join(Some(room), None)).unwrap();
    let warning = KeyWarning {
        user: bob_id,
        verified: Fingerprint(identity_in(bob_dir.path())),
        now: Fingerprint(identity_in(carol_dir.path())),
    };
    wait_for(&alice, "a warning that bob's key changed", |s| {
        s.key_warnings == [warning.clone()] && s.person(bob_id).is_some_and(|p| !p.verified)
    });
    wait_until("carol to hold alice's key", || {
        holds_key_of(&carol, alice_id)
    });

    drop(alice);
    let alice = client_in(alice_dir.path(), &address, None);
    wait_connected(&alice, "alice back in the lobby");
    wait_for(&alice, "the warning after a restart", |s| {
        s.key_warnings == [warning.clone()]
    });

    alice.command(Command::DismissKeyChange(bob_id));
    wait_for(&alice, "the warning dismissed", |s| {
        s.key_warnings.is_empty()
    });
    assert!(!alice.state().person(bob_id).unwrap().verified);
    assert_eq!(
        alice.state().verified.len(),
        1,
        "bob's key is still verified"
    );
}

#[test]
fn people_not_verified_are_shown_so_and_still_hear_us() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(Command::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    let alice_id = alice.state().me.unwrap();
    let code = invite(&alice);
    let runtime = Runtime::new().unwrap();
    let bob_dir = tempfile::tempdir().unwrap();
    let bob = call_in(&runtime, bob_dir.path(), &address, &code, room);

    wait_for(&alice, "bob's key, not verified", |s| {
        s.person(bob.me())
            .is_some_and(|p| p.identity_key.is_some() && !p.verified)
            && s.key_warnings.is_empty()
    });
    wait_until("bob to hold alice's key", || holds_key_of(&bob, alice_id));
    wait_for_within(Duration::from_secs(10), &alice, "bob's mic followed", |s| {
        s.health
            .receiving
            .iter()
            .any(|t| t.label.ends_with(", mic"))
    });
}

#[test]
fn an_epoch_floor_outlives_a_restart_and_refuses_a_replayed_sender_key() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(Command::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    let alice_id = alice.state().me.unwrap();
    let code = invite(&alice);
    let runtime = Runtime::new().unwrap();
    let bob_dir = tempfile::tempdir().unwrap();
    let (mut bob, placed) = runtime.block_on(async {
        let mut keystore = Keystore::open_in(bob_dir.path()).unwrap();
        let link = account::enroll(&mut keystore, &address, &code)
            .await
            .unwrap();
        let bob = Call::start(link, &keystore).unwrap();
        let (placed, _) = bob.join(Some(room), None).await.unwrap();
        (bob, placed)
    });
    // Alice's sender key for bob, as a server could record it to replay later.
    let (epoch, wrapped) = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(CallEvent::Server(ServerEvent::SenderKey {
                    from,
                    epoch,
                    wrapped,
                })) = bob.events.recv().await
                    && from == alice_id
                {
                    return (epoch, wrapped);
                }
            }
        })
        .await
        .expect("no sender key from alice")
    });
    assert_eq!(epoch, placed.epoch);
    // The same install before its floors were written, to show the replay works without them.
    let unsaved_dir = tempfile::tempdir().unwrap();
    for file in std::fs::read_dir(bob_dir.path()).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), unsaved_dir.path().join(file.file_name())).unwrap();
    }
    runtime.block_on(bob.close());
    drop(bob);

    // Starting again, then shown the old placement and key, as a replaying server would.
    let replayed = |dir: &Path| {
        let bob = runtime.block_on(async {
            let keystore = Keystore::open_in(dir).unwrap();
            let link = account::sign_in(&keystore, &address, None).await.unwrap();
            Call::start(link, &keystore).unwrap()
        });
        let notices = format!("{:?}", bob.engine.placed(&placed));
        let taken = bob
            .engine
            .keys()
            .sender_key(alice_id, epoch, &wrapped, Instant::now());
        let used = holds_key_of(&bob, alice_id);
        runtime.block_on(bob.close());
        (notices, taken, used)
    };
    let (notices, taken, used) = replayed(bob_dir.path());
    assert!(notices.contains("OldEpoch"), "{notices}");
    assert!(taken.is_err() && !used, "the replayed sender key was taken");
    let (notices, taken, used) = replayed(unsaved_dir.path());
    assert!(!notices.contains("OldEpoch"), "{notices}");
    assert!(taken.is_ok() && used, "{taken:?}");
}

#[test]
fn enrolling_again_takes_a_new_encryption_key_and_drops_that_accounts_floors() {
    let first_dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(first_dir.path(), "127.0.0.1:0".parse().unwrap());
    let (addr, address) = (first.addr, first.addr.to_string());
    let alice_dir = tempfile::tempdir().unwrap();
    let enc_key = || {
        Keystore::open_in(alice_dir.path())
            .unwrap()
            .enc_key()
            .public()
    };
    let before = enc_key();
    let alice = client_in(alice_dir.path(), &address, Some(&first.admin_invite()));
    wait_connected(&alice, "alice connected");
    let first_key = enc_key();
    assert_ne!(first_key, before, "enrolled with the old encryption key");
    let room = create_room(&alice);
    alice.command(Command::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    drop(alice);
    first.stop();

    // The same key with a fresh database, as a rebuilt server: its user ids and rooms
    // start over, so a floor kept for alice's user id there would silence her new room.
    let rebuilt_dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        first_dir.path().join(KEY_FILE),
        rebuilt_dir.path().join(KEY_FILE),
    )
    .unwrap();
    let rebuilt = ProcessLikeServer::start(rebuilt_dir.path(), addr);
    let runtime = Runtime::new().unwrap();
    let notices = runtime.block_on(async {
        let mut keystore = Keystore::open_in(alice_dir.path()).unwrap();
        let link = account::enroll(&mut keystore, &address, &rebuilt.admin_invite())
            .await
            .unwrap();
        assert_ne!(keystore.enc_key().public(), first_key);
        let call = Call::start(link, &keystore).unwrap();
        let created = call
            .request(Request::CreateRoom {
                name: "General".into(),
                password: None,
            })
            .await
            .unwrap();
        assert!(matches!(created, Response::Ok), "{created:?}");
        let (_, notices) = call.join(Some(room), None).await.unwrap();
        call.close().await;
        notices
    });
    assert!(!format!("{notices:?}").contains("OldEpoch"), "{notices:?}");
}

#[test]
fn a_removed_member_stops_reconnecting() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    let bob_id = bob.state().me.unwrap();
    alice.command(Command::RemoveUser {
        user: bob_id,
        cascade: false,
    });
    wait_for_within(Duration::from_secs(10), &bob, "bob to give up", |s| {
        matches!(s.connection, ConnectionStatus::Failed(_))
    });
}
