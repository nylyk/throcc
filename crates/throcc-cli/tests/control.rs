mod common;

use std::sync::Arc;

use throcc_client_core::connection;
use throcc_client_core::control;
use throcc_crypto::identity::{self, IdentityKey, profile_message};
use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, Role, UserId};
use throcc_proto::messages::{
    AccountStep, AuthError, AuthResult, ClientHello, MAX_AVATAR_BYTES, MIN_PROTOCOL, PROTOCOL,
    Placement, Request, Response, ServerHello, ServerMessage, ServerProfile, SignIn, SignedProfile,
    Welcome,
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
    let pictures: Vec<Vec<u8>> = (0..40u8).map(|i| vec![0xff, 0xd8, 0xff, i]).collect();
    for picture in &pictures {
        let set = alice
            .request(Request::SetServerAvatar(Some(picture.clone())))
            .await;
        assert_eq!(set, Response::Ok);
    }

    let mut tasks = JoinSet::new();
    for client in [alice.clone(), bob.clone()] {
        for picture in pictures.clone() {
            let client = client.clone();
            tasks.spawn(async move {
                let fetched = client
                    .request(Request::FetchAvatar(Hash::of(&picture)))
                    .await;
                assert_eq!(fetched, Response::Avatar(picture));
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
        let step: AccountStep = Ok(UserId(1));
        send.write_all(&framing::encode(&step).unwrap())
            .await
            .unwrap();
        let _: SignedProfile = read_message(&mut recv).await;
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
    let _link = control::login(connected, &IdentityKey::generate(), &ilse(), None)
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

#[tokio::test]
async fn a_sign_in_is_refused_unless_its_key_signed_its_valid_profile_for_it() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    let identity = alice.keystore.identity();
    let me = alice.link.welcome.me;
    let stranger = IdentityKey::generate();
    let fingerprint = alice.link.connected.fingerprint;
    let signed = |signer: &IdentityKey, user: UserId, name: &str, picture: Option<Vec<u8>>| {
        let avatar = picture.as_deref().map(Hash::of);
        SignedProfile {
            name: name.into(),
            sig: signer.sign(&profile_message(&fingerprint.0, user, name, avatar)),
            picture,
        }
    };
    for bad in [
        signed(&stranger, me, "Ilse", None),
        signed(identity, UserId(me.0 + 1), "Ilse", None),
        signed(identity, me, " Ilse", None),
        signed(identity, me, "Ilse", Some(vec![1; MAX_AVATAR_BYTES + 1])),
        SignedProfile {
            name: "Mallory".into(),
            ..signed(identity, me, "Ilse", None)
        },
    ] {
        let answer = sign_in_giving(&server.addr, identity, me, &bad).await;
        assert_eq!(answer, Some(AuthError::BadProfile), "{:?}", bad.name);
    }
    let picture = vec![2; MAX_AVATAR_BYTES];
    let good = signed(identity, me, "Ilse", Some(picture.clone()));
    assert_eq!(
        sign_in_giving(&server.addr, identity, me, &good).await,
        None
    );
    let fetched = bob.request(Request::FetchAvatar(Hash::of(&picture))).await;
    assert_eq!(fetched, Response::Avatar(picture));
}

/// Signs in to `addr` as `identity` over a bare connection, giving `profile` once the server
/// names the user `me`, and returns the server's refusal, if any.
async fn sign_in_giving(
    addr: &str,
    identity: &IdentityKey,
    me: UserId,
    profile: &SignedProfile,
) -> Option<AuthError> {
    let connected = connection::connect(addr, None).await.unwrap();
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let hello: ServerHello = read_message(&mut recv).await;
    let client_nonce = [7; 32];
    let message = identity::auth_message(&hello.nonce, &client_nonce, &connected.exporter);
    let sign_in = ClientHello {
        protocol: PROTOCOL,
        sign_in: SignIn::Auth {
            pubkey: identity.public(),
            client_nonce,
            want_room: None,
            signature: identity.sign(&message),
        },
    };
    send.write_all(&framing::encode(&sign_in).unwrap())
        .await
        .unwrap();
    let step: AccountStep = read_message(&mut recv).await;
    assert_eq!(step, Ok(me));
    send.write_all(&framing::encode(profile).unwrap())
        .await
        .unwrap();
    let answer = match read_message::<AuthResult>(&mut recv).await {
        AuthResult::Ok(_) => None,
        AuthResult::Err(e) => Some(e),
    };
    connected.close().await;
    answer
}
