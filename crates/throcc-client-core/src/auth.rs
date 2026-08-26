use ed25519_dalek::{Signer as _, SigningKey};
use quinn::Connection;
use rand::RngExt as _;
use throcc_proto::auth::{
    EXPORTER_BYTES, EXPORTER_CONTEXT, EXPORTER_LABEL, NONCE_BYTES, signing_input,
};
use throcc_proto::{
    Auth, AuthResult, PROTOCOL_VERSION, Placed, Role, Room, ServerHello, User, UserId,
};

use crate::control::{ControlReader, ControlWriter};
use crate::{Error, Result};

/// Everything `AuthResult::Ok` carries: enough to render without a second fetch.
#[derive(Debug, Clone, PartialEq)]
pub struct Welcome {
    pub me: UserId,
    pub role: Role,
    pub users: Vec<User>,
    pub rooms: Vec<Room>,
    pub placed: Placed,
}

/// The server's hello is read, and this client then proves ownership of its
/// identity key over this exact connection.
pub async fn handshake(
    connection: &Connection,
    writer: &mut ControlWriter,
    reader: &mut ControlReader,
    identity: &SigningKey,
    invite_code: Option<String>,
) -> Result<Welcome> {
    let hello: ServerHello = reader
        .read()
        .await?
        .ok_or_else(|| Error::Protocol("the control stream closed before the hello".into()))?;
    if hello.protocol != PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "server speaks protocol {}, this client speaks {PROTOCOL_VERSION}",
            hello.protocol
        )));
    }

    let client_nonce: [u8; NONCE_BYTES] = rand::rng().random();
    let signature = identity.sign(&signing_input(
        &hello.server_nonce,
        &client_nonce,
        &keying_material(connection)?,
    ));

    writer
        .write(&Auth {
            pubkey: identity.verifying_key().to_bytes(),
            client_nonce,
            invite_code,
            want_room: None,
            signature: signature.to_bytes(),
        })
        .await?;

    match reader.read().await? {
        Some(AuthResult::Ok {
            me,
            role,
            users,
            rooms,
            placed,
        }) => Ok(Welcome {
            me,
            role,
            users,
            rooms,
            placed,
        }),
        Some(AuthResult::Err(error)) => Err(Error::Rejected(error)),
        None => Err(Error::Protocol(
            "the control stream closed before the authentication result".into(),
        )),
    }
}

fn keying_material(connection: &Connection) -> Result<[u8; EXPORTER_BYTES]> {
    let mut material = [0u8; EXPORTER_BYTES];
    connection
        .export_keying_material(&mut material, EXPORTER_LABEL, EXPORTER_CONTEXT)
        .map_err(|_| Error::Protocol("the connection exports no keying material".into()))?;
    Ok(material)
}
