mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, Role, RoomId, UserId};
use throcc_proto::messages::{
    ClientMessage, ErrorCode, Event, MAX_AVATAR_BYTES, Presence, Request, Response, ServerMessage,
    SignedEncKey, Welcome,
};
use throcc_server::database::Database;
use throcc_server::rooms::{Handled, MAX_ROOMS, PasswordWork};
use throcc_server::{DATABASE_FILE, unix_now};

use common::*;

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

    let bob_placed = bob.join(Some(general)).await;
    assert_eq!(bob_placed.peers.len(), 1);
    let carol_placed = carol.join(Some(general)).await;
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

    bob.join(Some(gaming)).await;
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

    carol.disconnect();
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
async fn a_locked_room_needs_its_password_from_members_on_every_join() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let lock = |password: &str| Request::SetRoomPassword {
        room: general,
        password: Some(password.to_owned()),
    };

    assert_error(&bob.request(lock("mine")).await, ErrorCode::Forbidden);
    assert_eq!(alice.request(lock("hunter2")).await, Response::Ok);
    bob.event(|e| {
        matches!(e, Event::RoomLocked { room, locked: true } if *room == general).then_some(())
    })
    .await;
    assert_error(
        &bob.set_room(Some(general), None).await,
        ErrorCode::WrongPassword,
    );
    assert_error(
        &bob.set_room(Some(general), Some("hunter3")).await,
        ErrorCode::WrongPassword,
    );
    let placed = bob.set_room(Some(general), Some("hunter2")).await;
    assert!(matches!(placed, Response::Placed(p) if p.room == Some(general)));
    let placed = alice.set_room(Some(general), None).await;
    assert!(matches!(placed, Response::Placed(p) if p.room == Some(general)));

    bob.reconnect(&server.addr, Some(general)).await;
    let welcome = &bob.link.welcome;
    assert_eq!(welcome.placed.room, Some(general));
    assert!(welcome.rooms.iter().any(|r| r.id == general && r.locked));

    bob.join(None).await;
    bob.reconnect(&server.addr, Some(general)).await;
    assert_eq!(bob.link.welcome.placed.room, None);
}

#[tokio::test]
async fn a_room_created_with_a_password_is_locked_from_the_start() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let create = |name: &str, password: &str| Request::CreateRoom {
        name: name.into(),
        password: Some(password.to_owned()),
    };

    assert_error(
        &alice.request(create("Empty", "")).await,
        ErrorCode::Invalid,
    );
    assert_eq!(
        alice.request(create("Private", "hunter2")).await,
        Response::Ok
    );
    let private = bob
        .event(|e| match e {
            Event::RoomCreated(room) => {
                assert_eq!(room.name, "Private", "the refused room was created");
                assert!(room.locked);
                Some(room.id)
            }
            _ => None,
        })
        .await;
    assert_error(
        &bob.set_room(Some(private), None).await,
        ErrorCode::WrongPassword,
    );
    let placed = bob.set_room(Some(private), Some("hunter2")).await;
    assert!(matches!(placed, Response::Placed(p) if p.room == Some(private)));

    bob.reconnect(&server.addr, None).await;
    let rooms = &bob.link.welcome.rooms;
    assert!(rooms.iter().any(|r| r.id == private && r.locked));
}

#[tokio::test]
async fn a_password_checked_outside_the_hub_is_checked_again_before_it_counts() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let bob_id = bob.link.welcome.me;
    let lock = |password: Option<&str>| Request::SetRoomPassword {
        room: general,
        password: password.map(str::to_owned),
    };
    let join = |password: &str| Request::SetRoom {
        room: Some(general),
        password: Some(password.to_owned()),
    };
    // Bob's request up to its password work, then the rest, as his session would run them.
    let start = |request: Request| {
        let mut hub = server.shared.hub();
        let conn_id = hub.conn_id(bob_id).unwrap();
        match hub.handle(bob_id, conn_id, request) {
            Handled::Pending(work) => (conn_id, work),
            Handled::Done(res) => panic!("expected password work, got {res:?}"),
        }
    };
    let finish = |(conn_id, work): (u64, PasswordWork)| {
        let done = work.run();
        server.shared.hub().resume(bob_id, conn_id, done)
    };

    assert_eq!(alice.request(lock(Some("hunter2"))).await, Response::Ok);
    let joining = start(join("hunter2"));
    assert_eq!(alice.request(lock(Some("hunter3"))).await, Response::Ok);
    assert_error(&finish(joining), ErrorCode::WrongPassword);

    let joining = start(join("hunter3"));
    assert_eq!(alice.request(lock(None)).await, Response::Ok);
    assert!(matches!(finish(joining), Response::Placed(p) if p.room == Some(general)));
    bob.join(None).await;

    assert_eq!(alice.request(lock(Some("hunter4"))).await, Response::Ok);
    let joining = start(join("hunter4"));
    bob.reconnect(&server.addr, None).await;
    assert_error(&finish(joining), ErrorCode::Forbidden);

    let joining = start(join("hunter4"));
    assert_eq!(
        alice.request(Request::DeleteRoom(general)).await,
        Response::Ok
    );
    assert_error(&finish(joining), ErrorCode::NotFound);

    let promote = |role| Request::SetRole { user: bob_id, role };
    assert_eq!(alice.request(promote(Role::Manager)).await, Response::Ok);
    let creating = start(Request::CreateRoom {
        name: "Private".into(),
        password: Some("hunter5".into()),
    });
    assert_eq!(alice.request(promote(Role::User)).await, Response::Ok);
    assert_error(&finish(creating), ErrorCode::Forbidden);
}

#[tokio::test]
async fn a_join_that_cannot_bump_the_new_rooms_epoch_leaves_the_old_room_alone() {
    let dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(dir.path(), "127.0.0.1:0".parse().unwrap());
    let addr = first.addr;
    let mut alice = enroll_at(&addr.to_string(), first.bootstrap.as_ref().unwrap()).await;
    let general = create_room(&mut alice, "General").await;
    let full = create_room(&mut alice, "Full").await;
    first.stop();
    Database::open(&dir.path().join(DATABASE_FILE))
        .unwrap()
        .set_epoch(full, Epoch(u32::MAX))
        .unwrap();

    let second = ProcessLikeServer::start(dir.path(), addr);
    alice.reconnect(&addr.to_string(), None).await;
    let epoch = alice.join(Some(general)).await.epoch;
    assert_error(&alice.set_room(Some(full), None).await, ErrorCode::Internal);
    assert_eq!(alice.join(Some(general)).await.epoch, epoch);
    second.stop();
}

#[tokio::test]
async fn failed_set_room_leaves_the_client_in_place() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let placed = bob.join(Some(general)).await;
    let missing = bob.set_room(Some(RoomId(999)), None).await;
    assert_error(&missing, ErrorCode::NotFound);
    let again = bob.join(Some(general)).await;
    assert_eq!(again.tracks, placed.tracks);
}

#[tokio::test]
async fn reconnect_returns_to_the_wanted_room_with_fresh_tracks() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let first = bob.join(Some(general)).await;

    bob.reconnect(&server.addr, Some(general)).await;
    let placed = bob.link.welcome.placed;
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

#[tokio::test]
async fn media_ids_never_repeat_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(dir.path(), "127.0.0.1:0".parse().unwrap());
    let addr = first.addr;
    let mut alice = enroll_at(&addr.to_string(), first.bootstrap.as_ref().unwrap()).await;
    let general = create_room(&mut alice, "General").await;
    let before = alice.join(Some(general)).await.tracks.unwrap();
    first.stop();

    let second = ProcessLikeServer::start(dir.path(), addr);
    assert!(second.bootstrap.is_none());
    alice.reconnect(&addr.to_string(), Some(general)).await;
    let placed = &alice.link.welcome.placed;
    let after = placed.tracks.as_ref().unwrap();
    assert!(
        after.mic > before.shares[0].audio,
        "{before:?} then {after:?}"
    );
    assert!(placed.epoch > Epoch(1));
    second.stop();
}

/// A relay that forwards both ways until the returned flag is set.
async fn cuttable_relay(server: &str) -> (String, Arc<AtomicBool>) {
    let cut = Arc::new(AtomicBool::new(false));
    let relay_cut = cut.clone();
    let addr = relay(server, move |_, _| !relay_cut.load(Ordering::Relaxed)).await;
    (addr, cut)
}

#[tokio::test]
async fn killed_connection_retracts_presence_within_the_idle_timeout() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let code = invite(&alice).await;

    let (relay, cut) = cuttable_relay(&server.addr).await;
    let bob = enroll_at(&relay, &code).await;
    let bob_id = bob.link.welcome.me;
    bob.join(Some(general)).await;
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

#[tokio::test]
async fn requests_are_rate_limited_per_user() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut limited = 0;
    for _ in 0..150 {
        let res = alice.request(Request::ListUsers).await;
        if matches!(&res, Response::Err { code, .. } if *code == ErrorCode::RateLimited) {
            limited += 1;
        }
    }
    assert!(limited > 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        alice.request(Request::ListUsers).await,
        Response::Users(_)
    ));
}

#[tokio::test]
async fn picture_fetches_have_their_own_rate_limit() {
    let server = start_server();
    let alice = admin(&server).await;
    let fetches: Vec<_> = (0..4000)
        .map(|_| {
            let requester = alice.link.requester.clone();
            tokio::spawn(async move { requester.request(Request::GetAvatar(Hash([0; 32]))).await })
        })
        .collect();
    let mut limited = 0;
    for fetch in fetches {
        let res = fetch.await.unwrap().unwrap();
        if matches!(&res, Response::Err { code, .. } if *code == ErrorCode::RateLimited) {
            limited += 1;
        }
    }
    assert!(limited > 0);
    assert!(matches!(
        alice.request(Request::ListUsers).await,
        Response::Users(_)
    ));
}

#[tokio::test]
async fn only_admins_hear_about_new_users_and_role_changes() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let mut manager = invited(&server, &alice).await;
    let manager_id = manager.link.welcome.me;
    let set_role = |user, role| Request::SetRole { user, role };
    assert_eq!(
        alice.request(set_role(manager_id, Role::Manager)).await,
        Response::Ok
    );
    manager
        .event(|e| {
            matches!(e, Event::RoleChanged { user, .. } if *user == manager_id).then_some(())
        })
        .await;

    let mut carol = invited(&server, &alice).await;
    let carol_id = carol.link.welcome.me;
    alice
        .event(|e| matches!(e, Event::UserAdded(u) if u.id == carol_id).then_some(()))
        .await;
    assert_eq!(
        alice.request(set_role(carol_id, Role::Manager)).await,
        Response::Ok
    );
    for client in [&mut alice, &mut carol] {
        client
            .event(|e| {
                matches!(e, Event::RoleChanged { user, .. } if *user == carol_id).then_some(())
            })
            .await;
    }
    create_room(&mut alice, "Marker").await;
    manager
        .event(|e| match e {
            Event::UserAdded(_) => panic!("a manager heard about a new user"),
            Event::RoleChanged { user, .. } if *user == carol_id => {
                panic!("a manager heard about someone else's role")
            }
            Event::RoomCreated(room) if room.name == "Marker" => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test]
async fn a_member_who_disconnects_leaves_no_routes_behind() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let alice_mic = alice.join(Some(general)).await.tracks.unwrap().mic;
    let bob_mic = bob.join(Some(general)).await.tracks.unwrap().mic;
    let routes = || server.shared.sfu.table.load();
    assert_eq!(routes().routes[&alice_mic].subscribers.len(), 1);
    assert!(routes().routes.contains_key(&bob_mic));

    bob.disconnect();
    wait_for("the departed member's routes to go", || {
        let table = routes();
        !table.routes.contains_key(&bob_mic) && table.routes[&alice_mic].subscribers.is_empty()
    })
    .await;
}

#[tokio::test]
async fn picture_uploads_are_rate_limited() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut uploaded = 0;
    let last = loop {
        let picture = vec![uploaded as u8; MAX_AVATAR_BYTES];
        match alice.request(Request::SetAvatar(picture)).await {
            Response::AvatarHash(_) if uploaded < 100 => uploaded += 1,
            other => break other,
        }
    };
    assert_error(&last, ErrorCode::RateLimited);
    assert!((16..=17).contains(&uploaded), "{uploaded}");
}

#[tokio::test]
async fn an_expired_encryption_key_cannot_enter_a_room() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let me = alice.link.welcome.me;
    let expired = SignedEncKey {
        key: [1; 32],
        not_after: unix_now() - 1,
        sig: [2; 64],
    };
    server.shared.hub().db.set_enc_key(me, &expired).unwrap();
    alice.reconnect(&server.addr, Some(general)).await;
    assert_eq!(alice.link.welcome.placed.room, None);
}

#[tokio::test]
async fn exhausted_media_ids_refuse_joins_but_keep_the_server_up() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    server
        .shared
        .hub()
        .db
        .allocate_media_ids(u32::MAX - 1)
        .unwrap()
        .unwrap();
    let res = alice.set_room(Some(general), None).await;
    assert_error(&res, ErrorCode::Internal);
    assert!(matches!(
        alice.request(Request::ListUsers).await,
        Response::Users(_)
    ));
}

#[tokio::test]
async fn a_leave_that_cannot_bump_the_epoch_still_ends_presence() {
    let dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(dir.path(), "127.0.0.1:0".parse().unwrap());
    let addr = first.addr;
    let mut alice = enroll_at(&addr.to_string(), first.bootstrap.as_ref().unwrap()).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited_at(&addr.to_string(), &alice).await;
    first.stop();
    Database::open(&dir.path().join(DATABASE_FILE))
        .unwrap()
        .set_epoch(general, Epoch(u32::MAX - 1))
        .unwrap();

    let second = ProcessLikeServer::start(dir.path(), addr);
    alice.reconnect(&addr.to_string(), None).await;
    bob.reconnect(&addr.to_string(), None).await;
    let alice_id = alice.link.welcome.me;
    assert_eq!(alice.join(Some(general)).await.epoch, Epoch(u32::MAX));
    bob.event(|e| matches!(e, Event::PresenceChanged(p) if p.user == alice_id).then_some(()))
        .await;

    alice.disconnect();
    bob.event(|e| matches!(e, Event::PresenceEnded(u) if *u == alice_id).then_some(()))
        .await;
    second.stop();
}

#[tokio::test]
async fn a_server_holds_a_bounded_number_of_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let first = ProcessLikeServer::start(dir.path(), "127.0.0.1:0".parse().unwrap());
    let addr = first.addr;
    let mut alice = enroll_at(&addr.to_string(), first.bootstrap.as_ref().unwrap()).await;
    first.stop();
    let db = Database::open(&dir.path().join(DATABASE_FILE)).unwrap();
    for i in 0..MAX_ROOMS {
        db.create_room(&format!("Room {i}"), None).unwrap();
    }
    drop(db);

    let second = ProcessLikeServer::start(dir.path(), addr);
    alice.reconnect(&addr.to_string(), None).await;
    assert_eq!(alice.link.welcome.rooms.len(), MAX_ROOMS);
    let res = alice
        .request(Request::CreateRoom {
            name: "One more".into(),
            password: None,
        })
        .await;
    assert_error(&res, ErrorCode::TooLarge);
    second.stop();
}

#[tokio::test]
async fn members_leave_on_their_own_but_the_only_admin_cannot() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let carol = invited(&server, &bob).await;
    let leave = |user| Request::RemoveUser {
        user,
        cascade: false,
    };
    let alice_id = alice.link.welcome.me;
    assert_error(&alice.request(leave(alice_id)).await, ErrorCode::Forbidden);

    let bob_id = bob.link.welcome.me;
    let carol_id = carol.link.welcome.me;
    for other in [alice_id, carol_id] {
        assert_error(&bob.request(leave(other)).await, ErrorCode::Forbidden);
    }
    let cascade = Request::RemoveUser {
        user: bob_id,
        cascade: true,
    };
    assert_error(&bob.request(cascade).await, ErrorCode::Forbidden);
    invite(&bob).await;

    bob.link.requester.request(leave(bob_id)).await.ok();
    bob.event(|e| matches!(e, Event::Kicked { .. }).then_some(()))
        .await;
    let Response::Users(users) = alice.request(Request::ListUsers).await else {
        panic!("no user list");
    };
    assert!(users.iter().all(|u| u.id != bob_id));
    let carol_row = users.iter().find(|u| u.id == carol_id).unwrap();
    assert_eq!((carol_row.role, carol_row.invited_by), (Role::User, None));
    let now = unix_now();
    let bobs_invites = server
        .shared
        .hub()
        .db
        .invites_used(bob_id, now, now)
        .unwrap();
    assert_eq!(bobs_invites, 0);
}

#[tokio::test]
async fn a_member_who_stops_reading_pictures_stops_being_read() {
    const FETCHES: u32 = 40;
    let server = start_server();
    let alice = admin(&server).await;
    let picture: Vec<u8> = (0..MAX_AVATAR_BYTES).map(|i| (i % 251) as u8).collect();
    let Response::AvatarHash(hash) = alice.request(Request::SetAvatar(picture)).await else {
        panic!("upload refused");
    };
    let code = invite(&alice).await;
    let mut bob = raw_enroll(&server.addr, &code, small_window_transport(16 * 1024)).await;
    let bob_id = bob.welcome.me;

    let requests = (0..FETCHES)
        .map(|_| Request::GetAvatar(hash))
        .chain([Request::CreateInvite]);
    for (id, req) in (0..).zip(requests) {
        let message = ClientMessage::Request { id, req };
        bob.send
            .write_all(&framing::encode(&message).unwrap())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let invites = || {
        let now = unix_now();
        server
            .shared
            .hub()
            .db
            .invites_used(bob_id, now, now)
            .unwrap()
    };
    assert_eq!(invites(), 0);

    let mut pictures = 0;
    loop {
        match read_message(&mut bob.recv).await {
            ServerMessage::Response {
                res: Response::Avatar(_),
                ..
            } => pictures += 1,
            ServerMessage::Response { id, res } if id == FETCHES => {
                assert!(matches!(res, Response::Invite { .. }), "{res:?}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(pictures, FETCHES);
    assert_eq!(invites(), 1);
}

#[tokio::test]
async fn managers_kick_lower_ranks_out_of_rooms() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let mut carol = invited(&server, &alice).await;
    let (alice_id, bob_id, carol_id) = (
        alice.link.welcome.me,
        bob.link.welcome.me,
        carol.link.welcome.me,
    );
    let kick = |user| Request::KickFromRoom {
        user,
        room: general,
    };
    let promote = Request::SetRole {
        user: bob_id,
        role: Role::Manager,
    };
    assert_eq!(alice.request(promote).await, Response::Ok);
    alice.join(Some(general)).await;
    bob.join(Some(general)).await;
    carol.join(Some(general)).await;

    assert_error(&carol.request(kick(bob_id)).await, ErrorCode::Forbidden);
    assert_error(&bob.request(kick(alice_id)).await, ErrorCode::Forbidden);
    assert_eq!(bob.request(kick(carol_id)).await, Response::Ok);
    let by = carol
        .event(|e| match e {
            Event::KickedFromRoom { by, room } => Some((*by, *room)),
            _ => None,
        })
        .await;
    assert_eq!(by, (bob_id, general));
    assert_error(&bob.request(kick(carol_id)).await, ErrorCode::NotFound);
    assert_eq!(carol.join(Some(general)).await.room, Some(general));
    let elsewhere = Request::KickFromRoom {
        user: carol_id,
        room: RoomId(999),
    };
    assert_error(&bob.request(elsewhere).await, ErrorCode::NotFound);
}
