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
    let code = server.bootstrap_invite.clone();

    let admin = connect(&server, &admin_dir, Some(code.clone())).expect("the code should enroll");
    assert_eq!(admin.initial_state().role, Role::Admin);
    assert_eq!(admin.initial_state().users.len(), 1);
    assert_eq!(
        admin.initial_state().placed.room,
        None,
        "entering is explicit"
    );
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

    let first = connect(&server, &client_dir, Some(server.bootstrap_invite.clone())).unwrap();
    let me = first.initial_state().me;
    first.shutdown();

    let again = connect(&server, &client_dir, None).expect("the stored key is on the allowlist");
    assert_eq!(
        again.initial_state().me,
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

    let admin = connect(&server, &admin_dir, Some(server.bootstrap_invite.clone())).unwrap();
    let mut events = admin.events();
    admin.command(Command::CreateInvite).unwrap();

    let code = match next_event(&mut events) {
        Event::Invited { code, .. } => code,
        other => panic!("expected an invite, got {other:?}"),
    };

    let user = connect(&server, &user_dir, Some(code)).expect("the minted code should enroll");
    assert_eq!(user.initial_state().role, Role::User);
    assert_eq!(
        user.initial_state().users.len(),
        2,
        "the roster carries both keys"
    );
    user.shutdown();
    admin.shutdown();
}

#[test]
fn a_user_cannot_mint_a_code() {
    let server = TestServer::start();
    let admin_dir = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();

    let admin = connect(&server, &admin_dir, Some(server.bootstrap_invite.clone())).unwrap();
    let mut minted = admin.events();
    admin.command(Command::CreateInvite).unwrap();
    let code = match next_event(&mut minted) {
        Event::Invited { code, .. } => code,
        other => panic!("expected an invite, got {other:?}"),
    };

    let user = connect(&server, &user_dir, Some(code)).unwrap();
    let mut events = user.events();
    user.command(Command::CreateInvite).unwrap();

    match next_event(&mut events) {
        Event::Failed { message } => assert!(message.contains("Denied"), "unexpected: {message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    user.shutdown();
    admin.shutdown();
}
