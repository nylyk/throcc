mod common;

use std::time::Duration;

use throcc_client_core::account::{self, SignInError};
use throcc_client_core::keystore::Keystore;
use throcc_proto::messages::Event;
use throcc_proto::transport::close;

use common::*;

fn closed_with(connection: &quinn::Connection) -> Option<u32> {
    match connection.close_reason()? {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            Some(closed.error_code.into_inner() as u32)
        }
        other => panic!("closed without an application code: {other}"),
    }
}

#[tokio::test]
async fn another_device_of_a_member_signs_in_without_an_invite_and_pins_the_server() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    let fingerprint = bob.link.connected.fingerprint;

    let phone = tempfile::tempdir().unwrap();
    std::fs::copy(
        bob.keystore.dir().join("identity_key"),
        phone.path().join("identity_key"),
    )
    .unwrap();
    let mut keystore = Keystore::open_in(phone.path()).unwrap();
    assert_eq!(keystore.known_servers.get(&server.addr), None);
    let link = account::sign_in_unpinned(&mut keystore, &server.addr, &ilse())
        .await
        .unwrap();
    assert_eq!(link.welcome.me, bob.link.welcome.me);
    assert_eq!(keystore.known_servers.get(&server.addr), Some(fingerprint));

    keystore.known_servers.forget(&server.addr).unwrap();
    keystore
        .known_servers
        .trust("old.example.net", fingerprint)
        .unwrap();
    let refused = account::sign_in_unpinned(&mut keystore, &server.addr, &ilse()).await;
    assert!(matches!(refused, Err(SignInError::KnownAs(known)) if known == "old.example.net"));
    assert_eq!(keystore.known_servers.get(&server.addr), None);

    let stranger = tempfile::tempdir().unwrap();
    let mut keystore = Keystore::open_in(stranger.path()).unwrap();
    let refused = account::sign_in_unpinned(&mut keystore, &server.addr, &ilse()).await;
    assert!(matches!(refused, Err(SignInError::NotEnrolled(_))));
    assert_eq!(keystore.known_servers.get(&server.addr), None);
}

#[tokio::test]
async fn signing_in_again_replaces_the_earlier_connection() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;

    let bob_again = account::sign_in(&bob.keystore, &server.addr, &ilse(), None)
        .await
        .unwrap();
    let last = tokio::time::timeout(Duration::from_secs(5), bob.link.events.recv())
        .await
        .unwrap();
    assert_eq!(last, Some(Event::SignedInElsewhere));
    bob.link.connected.connection.closed().await;
    assert_eq!(
        closed_with(&bob.link.connected.connection),
        Some(close::SIGNED_IN_ELSEWHERE)
    );
    assert!(bob_again.connected.connection.close_reason().is_none());
}
