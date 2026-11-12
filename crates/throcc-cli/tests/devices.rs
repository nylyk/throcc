mod common;

use std::time::Duration;

use throcc_client_core::account;
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
async fn signing_in_again_replaces_the_earlier_connection() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    let mut bob_again = account::sign_in(&bob.keystore, &server.addr, None)
        .await
        .unwrap();

    let bob_third = account::sign_in(&bob.keystore, &server.addr, None)
        .await
        .unwrap();
    let last = tokio::time::timeout(Duration::from_secs(5), bob_again.events.recv())
        .await
        .unwrap();
    assert_eq!(last, Some(Event::SignedInElsewhere));
    bob_again.connected.connection.closed().await;
    assert_eq!(
        closed_with(&bob_again.connected.connection),
        Some(close::SIGNED_IN_ELSEWHERE)
    );
    assert!(bob_third.connected.connection.close_reason().is_none());
}
