mod common;

use std::time::Duration;

use serde::Deserialize;
use throcc_client_core::account;
use throcc_client_core::connection;
use throcc_crypto::identity;
use throcc_proto::framing;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::messages::{
    ClientHello, Event, MediaState, Placement, Request, Response, Room, ServerHello, ServerMessage,
    ServerProfile, SignIn, UserProfile,
};
use throcc_proto::transport::close;
use throcc_proto::v1;

use common::*;

/// A presence as protocol 1 defined it.
#[derive(Debug, Deserialize)]
struct PresenceV1 {
    user: UserId,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    avatar: Option<Hash>,
    room: Option<RoomId>,
    mic: bool,
    #[allow(dead_code)]
    deafened: bool,
    #[allow(dead_code)]
    sharing: bool,
    #[allow(dead_code)]
    role: Role,
}

/// A welcome as protocol 1 defined it.
#[derive(Debug, Deserialize)]
struct WelcomeV1 {
    me: UserId,
    #[allow(dead_code)]
    role: Role,
    #[allow(dead_code)]
    rooms: Vec<Room>,
    presence: Vec<PresenceV1>,
    #[allow(dead_code)]
    placement: Placement,
    #[allow(dead_code)]
    server: ServerProfile,
    #[allow(dead_code)]
    profile: UserProfile,
}

async fn read_body(recv: &mut quinn::RecvStream) -> Vec<u8> {
    let mut header = [0u8; framing::LEN_BYTES];
    recv.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; framing::body_len(header).unwrap()];
    recv.read_exact(&mut body).await.unwrap();
    body
}

fn closed_with(connection: &quinn::Connection) -> Option<u32> {
    match connection.close_reason()? {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            Some(closed.error_code.into_inner() as u32)
        }
        other => panic!("closed without an application code: {other}"),
    }
}

#[tokio::test]
async fn a_protocol_1_session_gets_protocol_1_shapes_and_the_old_kick() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let room = create_room(&mut alice, "den").await;
    alice.join(Some(room)).await;
    let bob = invited(&server, &alice).await;
    let bob_id = bob.link.welcome.me;
    bob.disconnect();

    let connected = connect_with_transport(&server.addr, connection::transport_config()).await;
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let hello: ServerHello = read_message(&mut recv).await;
    assert!((hello.min_protocol..=hello.max_protocol).contains(&1));
    let client_nonce = [7; 32];
    let message = identity::auth_message(&hello.nonce, &client_nonce, &connected.exporter);
    let sign_in = ClientHello {
        protocol: 1,
        sign_in: SignIn::Auth {
            pubkey: bob.keystore.identity().public(),
            client_nonce,
            want_room: None,
            signature: bob.keystore.identity().sign(&message),
        },
    };
    send.write_all(&framing::encode(&sign_in).unwrap())
        .await
        .unwrap();

    let (ok, welcome): (u32, WelcomeV1) = framing::decode(&read_body(&mut recv).await).unwrap();
    assert_eq!((ok, welcome.me), (0, bob_id));
    assert_eq!(welcome.presence.len(), 1);
    assert_eq!(welcome.presence[0].room, Some(room));

    let muted = MediaState::default();
    assert_eq!(alice.request(Request::SetMedia(muted)).await, Response::Ok);
    let (event, changed, presence): (u32, u32, PresenceV1) =
        framing::decode(&read_body(&mut recv).await).unwrap();
    assert_eq!((event, changed), (1, 0));
    assert_eq!(presence.user, alice.link.welcome.me);
    assert!(!presence.mic);

    let mut bob_again = account::sign_in(&bob.keystore, &server.addr, None)
        .await
        .unwrap();
    let kicked: ServerMessage = framing::decode(&read_body(&mut recv).await).unwrap();
    assert_eq!(
        kicked,
        ServerMessage::Event(Event::RemovedFromServer {
            reason: v1::SIGNED_IN_ELSEWHERE.to_owned(),
        })
    );
    connected.connection.closed().await;
    assert_eq!(closed_with(&connected.connection), Some(close::REMOVED));

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
