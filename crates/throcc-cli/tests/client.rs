mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::client::{Client, ClientApi, ClientOptions, Command, Event};
use throcc_client_core::keystore::Keystore;
use throcc_client_core::state::{ConnectionStatus, KeyWarning, State, WarningCause};
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Codec, MediaState, Request, Response};

use common::{ProcessLikeServer, share_1080p60, sharing};

/// The server takes one profile change per 10 s.
const PROFILE_INTERVAL: Duration = Duration::from_millis(10_500);

/// A server's address and bootstrap invite. It runs until the returned handle drops.
fn server() -> (String, InviteCode, ProcessLikeServer) {
    let server = ProcessLikeServer::start_temp();
    let bootstrap = server.bootstrap.clone().unwrap();
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
    wait_for(&bob, "a pinned key for alice", |_| {
        bob.safety_number(alice_id).is_some()
    });
    assert_eq!(bob.safety_number(alice_id), alice.safety_number(bob_id));

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
    wait_for(&bob, "a pinned key for alice", |_| {
        bob.safety_number(alice_id).is_some()
    });
    bob.command(Command::SetVerified(alice_id, true));
    wait_for(&bob, "alice verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
    drop(bob);

    let bob = client_in(bob_dir.path(), &address, None);
    wait_for(&bob, "alice still verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
}

#[test]
fn someone_new_using_a_known_persons_name_is_warned_of_until_verified() {
    let (address, bootstrap, _server) = server();
    let (alice, _a) = client(&address, &bootstrap);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(Command::Join(Some(room)));
    let joined_as = |name: &str, rename_after_joining: bool| {
        let (member, dir) = client(&address, &invite(&alice));
        let id = member.state().me.unwrap();
        let rename = Command::SetProfile {
            name: Some(name.into()),
            picture: None,
        };
        if rename_after_joining {
            member.command(Command::Join(Some(room)));
            wait_for(&alice, "a pinned key for them", |_| {
                alice.safety_number(id).is_some()
            });
            member.command(rename);
        } else {
            member.command(rename);
            member.command(Command::Join(Some(room)));
        }
        wait_for(&alice, "their name", |s| {
            s.person(id).is_some_and(|p| p.name == name)
        });
        (member, id, dir)
    };

    let (bob, bob_id, _b) = joined_as("Bob", true);
    bob.command(Command::Disconnect);
    wait_for(&alice, "bob gone", |s| s.person(bob_id).is_none());

    let (_other, other_id, _o) = joined_as("BOB", false);
    let warning = KeyWarning {
        user: other_id,
        cause: WarningCause::SameName { known: bob_id },
    };
    wait_for(&alice, "a warning naming both", |s| {
        s.key_warnings.contains(&warning)
    });
    alice.command(Command::SetVerified(other_id, true));
    wait_for(&alice, "the warning gone once verified", |s| {
        s.key_warnings.is_empty()
    });
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
