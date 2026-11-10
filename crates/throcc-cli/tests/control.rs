mod common;

use std::sync::Arc;

use throcc_client_core::connection;
use throcc_client_core::control;
use throcc_crypto::identity::IdentityKey;
use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, Role, UserId};
use throcc_proto::messages::{
    AuthResult, ClientHello, MIN_PROTOCOL, PROTOCOL, Placement, Request, Response, ServerHello,
    ServerMessage, ServerProfile, UserProfile, Welcome,
};
use throcc_proto::transport::{IDLE_TIMEOUT, close};
use throcc_server::{Congestion, tls};
use tokio::task::JoinSet;

use common::*;

#[tokio::test]
async fn concurrent_requests_match_by_id() {
    let server = start_server();
    let alice = Arc::new(admin(&server).await);
    let bob = Arc::new(invited(&server, &alice).await);

    let mut tasks = JoinSet::new();
    for (who, client) in [(0u8, alice.clone()), (1u8, bob.clone())] {
        for i in 0..40u8 {
            let client = client.clone();
            tasks.spawn(async move {
                let bytes = vec![who, i];
                let Response::AvatarHash(hash) =
                    client.request(Request::UploadAvatar(bytes.clone())).await
                else {
                    panic!("expected a hash");
                };
                let Response::Avatar(back) = client.request(Request::FetchAvatar(hash)).await
                else {
                    panic!("expected an avatar");
                };
                assert_eq!(back, bytes);
            });
        }
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let missing = alice.request(Request::FetchAvatar(Hash([0; 32]))).await;
    assert!(matches!(missing, Response::Err { .. }));
}

#[tokio::test]
async fn oversized_frame_closes_the_connection() {
    let server = start_server();
    let connected = connection::connect(&server.addr, None).await.unwrap();
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let _: ServerHello = read_message(&mut recv).await;

    send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    match connected.connection.closed().await {
        quinn::ConnectionError::ApplicationClosed(frame) => {
            assert_eq!(frame.error_code, close::PROTOCOL_ERROR.into());
            assert!(String::from_utf8_lossy(&frame.reason).contains("exceeds the 1 MiB cap"));
        }
        other => panic!("expected an application close, got {other}"),
    }
}

#[tokio::test]
async fn a_hello_for_an_unsupported_protocol_is_refused_before_its_sign_in_is_read() {
    let server = start_server();
    for protocol in [MIN_PROTOCOL - 1, PROTOCOL + 1] {
        let connected = connection::connect(&server.addr, None).await.unwrap();
        let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
        let hello: ServerHello = read_message(&mut recv).await;
        assert_eq!(
            (hello.min_protocol, hello.max_protocol),
            (MIN_PROTOCOL, PROTOCOL)
        );

        let undecodable_sign_in = [0xff_u8; 8];
        let client_hello = framing::encode(&(protocol, undecodable_sign_in)).unwrap();
        send.write_all(&client_hello).await.unwrap();
        match connected.connection.closed().await {
            quinn::ConnectionError::ApplicationClosed(frame) => {
                assert_eq!(frame.error_code, close::PROTOCOL_ERROR.into());
                assert_eq!(&frame.reason[..], b"unsupported protocol");
            }
            other => panic!("expected an application close, got {other}"),
        }
    }
}

#[tokio::test]
async fn unknown_reply_id_closes_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = bare_endpoint(
        dir.path(),
        tls::transport_config(Congestion::Cubic, IDLE_TIMEOUT).unwrap(),
    );
    let addr = endpoint.local_addr().unwrap().to_string();
    let fake = tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = ServerHello {
            nonce: [0; 32],
            min_protocol: PROTOCOL,
            max_protocol: PROTOCOL,
        };
        send.write_all(&framing::encode(&hello).unwrap())
            .await
            .unwrap();
        let _: ClientHello = read_message(&mut recv).await;
        let welcome = Welcome {
            me: UserId(1),
            role: Role::User,
            rooms: vec![],
            presence: vec![],
            placement: Placement {
                room: None,
                epoch: Epoch(0),
                tracks: None,
                peers: vec![],
                audiences: Default::default(),
            },
            server: ServerProfile::default(),
            profile: UserProfile {
                name: "Ilse".into(),
                avatar: None,
            },
        };
        send.write_all(&framing::encode(&AuthResult::Ok(Box::new(welcome))).unwrap())
            .await
            .unwrap();
        let stray = ServerMessage::Response {
            id: 999,
            response: Response::Ok,
        };
        send.write_all(&framing::encode(&stray).unwrap())
            .await
            .unwrap();
        connection.closed().await
    });

    let connected = connection::connect(&addr, None).await.unwrap();
    let _link = control::login(connected, &IdentityKey::generate(), None)
        .await
        .unwrap();
    match fake.await.unwrap() {
        quinn::ConnectionError::ApplicationClosed(frame) => {
            assert_eq!(frame.error_code, close::PROTOCOL_ERROR.into());
            assert_eq!(&frame.reason[..], b"unknown reply id");
        }
        other => panic!("expected the client to close, got {other}"),
    }
}
