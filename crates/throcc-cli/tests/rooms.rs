mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use throcc_crypto::identity::encryption_key_message;
use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, Role, RoomId, UserId};
use throcc_proto::limits::{MAX_ROOM_MEMBERS, MAX_ROOM_SHARES, MAX_ROOMS};
use throcc_proto::messages::{
    ClientMessage, Codec, ErrorCode, Event, MAX_AVATAR_BYTES, MediaState, Presence, Request,
    Response, ServerMessage, SignedEncryptionKey, Welcome,
};
use throcc_server::rooms::{Handled, INACTIVE_ACCOUNT_LIFETIME, PasswordWork};
use throcc_server::unix_now;

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

/// Applies `client`'s events to `view` until it shows `expected`.
async fn see(client: &mut TestClient, view: &mut View, expected: &View) {
    if view != expected {
        client
            .event(|e| {
                view.apply(e);
                (view == expected).then_some(())
            })
            .await;
    }
}

/// Has `admin` create a room locked with "hunter2" and waits for `watcher` to hear of it.
async fn create_locked_room(admin: &TestClient, watcher: &mut TestClient) -> RoomId {
    let create = Request::CreateRoom {
        name: "Private".into(),
        password: Some("hunter2".into()),
    };
    assert_eq!(admin.request(create).await, Response::Ok);
    watcher
        .event(|e| match e {
            Event::RoomCreated(room) => {
                assert_eq!(room.name, "Private", "another room was created");
                assert!(room.locked);
                Some(room.id)
            }
            _ => None,
        })
        .await
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
            Event::PeersChanged { room, epoch, peers } if *room == general => {
                Some((*epoch, peers.len()))
            }
            _ => None,
        })
        .await;
    assert_eq!(bob_state, (carol_placed.epoch, 2));

    bob.join(Some(gaming)).await;
    let expected = View(BTreeMap::from([
        (bob_id, Some(gaming)),
        (carol_id, Some(general)),
    ]));
    for (client, view) in [&mut alice, &mut bob, &mut carol]
        .into_iter()
        .zip(views.iter_mut())
    {
        see(client, view, &expected).await;
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
        matches!(e, Event::RoomLockChanged { room, locked: true } if *room == general).then_some(())
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
    let placement = bob.set_room(Some(general), Some("hunter2")).await;
    assert!(matches!(placement, Response::Placement(p) if p.room == Some(general)));
    let placement = alice.set_room(Some(general), None).await;
    assert!(matches!(placement, Response::Placement(p) if p.room == Some(general)));

    bob.reconnect(&server.addr, Some(general)).await;
    let welcome = &bob.link.welcome;
    assert_eq!(welcome.placement.room, Some(general));
    assert!(welcome.rooms.iter().any(|r| r.id == general && r.locked));

    bob.join(None).await;
    bob.reconnect(&server.addr, Some(general)).await;
    assert_eq!(bob.link.welcome.placement.room, None);
}

#[tokio::test]
async fn a_room_created_with_a_password_is_locked_from_the_start() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let empty = Request::CreateRoom {
        name: "Empty".into(),
        password: Some(String::new()),
    };

    assert_error(&alice.request(empty).await, ErrorCode::Invalid);
    let private = create_locked_room(&alice, &mut bob).await;
    assert_error(
        &bob.set_room(Some(private), None).await,
        ErrorCode::WrongPassword,
    );
    let placement = bob.set_room(Some(private), Some("hunter2")).await;
    assert!(matches!(placement, Response::Placement(p) if p.room == Some(private)));

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
    let join = |password: &str| Request::JoinRoom {
        room: general,
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
    assert!(matches!(finish(joining), Response::Placement(p) if p.room == Some(general)));
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
    let first = ProcessLikeServer::start_temp();
    let addr = first.addr.to_string();
    let mut alice = enroll_at(&addr, &first.admin_invite()).await;
    let general = create_room(&mut alice, "General").await;
    let full = create_room(&mut alice, "Full").await;
    let _second = first.restart(|db| db.set_epochs(&[(full, Epoch(u32::MAX))]).unwrap());

    alice.reconnect(&addr, None).await;
    let epoch = alice.join(Some(general)).await.epoch;
    assert_error(&alice.set_room(Some(full), None).await, ErrorCode::Internal);
    assert_eq!(alice.join(Some(general)).await.epoch, epoch);
}

#[tokio::test]
async fn failed_set_room_leaves_the_client_in_place() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let placement = bob.join(Some(general)).await;
    let missing = bob.set_room(Some(RoomId(999)), None).await;
    assert_error(&missing, ErrorCode::NotFound);
    let again = bob.join(Some(general)).await;
    assert_eq!(again.tracks, placement.tracks);
}

#[tokio::test]
async fn reconnect_returns_to_the_wanted_room_with_fresh_tracks() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let first = bob.join(Some(general)).await;

    bob.reconnect(&server.addr, Some(general)).await;
    let placement = bob.link.welcome.placement;
    assert_eq!(placement.room, Some(general));
    assert!(placement.epoch > first.epoch);
    let (old, new) = (first.tracks.unwrap(), placement.tracks.unwrap());
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
        .request(Request::UploadAvatar(vec![0; MAX_AVATAR_BYTES + 1]))
        .await;
    assert_error(&big, ErrorCode::TooLarge);
    let empty_name = alice.request(profile("   ")).await;
    assert_error(&empty_name, ErrorCode::Invalid);
}

#[tokio::test]
async fn media_ids_never_repeat_across_a_restart() {
    let first = ProcessLikeServer::start_temp();
    let addr = first.addr.to_string();
    let mut alice = enroll_at(&addr, &first.admin_invite()).await;
    let general = create_room(&mut alice, "General").await;
    let before = alice.join(Some(general)).await.tracks.unwrap();
    let _second = first.restart(|_| {});

    alice.reconnect(&addr, Some(general)).await;
    let placement = &alice.link.welcome.placement;
    let after = placement.tracks.as_ref().unwrap();
    assert!(
        after.mic > before.shares[0].audio,
        "{before:?} then {after:?}"
    );
    assert!(placement.epoch > Epoch(1));
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
    const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
    let server = start_server_with(|config| config.idle_timeout = IDLE_TIMEOUT);
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
    tokio::time::timeout(IDLE_TIMEOUT * 3, async {
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
    // A keep-alive sent after the cut restarts the idle timer once, a third of the timeout in.
    assert!(
        elapsed <= IDLE_TIMEOUT * 4 / 3 + Duration::from_secs(1),
        "took {elapsed:?}"
    );
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
            tokio::spawn(
                async move { requester.request(Request::FetchAvatar(Hash([0; 32]))).await },
            )
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
        match alice.request(Request::UploadAvatar(picture)).await {
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
    let expired = SignedEncryptionKey {
        key: [1; 32],
        not_after: unix_now() - 1,
        sig: [2; 64],
    };
    server
        .shared
        .hub()
        .db
        .set_encryption_key(me, &expired)
        .unwrap();
    alice.reconnect(&server.addr, Some(general)).await;
    assert_eq!(alice.link.welcome.placement.room, None);
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
    let first = ProcessLikeServer::start_temp();
    let addr = first.addr.to_string();
    let mut alice = enroll_at(&addr, &first.admin_invite()).await;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited_at(&addr, &alice).await;
    let _second = first.restart(|db| db.set_epochs(&[(general, Epoch(u32::MAX - 1))]).unwrap());

    alice.reconnect(&addr, None).await;
    bob.reconnect(&addr, None).await;
    let alice_id = alice.link.welcome.me;
    assert_eq!(alice.join(Some(general)).await.epoch, Epoch(u32::MAX));
    bob.event(|e| matches!(e, Event::PresenceChanged(p) if p.user == alice_id).then_some(()))
        .await;

    alice.disconnect();
    bob.event(|e| matches!(e, Event::PresenceEnded(u) if *u == alice_id).then_some(()))
        .await;
}

#[tokio::test]
async fn a_full_room_takes_nobody_until_someone_leaves() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let room = create_room(&mut alice, "General").await;
    alice.join(Some(room)).await;
    let mut members = Vec::new();
    for _ in 1..MAX_ROOM_MEMBERS {
        let member = invited(&server, &alice).await;
        member.join(Some(room)).await;
        members.push(member);
    }
    let late = invited(&server, &alice).await;
    assert_error(&late.set_room(Some(room), None).await, ErrorCode::Full);

    // A member who drops out finds their place taken when they come back.
    let mut dropped = members.pop().unwrap();
    dropped.disconnect();
    wait_for("the dropped member's place to free up", || {
        server
            .shared
            .hub()
            .conn_id(dropped.link.welcome.me)
            .is_none()
    })
    .await;
    late.join(Some(room)).await;
    dropped.reconnect(&server.addr, Some(room)).await;
    assert_eq!(dropped.link.welcome.placement.room, None);
}

#[tokio::test]
async fn a_room_runs_a_bounded_number_of_shares() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let room = create_room(&mut alice, "General").await;
    let share = share_1080p60(Codec::H264, 8000);
    let mut sharers = Vec::new();
    for _ in 0..MAX_ROOM_SHARES {
        let sharer = invited(&server, &alice).await;
        sharer.join(Some(room)).await;
        assert_eq!(
            sharer.request(Request::SetMedia(sharing(share))).await,
            Response::Ok
        );
        sharers.push(sharer);
    }
    let late = invited(&server, &alice).await;
    late.join(Some(room)).await;
    assert_error(
        &late.request(Request::SetMedia(sharing(share))).await,
        ErrorCode::Full,
    );

    // A running share may still change, and a stopped one makes room.
    let sharper = share_1080p60(Codec::H264, 12000);
    assert_eq!(
        sharers[0]
            .request(Request::SetMedia(sharing(sharper)))
            .await,
        Response::Ok
    );
    let stopped = Request::SetMedia(MediaState::default());
    assert_eq!(sharers[0].request(stopped).await, Response::Ok);
    assert_eq!(
        late.request(Request::SetMedia(sharing(share))).await,
        Response::Ok
    );
}

#[tokio::test]
async fn accounts_unseen_for_a_year_are_removed() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let room = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    let carol = invited(&server, &alice).await;
    let (bob_id, carol_id) = (bob.link.welcome.me, carol.link.welcome.me);
    let dave = invited(&server, &carol).await;
    // Presence shows people in rooms, so theirs shows when they go.
    bob.join(Some(room)).await;
    carol.join(Some(room)).await;
    carol.disconnect();
    alice
        .event(|e| matches!(e, Event::PresenceEnded(u) if *u == carol_id).then_some(()))
        .await;
    let long_ago = unix_now() - INACTIVE_ACCOUNT_LIFETIME.as_secs() - 60;
    {
        let hub = server.shared.hub();
        hub.db.mark_seen(bob_id, long_ago).unwrap();
        hub.db.mark_seen(carol_id, long_ago).unwrap();
    }
    // Signing in again counts as being seen.
    bob.reconnect(&server.addr, Some(room)).await;
    alice
        .event(|e| matches!(e, Event::PresenceChanged(p) if p.user == bob_id).then_some(()))
        .await;
    bob.disconnect();
    alice
        .event(|e| matches!(e, Event::PresenceEnded(u) if *u == bob_id).then_some(()))
        .await;

    server.shared.hub().remove_inactive(unix_now()).unwrap();
    alice
        .event(|e| matches!(e, Event::UserRemoved(u) if *u == carol_id).then_some(()))
        .await;
    let Response::Users(users) = alice.request(Request::ListUsers).await else {
        panic!("expected the user list");
    };
    let ids: Vec<UserId> = users.iter().map(|u| u.id).collect();
    assert!(ids.contains(&bob_id), "bob signed in again: {ids:?}");
    assert!(
        !ids.contains(&carol_id),
        "carol was unseen for a year: {ids:?}"
    );
    assert!(
        ids.contains(&dave.link.welcome.me),
        "who carol invited stays: {ids:?}"
    );
}

#[tokio::test]
async fn a_server_holds_a_bounded_number_of_rooms() {
    let first = ProcessLikeServer::start_temp();
    let addr = first.addr.to_string();
    let mut alice = enroll_at(&addr, &first.admin_invite()).await;
    let _second = first.restart(|db| {
        for i in 0..MAX_ROOMS {
            db.create_room(&format!("Room {i}"), None).unwrap();
        }
    });

    alice.reconnect(&addr, None).await;
    assert_eq!(alice.link.welcome.rooms.len(), MAX_ROOMS);
    let res = alice
        .request(Request::CreateRoom {
            name: "One more".into(),
            password: None,
        })
        .await;
    assert_error(&res, ErrorCode::TooLarge);
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
    bob.event(|e| matches!(e, Event::RemovedFromServer { .. }).then_some(()))
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
    let Response::AvatarHash(hash) = alice.request(Request::UploadAvatar(picture)).await else {
        panic!("upload refused");
    };
    let code = invite(&alice).await;
    let mut bob = raw_enroll(&server.addr, &code, small_window_transport(16 * 1024)).await;
    let bob_id = bob.welcome.me;

    let requests = (0..FETCHES)
        .map(|_| Request::FetchAvatar(hash))
        .chain([Request::CreateInvite]);
    for (id, request) in (0..).zip(requests) {
        let message = ClientMessage::Request { id, request };
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
                response: Response::Avatar(_),
                ..
            } => pictures += 1,
            ServerMessage::Response { id, response } if id == FETCHES => {
                assert!(matches!(response, Response::Invite { .. }), "{response:?}");
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

/// `client`'s encryption key replaced by `key`, signed for the server it is connected to.
fn signed_key(client: &TestClient, key: [u8; 32]) -> SignedEncryptionKey {
    let server = client.link.connected.fingerprint;
    let not_after = unix_now() + 24 * 60 * 60;
    let message = encryption_key_message(&server.0, client.link.welcome.me, not_after, &key);
    SignedEncryptionKey {
        key,
        not_after,
        sig: client.keystore.identity().sign(&message),
    }
}

#[tokio::test]
async fn only_a_new_encryption_key_rekeys_the_room_and_new_keys_are_rate_limited() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let epoch = alice.join(Some(general)).await.epoch;
    let current = alice
        .keystore
        .signed_encryption_key(&alice.link.connected.fingerprint, alice.link.welcome.me);
    for _ in 0..10 {
        assert_eq!(
            alice.request(Request::SetEncryptionKey(current)).await,
            Response::Ok
        );
    }
    assert_eq!(alice.join(Some(general)).await.epoch, epoch);

    for (i, key) in [[1; 32], [2; 32], [3; 32]].into_iter().enumerate() {
        let set = Request::SetEncryptionKey(signed_key(&alice, key));
        assert_eq!(alice.request(set).await, Response::Ok);
        let rekeyed = Epoch(epoch.0 + 1 + i as u32);
        assert_eq!(alice.join(Some(general)).await.epoch, rekeyed);
    }
    let fourth = Request::SetEncryptionKey(signed_key(&alice, [4; 32]));
    assert_error(&alice.request(fourth).await, ErrorCode::RateLimited);
}

#[tokio::test]
async fn hopping_between_rooms_is_rate_limited_but_a_reconnect_returns_to_the_last_room() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let one = create_room(&mut alice, "One").await;
    let two = create_room(&mut alice, "Two").await;
    let (mut hops, mut last) = (0, None);
    let refused = loop {
        let room = if hops % 2 == 0 { one } else { two };
        match alice.set_room(Some(room), None).await {
            Response::Placement(_) if hops < 100 => (hops, last) = (hops + 1, Some(room)),
            other => break other,
        }
    };
    assert_error(&refused, ErrorCode::RateLimited);
    assert_eq!(hops, 5);
    for _ in 0..3 {
        alice.reconnect(&server.addr, last).await;
        assert_eq!(alice.link.welcome.placement.room, last);
    }
    // Last, since moving to the lobby changes the room a reconnect returns to.
    assert_eq!(alice.join(None).await.room, None);
}

#[tokio::test]
async fn reconnects_return_to_the_room_left_only_so_often() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    alice.join(Some(general)).await;
    for _ in 0..10 {
        alice.reconnect(&server.addr, Some(general)).await;
        assert_eq!(alice.link.welcome.placement.room, Some(general));
    }
    alice.reconnect(&server.addr, Some(general)).await;
    assert_eq!(alice.link.welcome.placement.room, None);
}

#[tokio::test]
async fn a_reconnect_lands_in_the_lobby_once_the_room_left_is_locked_or_deleted() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let locked = create_room(&mut alice, "Locked").await;
    let deleted = create_room(&mut alice, "Deleted").await;
    let mut bob = invited(&server, &alice).await;
    for room in [locked, deleted] {
        bob.join(Some(room)).await;
        bob.disconnect();
        let change = if room == locked {
            Request::SetRoomPassword {
                room,
                password: Some("hunter2".into()),
            }
        } else {
            Request::DeleteRoom(room)
        };
        assert_eq!(alice.request(change).await, Response::Ok);
        bob.reconnect(&server.addr, Some(room)).await;
        assert_eq!(bob.link.welcome.placement.room, None, "{room:?}");
    }
}

#[tokio::test]
async fn a_reconnect_does_not_return_to_a_locked_room_one_was_kicked_from() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let private = create_locked_room(&alice, &mut bob).await;
    let joined = bob.set_room(Some(private), Some("hunter2")).await;
    assert!(matches!(joined, Response::Placement(_)), "{joined:?}");
    let kick = Request::KickFromRoom {
        user: bob.link.welcome.me,
        room: private,
    };
    assert_eq!(alice.request(kick).await, Response::Ok);
    bob.reconnect(&server.addr, Some(private)).await;
    assert_eq!(bob.link.welcome.placement.room, None);
}

/// Sends requests until the server refuses one, leaving `client` no allowance.
async fn use_up_requests(client: &TestClient) {
    for _ in 0..1000 {
        if let Response::Err {
            code: ErrorCode::RateLimited,
            ..
        } = client.request(Request::ListUsers).await
        {
            return;
        }
    }
    panic!("requests were never rate-limited");
}

#[tokio::test]
async fn sender_keys_for_the_current_epoch_need_no_request_allowance() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    alice.join(Some(general)).await;
    let epoch = bob.join(Some(general)).await.epoch;
    use_up_requests(&bob).await;

    let publish = || Request::PublishSenderKey {
        epoch,
        wrapped: vec![(alice.link.welcome.me, vec![7; 32])],
    };
    assert_eq!(bob.request(publish()).await, Response::Ok);
    assert_eq!(bob.request(publish()).await, Response::Ok);
    // A refused request can leave almost a whole request of allowance, which may refill
    // before the publish arrives. Then the publish spends it, and nothing is left for the next
    // request; a free third publish would leave it.
    let mut refused = false;
    for _ in 0..5 {
        use_up_requests(&bob).await;
        match bob.request(publish()).await {
            Response::Ok => assert_error(
                &bob.request(Request::ListUsers).await,
                ErrorCode::RateLimited,
            ),
            refusal => {
                assert_error(&refusal, ErrorCode::RateLimited);
                refused = true;
                break;
            }
        }
    }
    assert!(refused, "a third publish was never limited");
    let from = alice
        .event(|e| match e {
            Event::SenderKeyPublished { from, .. } => Some(*from),
            _ => None,
        })
        .await;
    assert_eq!(from, bob.link.welcome.me);
}

#[tokio::test]
async fn wrong_room_passwords_make_that_guesser_wait() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let carol = invited(&server, &alice).await;
    let private = create_locked_room(&alice, &mut bob).await;
    for _ in 0..5 {
        assert_error(
            &bob.set_room(Some(private), Some("guess")).await,
            ErrorCode::WrongPassword,
        );
    }
    let waiting = bob.set_room(Some(private), Some("hunter2")).await;
    assert!(
        matches!(&waiting, Response::Err { code: ErrorCode::RateLimited, message } if message.contains("wrong passwords")),
        "{waiting:?}"
    );
    let placement = carol.set_room(Some(private), Some("hunter2")).await;
    assert!(matches!(placement, Response::Placement(p) if p.room == Some(private)));
}

#[tokio::test]
async fn joins_waiting_for_the_password_workers_all_finish() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let carol = invited(&server, &alice).await;
    let dave = invited(&server, &alice).await;
    let erin = invited(&server, &alice).await;
    let private = create_locked_room(&alice, &mut bob).await;
    let (b, c, d, e) = tokio::join!(
        bob.set_room(Some(private), Some("hunter2")),
        carol.set_room(Some(private), Some("hunter2")),
        dave.set_room(Some(private), Some("hunter2")),
        erin.set_room(Some(private), Some("guess")),
    );
    for placement in [b, c, d] {
        assert!(matches!(placement, Response::Placement(p) if p.room == Some(private)));
    }
    assert_error(&e, ErrorCode::WrongPassword);
}

#[tokio::test]
async fn managers_cannot_delete_a_room_holding_someone_they_cannot_kick() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let general = create_room(&mut alice, "General").await;
    let bob = invited(&server, &alice).await;
    let carol = invited(&server, &alice).await;
    let promote = Request::SetRole {
        user: bob.link.welcome.me,
        role: Role::Manager,
    };
    assert_eq!(alice.request(promote).await, Response::Ok);
    alice.join(Some(general)).await;
    bob.join(Some(general)).await;

    let delete = Request::DeleteRoom(general);
    assert_error(&bob.request(delete.clone()).await, ErrorCode::Forbidden);
    assert_eq!(alice.join(None).await.room, None);
    carol.join(Some(general)).await;
    assert_eq!(bob.request(delete).await, Response::Ok);
    assert_eq!(bob.join(None).await.room, None);

    let lobby = create_room(&mut alice, "Lobby").await;
    bob.join(Some(lobby)).await;
    assert_eq!(
        alice.request(Request::DeleteRoom(lobby)).await,
        Response::Ok
    );
}

#[tokio::test]
async fn role_and_removal_requests_tell_non_admins_nothing_about_who_exists() {
    let server = start_server();
    let alice = admin(&server).await;
    let carol = invited(&server, &alice).await;
    let nobody = UserId(999);
    let set_role = Request::SetRole {
        user: nobody,
        role: Role::Manager,
    };
    let remove = Request::RemoveUser {
        user: nobody,
        cascade: true,
    };
    assert_error(&carol.request(set_role.clone()).await, ErrorCode::Forbidden);
    assert_error(&carol.request(remove).await, ErrorCode::Forbidden);
    assert_error(&alice.request(set_role).await, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_demoted_manager_needs_the_password_to_return_to_a_locked_room() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let bob_id = bob.link.welcome.me;
    let set_role = |role| Request::SetRole { user: bob_id, role };
    assert_eq!(alice.request(set_role(Role::Manager)).await, Response::Ok);
    let private = create_locked_room(&alice, &mut bob).await;
    assert_eq!(bob.join(Some(private)).await.room, Some(private));

    assert_eq!(alice.request(set_role(Role::User)).await, Response::Ok);
    bob.reconnect(&server.addr, Some(private)).await;
    assert_eq!(bob.link.welcome.placement.room, None);
}

#[tokio::test]
async fn share_starts_are_rate_limited_but_stops_and_the_mic_are_not() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let alice_id = alice.link.welcome.me;
    let general = create_room(&mut alice, "General").await;
    let mut bob = invited(&server, &alice).await;
    alice.join(Some(general)).await;
    bob.join(Some(general)).await;
    let share = sharing(share_1080p60(Codec::H264, 8000));
    let stop = MediaState::default();
    let mut starts = 0;
    let refused = loop {
        match alice.request(Request::SetMedia(share)).await {
            Response::Ok if starts < 100 => starts += 1,
            other => break other,
        }
        assert_eq!(alice.request(Request::SetMedia(stop)).await, Response::Ok);
    };
    assert_error(&refused, ErrorCode::RateLimited);
    assert_eq!(starts, 5);

    // The client still believes it is sharing, so its push-to-talk toggles carry the share
    // the server refused. The mic applies anyway, and the share only once there is room.
    let alice_presence = |mic: Option<bool>| {
        move |e: &Event| match e {
            Event::PresenceChanged(p) if p.user == alice_id && mic.is_none_or(|m| p.mic == m) => {
                Some(p.sharing)
            }
            _ => None,
        }
    };
    let ptt = |mic| Request::SetMedia(MediaState { mic, ..share });
    assert_error(&alice.request(ptt(true)).await, ErrorCode::RateLimited);
    assert!(!bob.event(alice_presence(Some(true))).await);
    let mut started = false;
    for mic in [false, true, false] {
        let response = alice.request(ptt(mic)).await;
        started |= response == Response::Ok;
        if !started {
            assert_error(&response, ErrorCode::RateLimited);
        }
        let seen = bob.event(alice_presence(None)).await;
        assert_eq!(seen, started);
    }

    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(alice.request(ptt(true)).await, Response::Ok);
    assert!(bob.event(alice_presence(Some(true))).await);
}
