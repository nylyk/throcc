use std::net::IpAddr;

use anyhow::Result;
use ed25519_dalek::{Signature, VerifyingKey};
use quinn::Connection;
use throcc_proto::auth::{
    NONCE_BYTES, TLS_CHANNEL_BINDING_BYTES, TLS_CHANNEL_BINDING_CONTEXT, TLS_CHANNEL_BINDING_LABEL,
    signing_input,
};
use throcc_proto::{Auth, AuthError, Epoch, InitialState, Placed, User};

use crate::State;
use crate::database::Admission;

pub enum Decision {
    Admitted {
        user: User,
        initial_state: Box<InitialState>,
    },
    Refused(AuthError),
}

pub fn tls_channel_binding(connection: &Connection) -> Result<[u8; TLS_CHANNEL_BINDING_BYTES]> {
    let mut material = [0u8; TLS_CHANNEL_BINDING_BYTES];
    connection
        .export_keying_material(
            &mut material,
            TLS_CHANNEL_BINDING_LABEL,
            TLS_CHANNEL_BINDING_CONTEXT,
        )
        .map_err(|_| anyhow::anyhow!("the connection exports no keying material"))?;
    Ok(material)
}

/// The signature is checked before the database is touched. The key is then
/// matched against the allowlist, or the invite presented with it is redeemed.
pub fn decide(
    state: &State,
    auth: &Auth,
    server_nonce: &[u8; NONCE_BYTES],
    tls_channel_binding: &[u8; TLS_CHANNEL_BINDING_BYTES],
    peer: IpAddr,
) -> Result<Decision> {
    if !signature_is_valid(auth, server_nonce, tls_channel_binding) {
        return Ok(refused(AuthError::BadSignature));
    }

    let invite_code = auth.invite_code.as_deref();
    if invite_code.is_some() && !state.redemption_limiter().permits(peer) {
        tracing::warn!(%peer, "refusing redemption: the failure budget is exhausted");
        return Ok(refused(AuthError::BadInvite));
    }

    match state.database.admit(&auth.pubkey, invite_code)? {
        Admission::NotAllowlisted => Ok(refused(AuthError::UnknownKey)),
        Admission::InviteRefused => {
            state.redemption_limiter().record_failure(peer);
            Ok(refused(AuthError::BadInvite))
        }
        Admission::Admitted {
            user,
            users,
            enrolled,
        } => {
            if enrolled {
                tracing::info!(user = %user.id, role = ?user.role, "enrolled a new user");
            }
            if let Some(room) = auth.want_room {
                tracing::debug!(%room, "no such room; placing in no room");
            }
            Ok(Decision::Admitted {
                initial_state: Box::new(InitialState {
                    me: user.id,
                    role: user.role,
                    users,
                    rooms: Vec::new(),
                    placed: Placed {
                        room: None,
                        epoch: Epoch(0),
                        tracks: None,
                        peers: Vec::new(),
                    },
                }),
                user,
            })
        }
    }
}

fn refused(error: AuthError) -> Decision {
    tracing::info!(?error, "refused a client");
    Decision::Refused(error)
}

fn signature_is_valid(
    auth: &Auth,
    server_nonce: &[u8; NONCE_BYTES],
    tls_channel_binding: &[u8; TLS_CHANNEL_BINDING_BYTES],
) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&auth.pubkey) else {
        return false;
    };
    key.verify_strict(
        &signing_input(server_nonce, &auth.client_nonce, tls_channel_binding),
        &Signature::from_bytes(&auth.signature),
    )
    .is_ok()
}
