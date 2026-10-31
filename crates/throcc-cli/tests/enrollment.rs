mod common;

use std::time::Duration;

use throcc_client_core::account::{self, SignInError};
use throcc_client_core::connection;
use throcc_client_core::control::LoginError;
use throcc_client_core::keystore::Keystore;
use throcc_crypto::identity::IdentityKey;
use throcc_crypto::pake::{ClientPake, PakeError, Transcript};
use throcc_proto::framing;
use throcc_proto::ids::Role;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    AuthError, AuthResult, ErrorCode, Event, Hello, PROTOCOL, PakeFinish, PakeStep, Request,
    Response, ServerHello,
};
use throcc_server::{Congestion, HANDSHAKES, tls};

use common::*;

async fn try_enroll(server_addr: &str, code: &InviteCode) -> Result<(), SignInError> {
    let dir = tempfile::tempdir().unwrap();
    let mut keystore = Keystore::open_in(dir.path()).unwrap();
    account::enroll(&mut keystore, server_addr, code)
        .await
        .map(|_| ())
}

fn failures(server: &TestServer, code: &InviteCode) -> Option<u32> {
    server
        .shared
        .hub()
        .db
        .invite(&code.id)
        .unwrap()
        .map(|i| i.failures)
}

/// Terminates the client's TLS with its own key and relays the control stream to `upstream`.
async fn start_mitm(upstream: String) -> String {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = bare_endpoint(
        dir.path(),
        tls::transport_config(Congestion::Cubic).unwrap(),
    );
    let addr = endpoint.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _dir = dir;
        let victim = endpoint.accept().await.unwrap().await.unwrap();
        let server = connection::connect(&upstream, None).await.unwrap();
        let (mut server_send, mut server_recv) = server.connection.accept_bi().await.unwrap();
        let (mut victim_send, mut victim_recv) = victim.open_bi().await.unwrap();
        let down =
            tokio::spawn(async move { tokio::io::copy(&mut server_recv, &mut victim_send).await });
        let up =
            tokio::spawn(async move { tokio::io::copy(&mut victim_recv, &mut server_send).await });
        victim.closed().await;
        server.connection.close(0u32.into(), b"");
        down.abort();
        up.abort();
    });
    addr
}

#[tokio::test]
async fn first_enrollment_becomes_admin() {
    let server = start_server();
    let alice = admin(&server).await;
    assert_eq!(alice.link.welcome.role, Role::Admin);
    let bob = invited(&server, &alice).await;
    assert_eq!(bob.link.welcome.role, Role::User);
}

#[tokio::test]
async fn relaying_mitm_fails_at_key_confirmation_and_uses_one_try() {
    let server = start_server();
    let alice = admin(&server).await;
    let code = invite(&alice).await;
    let mitm = start_mitm(server.addr.clone()).await;

    let err = try_enroll(&mitm, &code).await.unwrap_err();
    assert!(
        matches!(
            err,
            SignInError::Login(LoginError::Pake(PakeError::Confirmation))
        ),
        "{err}"
    );
    assert_eq!(failures(&server, &code), Some(1));

    try_enroll(&server.addr, &code).await.unwrap();
    assert_eq!(failures(&server, &code), None);
}

#[tokio::test]
async fn third_failure_burns_the_invite_and_tells_the_minter() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let code = invite(&alice).await;
    let wrong = InviteCode {
        id: code.id.clone(),
        secret: "00000".into(),
    };

    for attempt in 1..=3 {
        let err = try_enroll(&server.addr, &wrong).await.unwrap_err();
        assert!(
            matches!(err, SignInError::Login(LoginError::Pake(_))),
            "attempt {attempt}: {err}"
        );
    }
    let burned = alice
        .event(|e| match e {
            Event::InviteBurned { code_id } => Some(code_id.clone()),
            _ => None,
        })
        .await;
    assert_eq!(burned, code.id);
    assert_eq!(failures(&server, &code), None);
    assert!(try_enroll(&server.addr, &code).await.is_err());
}

#[tokio::test]
async fn unknown_invite_ids_fail_like_wrong_secrets() {
    let server = start_server();
    let _alice = admin(&server).await;
    let unknown: InviteCode = "ZZZ-ZZZZZ".parse().unwrap();
    let err = try_enroll(&server.addr, &unknown).await.unwrap_err();
    assert!(
        matches!(
            err,
            SignInError::Login(LoginError::Pake(PakeError::Confirmation))
        ),
        "{err}"
    );
}

#[tokio::test]
async fn repeated_failures_from_one_source_are_rate_limited() {
    let server = start_server();
    let unknown: InviteCode = "ZZZ-ZZZZZ".parse().unwrap();
    for _ in 0..10 {
        try_enroll(&server.addr, &unknown).await.unwrap_err();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = try_enroll(&server.addr, &unknown).await.unwrap_err();
    assert!(
        matches!(
            err,
            SignInError::Login(LoginError::Refused(AuthError::RateLimited))
        ),
        "{err}"
    );
}

#[tokio::test]
async fn users_get_three_invites_a_day() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    for _ in 0..3 {
        invite(&bob).await;
    }
    let fourth = bob.request(Request::CreateInvite).await;
    assert_error(&fourth, ErrorCode::RateLimited);
    for _ in 0..5 {
        invite(&alice).await;
    }
}

#[tokio::test]
async fn enrolled_users_count_against_the_daily_cap() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    for _ in 0..3 {
        invited(&server, &bob).await;
    }
    let fourth = bob.request(Request::CreateInvite).await;
    assert_error(&fourth, ErrorCode::RateLimited);
}

#[tokio::test]
async fn manager_cannot_remove_an_admin() {
    let server = start_server();
    let alice = admin(&server).await;
    let bob = invited(&server, &alice).await;
    let carol = invited(&server, &alice).await;
    let bob_id = bob.link.welcome.me;
    assert_eq!(
        alice
            .request(Request::SetRole {
                user: bob_id,
                role: Role::Manager
            })
            .await,
        Response::Ok
    );

    let alice_id = alice.link.welcome.me;
    let carol_id = carol.link.welcome.me;
    for target in [alice_id, carol_id] {
        let res = bob
            .request(Request::RemoveUser {
                user: target,
                cascade: false,
            })
            .await;
        assert_error(&res, ErrorCode::Forbidden);
    }
    let res = alice
        .request(Request::SetRole {
            user: bob_id,
            role: Role::Admin,
        })
        .await;
    assert_error(&res, ErrorCode::Forbidden);
}

#[tokio::test]
async fn removal_closes_the_connection_and_cascades() {
    let server = start_server();
    let alice = admin(&server).await;
    let mut bob = invited(&server, &alice).await;
    let carol = invited(&server, &bob).await;
    let bob_id = bob.link.welcome.me;

    let res = alice
        .request(Request::RemoveUser {
            user: bob_id,
            cascade: true,
        })
        .await;
    assert_eq!(res, Response::Ok);
    bob.event(|e| matches!(e, Event::Kicked { .. }).then_some(()))
        .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        carol.link.connected.connection.closed(),
    )
    .await
    .unwrap();

    let err = account::sign_in(&carol.keystore, &server.addr, None)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            SignInError::Login(LoginError::Refused(AuthError::UnknownKey))
        ),
        "{err}"
    );
}

#[tokio::test]
async fn concurrent_runs_on_one_invite_enroll_at_most_one_user() {
    let server = start_server();
    let alice = admin(&server).await;
    let code = invite(&alice).await;
    let runs: Vec<_> = (0..3)
        .map(|_| {
            let (addr, code) = (server.addr.clone(), code.clone());
            tokio::spawn(async move { try_enroll(&addr, &code).await.is_ok() })
        })
        .collect();
    let mut enrolled = 0;
    for run in runs {
        enrolled += usize::from(run.await.unwrap());
    }
    assert_eq!(enrolled, 1);
    let Response::Users(users) = alice.request(Request::ListUsers).await else {
        panic!()
    };
    assert_eq!(users.len(), 2);
}

#[tokio::test]
async fn parallel_runs_from_one_source_are_rate_limited() {
    let server = start_server();
    let unknown: InviteCode = "ZZZ-ZZZZZ".parse().unwrap();
    let runs: Vec<_> = (0..20)
        .map(|_| {
            let (addr, code) = (server.addr.clone(), unknown.clone());
            tokio::spawn(async move { try_enroll(&addr, &code).await.err().unwrap() })
        })
        .collect();
    let mut limited = 0;
    for run in runs {
        let err = run.await.unwrap();
        limited += usize::from(matches!(
            err,
            SignInError::Login(LoginError::Refused(AuthError::RateLimited))
        ));
    }
    assert_eq!(limited, 10);
}

#[tokio::test]
async fn a_failed_run_does_not_burn_an_invite_while_another_run_is_in_flight() {
    let server = start_server();
    let alice = admin(&server).await;
    let code = invite(&alice).await;

    let connected = connection::connect(&server.addr, None).await.unwrap();
    let (mut send, mut recv) = connected.connection.accept_bi().await.unwrap();
    let hello: ServerHello = framing::decode(&read_frame(&mut recv).await).unwrap();
    let identity = IdentityKey::generate();
    let pubkey = identity.public();
    let client_nonce = [7; 32];
    let (pake, pake_a) = ClientPake::start(&code.id, &code.secret);
    let enroll = Hello::Enroll {
        code_id: code.id.clone(),
        pubkey,
        client_nonce,
        pake_a: pake_a.clone(),
    };
    send.write_all(&framing::encode(&enroll).unwrap())
        .await
        .unwrap();
    let step: PakeStep = framing::decode(&read_frame(&mut recv).await).unwrap();
    let reply = step.unwrap();

    let wrong = InviteCode {
        id: code.id.clone(),
        secret: "00000".into(),
    };
    for _ in 0..2 {
        try_enroll(&server.addr, &wrong).await.unwrap_err();
    }
    assert_eq!(failures(&server, &code), Some(3));

    let transcript = Transcript {
        server_nonce: &hello.nonce,
        client_nonce: &client_nonce,
        code_id: &code.id,
        pubkey: &pubkey,
        spki_hash: &connected.fingerprint.0,
        exporter: &connected.exporter,
        pake_a: &pake_a,
        pake_b: &reply.pake_b,
    };
    let finish = PakeFinish {
        confirm_a: pake.finish(&transcript, &reply.confirm_b).unwrap(),
        signature: identity.sign(&transcript.to_bytes()),
    };
    send.write_all(&framing::encode(&finish).unwrap())
        .await
        .unwrap();
    let result: AuthResult = framing::decode(&read_frame(&mut recv).await).unwrap();
    assert!(matches!(result, AuthResult::Ok(_)), "{result:?}");
}

#[tokio::test]
async fn one_source_signs_in_a_few_connections_at_a_time() {
    let server = start_server();
    let mut idle = Vec::new();
    for _ in 0..HANDSHAKES.per_source {
        idle.push(connection::connect(&server.addr, None).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let enrolling = tokio::spawn({
        let (addr, code) = (server.addr.clone(), server.bootstrap.clone());
        async move { enroll_at(&addr, &code).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!enrolling.is_finished());

    idle.pop().unwrap().connection.close(0u32.into(), b"");
    let admin = tokio::time::timeout(Duration::from_secs(3), enrolling)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admin.link.welcome.role, Role::Admin);
}

#[tokio::test]
async fn a_run_whose_client_stops_reading_still_burns_the_invite() {
    let server = start_server();
    let mut alice = admin(&server).await;
    let code = invite(&alice).await;
    let server_hello = ServerHello {
        nonce: [0; 32],
        protocol: PROTOCOL,
    };
    let window = framing::encode(&server_hello).unwrap().len() as u32;
    let stuck = connect_with_transport(&server.addr, small_window_transport(window)).await;
    let (mut send, _recv) = stuck.connection.accept_bi().await.unwrap();
    let (_, pake_a) = ClientPake::start(&code.id, &code.secret);
    let enroll = Hello::Enroll {
        code_id: code.id.clone(),
        pubkey: IdentityKey::generate().public(),
        client_nonce: [7; 32],
        pake_a,
    };
    send.write_all(&framing::encode(&enroll).unwrap())
        .await
        .unwrap();
    wait_for("the stuck run to claim a try", || {
        failures(&server, &code) == Some(1)
    })
    .await;

    let wrong = InviteCode {
        id: code.id.clone(),
        secret: "00000".into(),
    };
    for _ in 0..2 {
        try_enroll(&server.addr, &wrong).await.unwrap_err();
    }
    assert_eq!(failures(&server, &code), Some(3));

    wait_for("the stuck run to burn the invite", || {
        failures(&server, &code).is_none()
    })
    .await;
    let burned = alice
        .event(|e| match e {
            Event::InviteBurned { code_id } => Some(code_id.clone()),
            _ => None,
        })
        .await;
    assert_eq!(burned, code.id);
}
