mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use throcc_client_core::account;
use throcc_client_core::blocked::BlockedStore;
use throcc_client_core::call::{Call, CallEvent};
use throcc_client_core::client::{Client, ClientApi, UiCommand, UiEvent};
use throcc_client_core::keystore::Keystore;
use throcc_client_core::profile::Profile;
use throcc_client_core::state::{
    BLOCKED_NAME, ConnectionStatus, KeyWarning, Sound, SoundLevel, State,
};
use throcc_client_core::verified::VerifiedStore;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    Codec, Event as ServerEvent, MediaState, PublicKey, Request, Response,
};
use throcc_server::identity::KEY_FILE;
use tokio::runtime::Runtime;

use common::{ProcessLikeServer, client_as, named, share_1080p60, sharing, wait_for_state_within};

/// A server's address and an admin invite. It runs until the returned handle drops.
fn server() -> (String, InviteCode, ProcessLikeServer) {
    let server = ProcessLikeServer::start_temp();
    let admin_invite = server.admin_invite();
    (server.addr.to_string(), admin_invite, server)
}

fn client(address: &str, invite: &InviteCode) -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (client_in(dir.path(), address, Some(invite)), dir)
}

/// A client named Ilse keeping its keys in `dir`, enrolling with `invite` if given.
fn client_in(dir: &Path, address: &str, invite: Option<&InviteCode>) -> Client {
    client_as(dir, address, invite, named("Ilse"))
}

fn wait_for(client: &Client, what: &str, ready: impl FnMut(&State) -> bool) {
    wait_for_state_within(Duration::from_secs(5), client, what, ready);
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
        let link = account::enroll(&mut keystore, address, invite, "Ilse")
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
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
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

fn verify(user: UserId, key: PublicKey) -> UiCommand {
    UiCommand::SetVerified {
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
    command: UiCommand,
    what: &str,
    mut pick: impl FnMut(UiEvent) -> Option<T>,
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
    command_then(client, UiCommand::CreateInvite, "no invite", |e| match e {
        UiEvent::Invite { code, .. } => Some(code.parse().unwrap()),
        _ => None,
    })
}

fn create_room(client: &Client) -> RoomId {
    client.command(UiCommand::CreateRoom("General".into(), None));
    wait_for(client, "the room", |s| !s.rooms.is_empty());
    client.state().rooms[0].id
}

#[test]
fn mic_and_deafen_set_outside_a_room_carry_into_it() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    wait_connected(&bob, "bob connected");
    let bob_id = bob.state().me.unwrap();

    bob.command(UiCommand::SetDeafened(true));
    wait_for(&bob, "bob deafened in the lobby", |s| s.deafened && !s.mic);
    bob.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "bob in the room, deafened", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.room == Some(room) && p.deafened && !p.mic)
    });

    bob.command(UiCommand::Join(None));
    bob.command(UiCommand::SetDeafened(false));
    bob.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "bob back in the room, hearing and talking", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.room == Some(room) && !p.deafened && p.mic)
    });
}

#[test]
fn two_clients_see_each_others_presence_media_and_safety_number() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);

    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let bob_id = bob.state().me.unwrap();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    wait_for(&alice, "bob's mic on", |s| {
        s.person(bob_id).is_some_and(|p| p.mic)
    });
    bob.command(UiCommand::SetMic(false));
    wait_for(&alice, "bob's mic off", |s| {
        s.person(bob_id).is_some_and(|p| !p.mic)
    });
    wait_for(&bob, "bob's own state", |s| !s.mic);

    bob.command(UiCommand::SetDeafened(true));
    wait_for(&alice, "bob deafened and muted", |s| {
        s.person(bob_id).is_some_and(|p| p.deafened && !p.mic)
    });

    let alice_id = alice.state().me.unwrap();
    assert_eq!(
        bob.safety_number(&key_of(&bob, alice_id)),
        alice.safety_number(&key_of(&alice, bob_id))
    );

    bob.command(UiCommand::Disconnect);
    wait_for(&alice, "bob gone", |s| s.person(bob_id).is_none());
}

#[test]
fn a_changed_profile_reaches_others_and_the_next_session_renames_alone() {
    let (address, admin_invite, _server) = server();
    let alice_dir = tempfile::tempdir().unwrap();
    let alice = client_in(alice_dir.path(), &address, Some(&admin_invite));
    wait_connected(&alice, "connected");
    let picture: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
    let with_picture = Profile {
        picture: Some(Arc::from(picture.clone())),
        ..named("Ilse")
    };
    let saved = UiCommand::SetProfile(with_picture.clone());
    command_then(&alice, saved, "the profile wasn't saved", |e| {
        matches!(e, UiEvent::ProfileSaved).then_some(())
    });
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice's avatar hash", |s| {
        s.person(alice_id).is_some_and(|p| p.avatar.is_some())
    });
    let hash = bob.state().person(alice_id).unwrap().avatar.unwrap();
    wait_for(&bob, "alice's picture", |_| bob.avatar(hash).is_some());
    assert_eq!(&*bob.avatar(hash).unwrap(), picture.as_slice());

    // The server takes one profile change per session every 10 s, so rename in a new one.
    drop(alice);
    let renamed = Profile {
        name: "Alice".into(),
        ..with_picture
    };
    let alice = client_as(alice_dir.path(), &address, None, renamed);
    alice.command(UiCommand::Join(Some(room)));
    wait_for(&bob, "alice's new name", |s| {
        s.person(alice_id)
            .is_some_and(|p| p.name == "Alice" && p.avatar == Some(hash))
    });
}

#[test]
fn a_profile_saved_again_too_soon_reaches_others_once_the_server_allows_it() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let saved = UiCommand::SetProfile(named("Mara"));
    command_then(&alice, saved, "the first profile wasn't saved", |e| {
        matches!(e, UiEvent::ProfileSaved).then_some(())
    });
    let room = create_room(&alice);
    let (bob, _b) = client(&address, &invite(&alice));
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice's first name", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Mara")
    });

    let mut events = alice.events();
    alice.command(UiCommand::SetProfile(named("Alice")));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match events.try_recv() {
            Ok(UiEvent::ProfileSaved) => break,
            Ok(UiEvent::ProfileRefused(reason)) => panic!("the second save was refused: {reason}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "the second profile wasn't saved");
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_for(&bob, "alice's second name", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Alice")
    });
}

#[test]
fn a_profile_the_app_keeps_mid_session_reaches_the_server() {
    let (address, admin_invite, _server) = server();
    let dir = tempfile::tempdir().unwrap();
    let keystore = Keystore::open_in(dir.path()).unwrap();
    let verified = VerifiedStore::load(dir.path()).unwrap();
    let blocked = BlockedStore::load(dir.path()).unwrap();
    let (kept, profile) = tokio::sync::watch::channel(named("Ilse"));
    let alice = Client::connect(
        &address,
        keystore,
        Some(&admin_invite),
        profile,
        None,
        verified,
        blocked,
    )
    .unwrap();
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let (bob, _b) = client(&address, &invite(&alice));
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Ilse")
    });

    kept.send_replace(named("Mara"));
    wait_for(&bob, "alice's kept name", |s| {
        s.person(alice_id).is_some_and(|p| p.name == "Mara")
    });
}

#[test]
fn a_share_started_mid_call_shows_and_stays_while_the_mic_changes() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());

    let runtime = Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bob = call_in(&runtime, dir.path(), &address, &invite(&alice), room);
    let bob_id = bob.me();
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

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
        let Ok(UiEvent::State(state)) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let bob = state.person(bob_id).unwrap();
        assert_eq!(bob.sharing, Some(share), "bob shown as not sharing");
        if bob.mic {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(alice.state().person(bob_id).unwrap().sharing, Some(share));
}

#[test]
fn a_request_too_large_to_send_fails_alone() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let command = UiCommand::SetServerProfile {
        name: None,
        picture: Some(Some(vec![0; 2 << 20])),
    };
    command_then(&alice, command, "no refusal", |e| match e {
        UiEvent::ServerProfileRefused(_) => Some(()),
        _ => None,
    });
    invite(&alice);
}

#[test]
fn a_dropped_client_leaves_at_once() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    let bob_id = bob.state().me.unwrap();
    bob.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "bob in the room", |s| {
        s.person(bob_id).is_some_and(|p| p.room == Some(room))
    });

    drop(bob);
    wait_for(&alice, "bob gone well before the idle timeout", |s| {
        s.person(bob_id).is_none()
    });
}

#[test]
fn a_new_server_gets_our_name_at_enrollment_and_our_picture_once_connected() {
    let (address, admin_invite, _server) = server();
    let dir = tempfile::tempdir().unwrap();
    let picture: Vec<u8> = (0..2000u32).map(|i| (i * 3) as u8).collect();
    let profile = Profile {
        picture: Some(Arc::from(picture.clone())),
        ..named("Mara")
    };
    let alice = client_as(dir.path(), &address, Some(&admin_invite), profile);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    wait_for(&bob, "alice's profile", |s| {
        s.person(alice_id)
            .is_some_and(|p| p.name == "Mara" && p.avatar.is_some())
    });
    let hash = bob.state().person(alice_id).unwrap().avatar.unwrap();
    wait_for(&bob, "alice's picture", |_| bob.avatar(hash).is_some());
    assert_eq!(&*bob.avatar(hash).unwrap(), picture.as_slice());
}

#[test]
fn only_admins_name_the_server_and_everyone_sees_it() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    alice.command(UiCommand::SetServerProfile {
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
    alice.command(UiCommand::SetServerProfile {
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

    let rename = UiCommand::SetServerProfile {
        name: Some(Some("Bob's".into())),
        picture: None,
    };
    command_then(&bob, rename, "bob's rename wasn't refused", |e| {
        matches!(e, UiEvent::ServerProfileRefused(_)).then_some(())
    });
    assert_eq!(alice.state().server.clone().unwrap().title(), "Moss Co-op");

    alice.command(UiCommand::SetServerProfile {
        name: Some(None),
        picture: None,
    });
    wait_for(&bob, "the name cleared", |s| {
        s.server
            .as_ref()
            .is_some_and(|s| s.name.is_none() && s.avatar == Some(hash))
    });

    let other: Vec<u8> = (0..900u32).map(|i| (i * 5) as u8).collect();
    alice.command(UiCommand::SetServerProfile {
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
fn only_the_compared_key_is_verified_and_it_stays_verified_in_the_next_session() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    let code = invite(&alice);
    let bob_dir = tempfile::tempdir().unwrap();
    let bob = client_in(bob_dir.path(), &address, Some(&code));
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    let alice_id = alice.state().me.unwrap();
    let presented = key_of(&bob, alice_id);

    let compared = [9; 32];
    command_then(&bob, verify(alice_id, compared), "no notice", refused);
    assert!(
        VerifiedStore::load(bob_dir.path())
            .unwrap()
            .list()
            .is_empty()
    );
    assert!(!bob.state().person(alice_id).unwrap().verified);
    bob.command(verify(alice_id, presented));
    wait_for(&bob, "alice verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
    let unverify = UiCommand::SetVerified {
        user: alice_id,
        key: Fingerprint(compared),
        verified: false,
    };
    command_then(&bob, unverify, "no notice", refused);
    assert!(bob.state().person(alice_id).unwrap().verified);
    drop(bob);

    let bob = client_in(bob_dir.path(), &address, None);
    wait_connected(&bob, "bob back");
    assert!(
        bob.state()
            .person(alice_id)
            .is_some_and(|p| !p.verified && p.identity_key.is_none()),
        "verified before alice presented her key"
    );
    bob.command(UiCommand::Join(Some(room)));
    wait_for(&bob, "alice still verified", |s| {
        s.person(alice_id).is_some_and(|p| p.verified)
    });
}

fn refused(event: UiEvent) -> Option<()> {
    matches!(event, UiEvent::Notice(n) if n.contains("before you confirmed")).then_some(())
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
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "bob verified on the second server", |s| {
        s.person(bob_id).is_some_and(|p| p.verified) && s.key_warnings.is_empty()
    });
    let verified = VerifiedStore::load(alice_dir.path()).unwrap().list();
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].key, Fingerprint(identity_in(bob_dir.path())));
    assert_eq!(verified[0].servers.len(), 2);
}

/// A room, and another one to keep someone out of it, both made by `admin`.
fn room_and_elsewhere(admin: &Client) -> (RoomId, RoomId) {
    let room = create_room(admin);
    admin.command(UiCommand::CreateRoom("Elsewhere".into(), None));
    wait_for(admin, "the other room", |s| s.rooms.len() == 2);
    let elsewhere = admin
        .state()
        .rooms
        .iter()
        .find(|r| r.id != room)
        .unwrap()
        .id;
    (room, elsewhere)
}

/// Has `alice`, the admin, set `bob`'s voice playback while he is in another room, so before
/// he presents his key, then brings him into her room until she holds his key.
fn set_before_his_key(alice: &Client, bob: &Client, voice: SoundLevel) -> UserId {
    let (room, elsewhere) = room_and_elsewhere(alice);
    let bob_id = bob.state().me.unwrap();
    bob.command(UiCommand::Join(Some(elsewhere)));
    wait_for(alice, "bob in the other room", |s| {
        s.person(bob_id).is_some_and(|p| p.identity_key.is_none())
    });
    alice.command(UiCommand::SetSoundLevel(bob_id, Sound::Voice, voice));
    alice.command(UiCommand::Join(Some(room)));
    bob.command(UiCommand::Join(Some(room)));
    key_of(alice, bob_id);
    bob_id
}

#[test]
fn playback_follows_the_identity_key_to_every_server() {
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let quiet = SoundLevel {
        volume: 0.25,
        muted: false,
    };
    let loud = SoundLevel {
        volume: 2.0,
        muted: false,
    };
    let muted = SoundLevel {
        volume: 1.0,
        muted: true,
    };
    let (first, first_admin, _first) = server();
    {
        let alice = client_in(alice_dir.path(), &first, Some(&first_admin));
        wait_connected(&alice, "alice connected");
        let bob = client_in(bob_dir.path(), &first, Some(&invite(&alice)));
        let bob_id = set_before_his_key(&alice, &bob, quiet);
        alice.command(UiCommand::SetSoundLevel(bob_id, Sound::Share, muted));
        wait_for(&alice, "bob quiet and his stream muted", |s| {
            s.person(bob_id)
                .is_some_and(|p| p.playback.voice == quiet && p.playback.share == muted)
        });
    }

    let (second, second_admin, _second) = server();
    let alice = client_in(alice_dir.path(), &second, Some(&second_admin));
    wait_connected(&alice, "alice on the second server");
    let bob = client_in(bob_dir.path(), &second, Some(&invite(&alice)));
    let bob_id = set_before_his_key(&alice, &bob, loud);
    wait_for(&alice, "bob louder, his stream still muted", |s| {
        s.person(bob_id)
            .is_some_and(|p| p.playback.voice == loud && p.playback.share == muted)
    });
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
    let (room, elsewhere) = room_and_elsewhere(&alice);
    alice.command(UiCommand::Join(Some(room)));
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

    alice.command(UiCommand::DismissKeyChange {
        user: bob_id,
        shown: alice
            .state()
            .person(bob_id)
            .and_then(|p| p.identity_key)
            .map(Fingerprint),
    });
    wait_for(&alice, "the warning dismissed", |s| {
        s.key_warnings.is_empty()
    });
    assert!(!alice.state().person(bob_id).unwrap().verified);
    assert_eq!(
        VerifiedStore::load(alice_dir.path()).unwrap().list().len(),
        1,
        "bob's key is still verified"
    );
}

#[test]
fn people_not_verified_are_shown_so_and_still_hear_us() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(UiCommand::Join(Some(room)));
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
    wait_for_state_within(Duration::from_secs(10), &alice, "bob's mic followed", |s| {
        s.health
            .receiving
            .iter()
            .any(|t| t.label.ends_with(", mic"))
    });
}

#[test]
fn an_epoch_floor_outlives_a_restart_and_refuses_a_replayed_sender_key() {
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    let alice_id = alice.state().me.unwrap();
    let code = invite(&alice);
    let runtime = Runtime::new().unwrap();
    let bob_dir = tempfile::tempdir().unwrap();
    let (mut bob, placement) = runtime.block_on(async {
        let mut keystore = Keystore::open_in(bob_dir.path()).unwrap();
        let link = account::enroll(&mut keystore, &address, &code, "Ilse")
            .await
            .unwrap();
        let bob = Call::start(link, &keystore).unwrap();
        let (placement, _) = bob.join(Some(room), None).await.unwrap();
        (bob, placement)
    });
    // Alice's sender key for bob, as a server could record it to replay later.
    let (epoch, wrapped) = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(CallEvent::Server(ServerEvent::SenderKeyPublished {
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
    assert_eq!(epoch, placement.epoch);
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
        let notices = format!("{:?}", bob.engine.apply_placement(&placement));
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
    let encryption_key = || {
        Keystore::open_in(alice_dir.path())
            .unwrap()
            .encryption_key()
            .public()
    };
    let before = encryption_key();
    let alice = client_in(alice_dir.path(), &address, Some(&first.admin_invite()));
    wait_connected(&alice, "alice connected");
    let first_key = encryption_key();
    assert_ne!(first_key, before, "enrolled with the old encryption key");
    let room = create_room(&alice);
    alice.command(UiCommand::Join(Some(room)));
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
        let link = account::enroll(&mut keystore, &address, &rebuilt.admin_invite(), "Ilse")
            .await
            .unwrap();
        assert_ne!(keystore.encryption_key().public(), first_key);
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
    let (address, admin_invite, _server) = server();
    let (alice, _a) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let code = invite(&alice);
    let (bob, _b) = client(&address, &code);
    let bob_id = bob.state().me.unwrap();
    alice.command(UiCommand::RemoveUser {
        user: bob_id,
        cascade: false,
    });
    wait_for_state_within(Duration::from_secs(10), &bob, "bob to give up", |s| {
        matches!(s.connection, ConnectionStatus::Failed(_))
    });
}

#[test]
fn a_client_replaced_by_another_device_stays_signed_out() {
    let (address, invite, _server) = server();
    let (first, dir) = client(&address, &invite);
    let second = client_in(dir.path(), &address, None);
    wait_for(&first, "the first device to be signed out", |s| {
        s.connection == ConnectionStatus::SignedInElsewhere
    });

    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(
        first.state().connection,
        ConnectionStatus::SignedInElsewhere
    );
    assert_eq!(second.state().connection, ConnectionStatus::Connected);
}

#[test]
fn a_blocked_person_stays_in_the_room_unnamed_and_still_gets_our_key() {
    let (address, admin_invite, _server) = server();
    let (alice, alice_dir) = client(&address, &admin_invite);
    wait_connected(&alice, "connected");
    let room = create_room(&alice);
    alice.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "alice in the room", |s| s.current.is_some());
    let alice_id = alice.state().me.unwrap();
    let code = invite(&alice);
    let runtime = Runtime::new().unwrap();
    let bob_dir = tempfile::tempdir().unwrap();
    let bob = call_in(&runtime, bob_dir.path(), &address, &code, room);
    let shown_blocked = |s: &State| {
        s.person(bob.me()).is_some_and(|p| {
            p.blocked && p.name == BLOCKED_NAME && p.avatar.is_none() && p.room == Some(room)
        })
    };
    let shown_by_name = |s: &State| {
        s.person(bob.me())
            .is_some_and(|p| p.name == "Ilse" && !p.blocked)
    };
    wait_for(&alice, "bob by name", shown_by_name);

    alice.command(UiCommand::Block(bob.me()));
    wait_for(&alice, "bob blocked but still shown", shown_blocked);

    drop(alice);
    let alice = client_in(alice_dir.path(), &address, None);
    alice.command(UiCommand::Join(Some(room)));
    wait_for(&alice, "bob still blocked after a restart", shown_blocked);
    wait_until("blocked bob to hold alice's new key", || {
        holds_key_of(&bob, alice_id)
    });

    alice.command(UiCommand::Unblock(bob.me()));
    wait_for(&alice, "bob by name again", shown_by_name);
}
