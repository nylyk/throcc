mod common;

use std::sync::Arc;

use throcc_client_core::connection;
use throcc_client_core::control;
use throcc_crypto::identity::IdentityKey;
use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, Role, UserId};
use throcc_proto::messages::{
    AuthResult, Hello, PROTOCOL, Placed, Request, Response, ServerHello, ServerMessage, Welcome,
};
use throcc_proto::transport::close;
use throcc_server::identity::ServerIdentity;
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
                    client.request(Request::SetAvatar(bytes.clone())).await
                else {
                    panic!("expected a hash");
                };
                let Response::Avatar(back) = client.request(Request::GetAvatar(hash)).await else {
                    panic!("expected an avatar");
                };
                assert_eq!(back, bytes);
            });
        }
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let missing = alice.request(Request::GetAvatar(Hash([0; 32]))).await;
    assert!(matches!(missing, Response::Err { .. }));
}

#[tokio::test]
async fn oversized_frame_closes_the_connection() {
    let server = start_server();
    let connected = connection::connect(&server.addr, None).await.unwrap();
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let mut header = [0u8; 4];
    recv.read_exact(&mut header).await.unwrap();
    let mut body = vec![0u8; framing::body_len(header).unwrap()];
    recv.read_exact(&mut body).await.unwrap();
    let _: ServerHello = framing::decode(&body).unwrap();

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
async fn unknown_reply_id_closes_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let identity = ServerIdentity::load_or_create(dir.path()).unwrap();
    let config =
        tls::server_config(&identity, tls::transport_config(Congestion::Cubic).unwrap()).unwrap();
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap().to_string();
    let fake = tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = ServerHello {
            nonce: [0; 32],
            protocol: PROTOCOL,
        };
        send.write_all(&framing::encode(&hello).unwrap())
            .await
            .unwrap();
        let mut header = [0u8; 4];
        recv.read_exact(&mut header).await.unwrap();
        let mut body = vec![0u8; framing::body_len(header).unwrap()];
        recv.read_exact(&mut body).await.unwrap();
        let _: Hello = framing::decode(&body).unwrap();
        let welcome = Welcome {
            me: UserId(1),
            role: Role::User,
            rooms: vec![],
            presence: vec![],
            placed: Placed {
                room: None,
                epoch: Epoch(0),
                tracks: None,
                peers: vec![],
            },
        };
        send.write_all(&framing::encode(&AuthResult::Ok(Box::new(welcome))).unwrap())
            .await
            .unwrap();
        let stray = ServerMessage::Response {
            id: 999,
            res: Response::Ok,
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
