mod common;

use common::{SERVER_LABEL, TestServer, keystore, next_event};
use tempfile::TempDir;
use throcc_client_core::{Client, Command, Error, Event};
use throcc_proto::{AuthError, Role};

fn connect(
    server: &TestServer,
    directory: &TempDir,
    invite: Option<String>,
) -> Result<Client, Error> {
    Client::connect(server.address, SERVER_LABEL, keystore(directory), invite)
}

#[test]
fn a_key_that_is_not_allowlisted_is_refused() {
    let server = TestServer::start();
    let client_dir = TempDir::new().unwrap();

    match connect(&server, &client_dir, None).err() {
        Some(Error::Rejected(AuthError::UnknownKey)) => {}
        other => panic!("expected an unknown key to be refused, got {other:?}"),
    }
}

#[test]
fn the_bootstrap_code_enrolls_one_admin_and_is_then_spent() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let second_dir = TempDir::new().unwrap();
    let code = server.bootstrap_invite();

    let admin = connect(&server, &admin_dir, Some(code.clone())).expect("the code should enroll");
    assert_eq!(admin.welcome().role, Role::Admin);
    assert_eq!(admin.welcome().users.len(), 1);
    assert_eq!(admin.welcome().placed.room, None, "entering is explicit");
    admin.shutdown();

    match connect(&server, &second_dir, Some(code)).err() {
        Some(Error::Rejected(AuthError::BadInvite)) => {}
        other => panic!("a spent code must not enroll a second key, got {other:?}"),
    }
}

#[test]
fn an_enrolled_key_reconnects_without_a_code() {
    let server = TestServer::start();
    let client_dir = TempDir::new().unwrap();

    let first = connect(&server, &client_dir, Some(server.bootstrap_invite())).unwrap();
    let me = first.welcome().me;
    first.shutdown();

    let again = connect(&server, &client_dir, None).expect("the stored key is on the allowlist");
    assert_eq!(
        again.welcome().me,
        me,
        "a UserId belongs to the key forever"
    );
    again.shutdown();
}

#[test]
fn an_admin_mints_a_code_that_enrolls_a_second_user() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = connect(&server, &admin_dir, Some(server.bootstrap_invite())).unwrap();
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

    let user = connect(&server, &user_dir, Some(code)).expect("the minted code should enroll");
    assert_eq!(user.welcome().role, Role::User);
    assert_eq!(
        user.welcome().users.len(),
        2,
        "the roster carries both keys"
    );
    user.shutdown();
    admin.shutdown();
}

#[test]
fn an_admin_cannot_mint_its_own_rank() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();

    let admin = connect(&server, &admin_dir, Some(server.bootstrap_invite())).unwrap();
    let mut events = admin.events();
    admin
        .command(Command::CreateInvite {
            role: Role::Admin,
            ttl_secs: 0,
        })
        .unwrap();

    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("Denied"), "unexpected: {message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    admin.shutdown();
}
