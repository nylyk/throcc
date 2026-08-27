mod common;

use common::{SERVER_LABEL, TestServer, keystore, next_event};
use tempfile::TempDir;
use throcc_client_core::{Client, Command, Event};
use throcc_proto::{Role, RoomId};

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
