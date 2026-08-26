use std::net::IpAddr;

use ed25519_dalek::{Signer as _, SigningKey};
use tempfile::TempDir;
use throcc_proto::auth::signing_input;
use throcc_proto::{Auth, AuthError, AuthResult, Role};
use throcc_server::database::Database;
use throcc_server::{State, auth, invite};

const SERVER_NONCE: [u8; 32] = [1u8; 32];
const EXPORTER: [u8; 32] = [2u8; 32];
const PEER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

fn state(directory: &TempDir) -> State {
    State::new(Database::open(directory.path()).unwrap())
}

fn signed(
    key: &SigningKey,
    server_nonce: &[u8; 32],
    exporter: &[u8; 32],
    invite_code: Option<&str>,
) -> Auth {
    let client_nonce = [3u8; 32];
    Auth {
        pubkey: key.verifying_key().to_bytes(),
        client_nonce,
        invite_code: invite_code.map(str::to_string),
        want_room: None,
        signature: key
            .sign(&signing_input(server_nonce, &client_nonce, exporter))
            .to_bytes(),
    }
}

fn refusal(result: AuthResult) -> AuthError {
    match result {
        AuthResult::Err(error) => error,
        AuthResult::Ok { .. } => panic!("expected a refusal"),
    }
}

#[test]
fn a_signature_is_bound_to_this_connection_and_this_nonce() {
    let directory = TempDir::new().unwrap();
    let state = state(&directory);
    let key = SigningKey::generate(&mut rand::rng());
    let code = state
        .database
        .create_invite(Role::User, invite::DEFAULT_TTL)
        .unwrap()
        .code;

    let relayed = signed(&key, &SERVER_NONCE, &[9u8; 32], Some(&code));
    let decision = auth::decide(&state, &relayed, &SERVER_NONCE, &EXPORTER, PEER).unwrap();
    assert_eq!(
        refusal(decision.result),
        AuthError::BadSignature,
        "a signature made against another connection's exporter must not be accepted"
    );

    let replayed = signed(&key, &[8u8; 32], &EXPORTER, Some(&code));
    let decision = auth::decide(&state, &replayed, &SERVER_NONCE, &EXPORTER, PEER).unwrap();
    assert_eq!(refusal(decision.result), AuthError::BadSignature);

    assert_eq!(
        state.database.user_count().unwrap(),
        0,
        "a bad signature must not reach the database"
    );
}

#[test]
fn a_code_enrolls_once() {
    let directory = TempDir::new().unwrap();
    let state = state(&directory);
    let code = state
        .database
        .create_invite(Role::Manager, invite::DEFAULT_TTL)
        .unwrap()
        .code;

    let first = SigningKey::generate(&mut rand::rng());
    let decision = auth::decide(
        &state,
        &signed(&first, &SERVER_NONCE, &EXPORTER, Some(&code)),
        &SERVER_NONCE,
        &EXPORTER,
        PEER,
    )
    .unwrap();
    let enrolled = decision.user.expect("the code should enroll");
    assert_eq!(enrolled.role, Role::Manager);

    let second = SigningKey::generate(&mut rand::rng());
    let decision = auth::decide(
        &state,
        &signed(&second, &SERVER_NONCE, &EXPORTER, Some(&code)),
        &SERVER_NONCE,
        &EXPORTER,
        PEER,
    )
    .unwrap();
    assert_eq!(refusal(decision.result), AuthError::BadInvite);
    assert_eq!(state.database.user_count().unwrap(), 1);
}

#[test]
fn a_lower_case_code_is_accepted() {
    let directory = TempDir::new().unwrap();
    let state = state(&directory);
    let code = state
        .database
        .create_invite(Role::User, invite::DEFAULT_TTL)
        .unwrap()
        .code;

    let key = SigningKey::generate(&mut rand::rng());
    let decision = auth::decide(
        &state,
        &signed(&key, &SERVER_NONCE, &EXPORTER, Some(&code.to_lowercase())),
        &SERVER_NONCE,
        &EXPORTER,
        PEER,
    )
    .unwrap();
    assert!(decision.user.is_some());
}

#[test]
fn guessing_stops_being_attempted_once_the_budget_is_spent() {
    let directory = TempDir::new().unwrap();
    let state = state(&directory);
    let valid = state
        .database
        .create_invite(Role::User, invite::DEFAULT_TTL)
        .unwrap()
        .code;

    let guesser = SigningKey::generate(&mut rand::rng());
    for guess in 0..invite::FAILURES_PER_ADDRESS {
        let decision = auth::decide(
            &state,
            &signed(
                &guesser,
                &SERVER_NONCE,
                &EXPORTER,
                Some(&format!("AAAA{guess:02}")),
            ),
            &SERVER_NONCE,
            &EXPORTER,
            PEER,
        )
        .unwrap();
        assert_eq!(refusal(decision.result), AuthError::BadInvite);
    }

    let decision = auth::decide(
        &state,
        &signed(&guesser, &SERVER_NONCE, &EXPORTER, Some(&valid)),
        &SERVER_NONCE,
        &EXPORTER,
        PEER,
    )
    .unwrap();
    assert_eq!(
        refusal(decision.result),
        AuthError::BadInvite,
        "a spent budget must refuse even a code that would have worked"
    );
    assert_eq!(state.database.user_count().unwrap(), 0);
}

#[test]
fn an_expired_code_does_not_enroll() {
    let directory = TempDir::new().unwrap();
    let state = state(&directory);
    let code = state
        .database
        .create_invite(Role::User, std::time::Duration::ZERO)
        .unwrap()
        .code;

    let key = SigningKey::generate(&mut rand::rng());
    let decision = auth::decide(
        &state,
        &signed(&key, &SERVER_NONCE, &EXPORTER, Some(&code)),
        &SERVER_NONCE,
        &EXPORTER,
        PEER,
    )
    .unwrap();
    assert_eq!(refusal(decision.result), AuthError::BadInvite);
}
