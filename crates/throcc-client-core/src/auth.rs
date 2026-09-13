use ed25519_dalek::{Signer as _, SigningKey};
use quinn::Connection;
use rand::RngExt as _;
use throcc_proto::auth::{
    NONCE_BYTES, TLS_CHANNEL_BINDING_BYTES, TLS_CHANNEL_BINDING_CONTEXT, TLS_CHANNEL_BINDING_LABEL,
    signing_input,
};
use throcc_proto::{Auth, AuthResult, InitialState, PROTOCOL_VERSION, ServerHello};

use crate::control::{ControlReader, ControlWriter};
use crate::{Error, Result};

/// The server's hello is read, and this client then proves ownership of its
/// identity key over this exact connection.
pub async fn handshake(
    connection: &Connection,
    writer: &mut ControlWriter,
    reader: &mut ControlReader,
    identity: &SigningKey,
    invite_code: Option<String>,
) -> Result<InitialState> {
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
        &tls_channel_binding(connection)?,
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
        Some(AuthResult::Ok(initial_state)) => Ok(initial_state),
        Some(AuthResult::Err(error)) => Err(Error::Rejected(error)),
        None => Err(Error::Protocol(
            "the control stream closed before the authentication result".into(),
        )),
    }
}

fn tls_channel_binding(connection: &Connection) -> Result<[u8; TLS_CHANNEL_BINDING_BYTES]> {
    let mut material = [0u8; TLS_CHANNEL_BINDING_BYTES];
    connection
        .export_keying_material(
            &mut material,
            TLS_CHANNEL_BINDING_LABEL,
            TLS_CHANNEL_BINDING_CONTEXT,
        )
        .map_err(|_| Error::Protocol("the connection exports no keying material".into()))?;
    Ok(material)
}
