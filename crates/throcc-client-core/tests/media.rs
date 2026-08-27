mod common;

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{SERVER_LABEL, TestServer, keystore, next_event};
use tempfile::TempDir;
use throcc_client_core::{Client, Command, Event};
use throcc_proto::{MediaId, Role, RoomId, Tracks};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::TryRecvError;

const UNITS: u64 = 30;
const QUIET: Duration = Duration::from_millis(300);

fn admin(server: &TestServer, directory: &TempDir) -> Client {
    Client::connect(
        server.address,
        SERVER_LABEL,
        keystore(directory),
        Some(server.bootstrap_invite()),
    )
    .expect("the bootstrap code should enroll an admin")
}

fn enrolled(server: &TestServer, directory: &TempDir, admin: &Client) -> Client {
    let mut events = admin.events();
    admin
        .command(Command::CreateInvite {
            role: Role::User,
            ttl_secs: 0,
        })
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
        Event::RoomCreated(room) => room.id,
        other => panic!("expected the room, got {other:?}"),
    }
}

fn enter(client: &Client, room: RoomId) -> Tracks {
    let mut events = client.events();
    client.command(Command::SetRoom(Some(room))).unwrap();
    match next_event(&mut events) {
        Event::Placed(placed) => placed.tracks.expect("a room entered allocates tracks"),
        other => panic!("expected a placement, got {other:?}"),
    }
}

fn counted(counter: u64, len: usize) -> Vec<u8> {
    let mut unit = counter.to_be_bytes().to_vec();
    unit.resize(len, 0);
    unit
}

fn counter_of(unit: &[u8]) -> u64 {
    u64::from_be_bytes(unit[..8].try_into().expect("a counted unit"))
}

type Arrivals = HashMap<MediaId, Vec<Vec<u8>>>;

/// Collects units in arrival order per track, from before the first send until
/// the stream has been quiet, so the collector never falls behind the channel.
fn collecting(mut events: Receiver<Event>) -> JoinHandle<Arrivals> {
    std::thread::spawn(move || {
        let mut arrivals = Arrivals::new();
        let mut last_arrival = Instant::now();
        loop {
            match events.try_recv() {
                Ok(Event::Media {
                    media_id, bytes, ..
                }) => {
                    arrivals.entry(media_id).or_default().push(bytes);
                    last_arrival = Instant::now();
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    if last_arrival.elapsed() > QUIET {
                        return arrivals;
                    }
                    std::thread::yield_now();
                }
                Err(TryRecvError::Lagged(missed)) => panic!("the collector fell {missed} behind"),
                Err(TryRecvError::Closed) => return arrivals,
            }
        }
    })
}

#[test]
fn units_cross_the_server_in_order_and_without_duplicates() {
    let server = TestServer::start();
    let sender_dir = TempDir::new().unwrap();
    let receiver_dir = TempDir::new().unwrap();

    let sender = admin(&server, &sender_dir);
    let receiver = enrolled(&server, &receiver_dir, &sender);
    let room = created(&sender, "lounge");
    enter(&receiver, room);
    let tracks = enter(&sender, room);
    let share = tracks.shares.first().expect("one share");

    let arriving = collecting(receiver.events());
    let sending = sender.media();
    for counter in 1..=UNITS {
        sending.send(tracks.mic, &counted(counter, 80), None, false);
        sending.send(share.video, &counted(counter, 3_000), Some(90), false);
        sending.send(
            share.audio.expect("a share allocates an audio id"),
            &counted(counter, 160),
            Some(90),
            false,
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let arrivals = arriving.join().expect("the collector");
    assert_eq!(sending.dropped_units(), 0, "nothing was dropped locally");
    assert_eq!(
        arrivals.len(),
        3,
        "one stream per media id, got {:?}",
        arrivals.keys().collect::<Vec<_>>()
    );

    for (media_id, units) in &arrivals {
        let counters: Vec<u64> = units.iter().map(|unit| counter_of(unit)).collect();
        assert!(
            counters.windows(2).all(|pair| pair[0] < pair[1]),
            "{media_id} arrived out of order or duplicated: {counters:?}"
        );
        assert_eq!(
            counters.len() as u64,
            UNITS,
            "{media_id} lost units on loopback"
        );
        let expected_len = if *media_id == tracks.mic { 80 } else { 160 };
        let expected_len = if *media_id == share.video {
            3_000
        } else {
            expected_len
        };
        assert!(
            units.iter().all(|unit| unit.len() == expected_len),
            "{media_id} reassembled to the wrong length"
        );
    }

    receiver.shutdown();
    sender.shutdown();
}

#[test]
fn a_client_in_another_room_receives_nothing() {
    let server = TestServer::start();
    let sender_dir = TempDir::new().unwrap();
    let elsewhere_dir = TempDir::new().unwrap();

    let sender = admin(&server, &sender_dir);
    let elsewhere = enrolled(&server, &elsewhere_dir, &sender);
    let lounge = created(&sender, "lounge");
    let other = created(&sender, "other");
    enter(&elsewhere, other);
    let tracks = enter(&sender, lounge);

    let arriving = collecting(elsewhere.events());
    let sending = sender.media();
    for counter in 1..=UNITS {
        sending.send(tracks.mic, &counted(counter, 80), None, false);
    }

    assert!(
        arriving.join().expect("the collector").is_empty(),
        "media must not leave the room it was sent in"
    );

    elsewhere.shutdown();
    sender.shutdown();
}

#[test]
fn a_sender_claiming_another_peers_media_id_is_dropped() {
    let server = TestServer::start();
    let spoofer_dir = TempDir::new().unwrap();
    let victim_dir = TempDir::new().unwrap();

    let spoofer = admin(&server, &spoofer_dir);
    let victim = enrolled(&server, &victim_dir, &spoofer);
    let room = created(&spoofer, "lounge");
    let victim_tracks = enter(&victim, room);
    let own_tracks = enter(&spoofer, room);

    let spoofed = collecting(victim.events());
    let sending = spoofer.media();
    for counter in 1..=UNITS {
        sending.send(victim_tracks.mic, &counted(counter, 80), None, false);
        sending.send(MediaId(u32::MAX), &counted(counter, 80), None, false);
    }
    assert!(
        spoofed.join().expect("the collector").is_empty(),
        "the server must not forward an id the sender does not own"
    );

    let own = collecting(victim.events());
    for counter in 1..=UNITS {
        sending.send(own_tracks.mic, &counted(counter, 80), None, false);
    }
    assert_eq!(
        own.join().expect("the collector").len(),
        1,
        "the spoofer's own track still flows"
    );

    victim.shutdown();
    spoofer.shutdown();
}
