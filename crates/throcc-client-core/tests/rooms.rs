mod common;

use common::{SERVER_LABEL, TestServer, keystore, next_event};
use tempfile::TempDir;
use throcc_client_core::{Client, Command, Event};
use throcc_proto::{Placed, Role, RoomId};

fn admin(server: &TestServer, directory: &TempDir) -> Client {
    Client::connect(
        server.address,
        SERVER_LABEL,
        keystore(directory),
        Some(server.bootstrap_invite()),
    )
    .expect("the bootstrap code should enroll an admin")
}

fn enrolled(server: &TestServer, directory: &TempDir, admin: &Client, role: Role) -> Client {
    let mut events = admin.events();
    admin
        .command(Command::CreateInvite { role, ttl_secs: 0 })
        .unwrap();
    let code = match next_event(&mut events) {
        Event::Invited { code, .. } => code,
        other => panic!("expected an invite, got {other:?}"),
    };
    Client::connect(
        server.address,
        SERVER_LABEL,
        keystore(directory),
        Some(code),
    )
    .expect("a minted code should enroll")
}

fn created(client: &Client, name: &str) -> RoomId {
    let mut events = client.events();
    client
        .command(Command::CreateRoom {
            name: name.to_string(),
        })
        .unwrap();
    match next_event(&mut events) {
        Event::RoomCreated(room) => {
            assert_eq!(room.name, name);
            room.id
        }
        other => panic!("expected the room, got {other:?}"),
    }
}

fn placed(client: &Client, target: Option<RoomId>) -> Placed {
    let mut events = client.events();
    client.command(Command::SetRoom(target)).unwrap();
    match next_event(&mut events) {
        Event::Placed(placed) => {
            assert_eq!(placed.room, target);
            placed
        }
        other => panic!("expected a placement, got {other:?}"),
    }
}

#[test]
fn a_created_room_reaches_everyone_connected() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let user = enrolled(&server, &user_dir, &admin, Role::User);
    let mut watching = user.events();

    let room = created(&admin, "lounge");
    match next_event(&mut watching) {
        Event::RoomCreated(created) => {
            assert_eq!((created.id, created.name), (room, "lounge".into()))
        }
        other => panic!("the other client should see the room, got {other:?}"),
    }

    user.shutdown();
    admin.shutdown();
}

#[test]
fn a_room_survives_a_reconnect_and_arrives_with_the_welcome() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let room = created(&admin, "lounge");
    admin.shutdown();

    let again = Client::connect(server.address, SERVER_LABEL, keystore(&admin_dir), None).unwrap();
    let rooms = &again.welcome().rooms;
    assert_eq!(rooms.len(), 1);
    assert_eq!((rooms[0].id, rooms[0].name.as_str()), (room, "lounge"));
    again.shutdown();
}

#[test]
fn renaming_and_deleting_reach_everyone_connected() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let room = created(&admin, "lounge");
    let mut events = admin.events();

    admin
        .command(Command::RenameRoom {
            room,
            name: "quiet".into(),
        })
        .unwrap();
    assert_eq!(
        next_event(&mut events),
        Event::RoomRenamed {
            room,
            name: "quiet".into()
        }
    );

    admin.command(Command::DeleteRoom(room)).unwrap();
    assert_eq!(next_event(&mut events), Event::RoomDeleted(room));
    admin.shutdown();
}

#[test]
fn a_user_cannot_manage_rooms() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let user = enrolled(&server, &user_dir, &admin, Role::User);
    let mut events = user.events();

    user.command(Command::CreateRoom {
        name: "mine".into(),
    })
    .unwrap();
    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("Denied"), "unexpected: {message}"),
        other => panic!("a User must not create rooms, got {other:?}"),
    }

    user.shutdown();
    admin.shutdown();
}

#[test]
fn a_nameless_room_and_a_missing_room_are_refused() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let mut events = admin.events();

    admin
        .command(Command::CreateRoom { name: "  ".into() })
        .unwrap();
    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("Invalid"), "unexpected: {message}"),
        other => panic!("an empty name must be refused, got {other:?}"),
    }

    admin.command(Command::DeleteRoom(RoomId(404))).unwrap();
    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("NotFound"), "unexpected: {message}"),
        other => panic!("a missing room must be refused, got {other:?}"),
    }

    admin.shutdown();
}

#[test]
fn entering_a_room_allocates_tracks_and_bumps_the_epoch() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    assert_eq!(admin.welcome().placed.room, None, "entering is explicit");
    assert!(
        admin.welcome().placed.tracks.is_none(),
        "no room, no tracks"
    );

    let room = created(&admin, "lounge");
    let entered = placed(&admin, Some(room));
    let first = entered.tracks.expect("a room entered allocates tracks");
    assert!(entered.peers.is_empty(), "nobody else is in there");

    let again = placed(&admin, Some(room));
    let second = again
        .tracks
        .expect("every membership change allocates tracks");
    assert!(
        again.epoch > entered.epoch,
        "every membership change bumps the epoch"
    );
    assert!(
        second.mic > first.mic,
        "a media id is never reused: {second:?} follows {first:?}"
    );

    let left = placed(&admin, None);
    assert!(left.tracks.is_none(), "no room, no tracks");
    admin.shutdown();
}

#[test]
fn a_peer_sees_every_transition() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let user = enrolled(&server, &user_dir, &admin, Role::User);
    let room = created(&admin, "lounge");
    placed(&user, Some(room));

    let mut watching = user.events();
    let entered = placed(&admin, Some(room));
    assert_eq!(
        entered.peers.len(),
        1,
        "the peer already in the room is in the placement"
    );
    match next_event(&mut watching) {
        Event::UserEntered {
            room: seen, peer, ..
        } => {
            assert_eq!(seen, room);
            assert_eq!(peer.user, admin.welcome().me);
        }
        other => panic!("expected an entry, got {other:?}"),
    }

    placed(&admin, None);
    match next_event(&mut watching) {
        Event::UserExited {
            room: seen, user, ..
        } => {
            assert_eq!(seen, room);
            assert_eq!(user, admin.welcome().me);
        }
        other => panic!("expected an exit, got {other:?}"),
    }

    user.shutdown();
    admin.shutdown();
}

#[test]
fn a_disconnect_removes_the_occupant() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let user = enrolled(&server, &user_dir, &admin, Role::User);
    let room = created(&admin, "lounge");
    placed(&user, Some(room));
    placed(&admin, Some(room));

    let mut watching = user.events();
    let departing = admin.welcome().me;
    admin.shutdown();
    match next_event(&mut watching) {
        Event::UserExited { user, .. } => assert_eq!(user, departing),
        other => panic!("a closed connection must leave the room, got {other:?}"),
    }

    assert!(
        placed(&user, Some(room)).peers.is_empty(),
        "the room is empty again"
    );
    user.shutdown();
}

#[test]
fn entering_a_room_that_does_not_exist_leaves_you_where_you_were() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let room = created(&admin, "lounge");
    let entered = placed(&admin, Some(room));

    let mut events = admin.events();
    admin.command(Command::SetRoom(Some(RoomId(404)))).unwrap();
    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("NotFound"), "unexpected: {message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }

    let unchanged = placed(&admin, Some(room));
    assert!(
        unchanged.epoch > entered.epoch,
        "a refused move must not have moved anyone"
    );
    admin.shutdown();
}

#[test]
fn deleting_an_occupied_room_moves_its_occupants_to_no_room() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let room = created(&admin, "lounge");
    placed(&admin, Some(room));

    let mut events = admin.events();
    admin.command(Command::DeleteRoom(room)).unwrap();
    assert_eq!(next_event(&mut events), Event::RoomDeleted(room));

    let lobby = placed(&admin, None);
    assert_eq!(lobby.room, None);
    assert!(lobby.tracks.is_none());
    admin.shutdown();
}

#[test]
fn fifty_moves_leave_no_roster_divergence() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = admin(&server, &admin_dir);
    let user = enrolled(&server, &user_dir, &admin, Role::User);
    let watched = created(&admin, "watched");
    let elsewhere = created(&admin, "elsewhere");
    placed(&user, Some(watched));

    let mut watching = user.events();
    let mover = admin.welcome().me;
    let mut where_admin_is: Option<RoomId> = None;
    let mut transitions = 0;

    let mut draw: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..50 {
        draw = draw.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let target = match draw >> 62 {
            0 => None,
            1 => Some(elsewhere),
            _ => Some(watched),
        };
        if target == where_admin_is {
            continue;
        }
        placed(&admin, target);
        if where_admin_is == Some(watched) || target == Some(watched) {
            transitions += 1;
        }
        where_admin_is = target;
    }

    let mut present = false;
    for _ in 0..transitions {
        match next_event(&mut watching) {
            Event::UserEntered { room, peer, .. } => {
                assert_eq!((room, peer.user), (watched, mover));
                assert!(!present, "an entry arrived for a peer already in the room");
                present = true;
            }
            Event::UserExited { room, user, .. } => {
                assert_eq!((room, user), (watched, mover));
                assert!(
                    present,
                    "an exit arrived for a peer that was not in the room"
                );
                present = false;
            }
            other => panic!("expected a membership event, got {other:?}"),
        }
    }

    assert_eq!(
        present,
        where_admin_is == Some(watched),
        "the watching client's roster diverged from where the mover actually is"
    );
    user.shutdown();
    admin.shutdown();
}
