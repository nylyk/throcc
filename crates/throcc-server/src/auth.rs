use std::net::IpAddr;

use anyhow::Result;
use ed25519_dalek::{Signature, VerifyingKey};
use quinn::Connection;
use throcc_proto::auth::{
    EXPORTER_BYTES, EXPORTER_CONTEXT, EXPORTER_LABEL, NONCE_BYTES, signing_input,
};
use throcc_proto::{Auth, AuthError, User};

use crate::State;
use crate::database::Admission;

pub enum Decision {
    /// The roster comes from the transaction that admitted the user, so it cannot
    /// shift between admitting and answering.
    Admitted {
        user: User,
        users: Vec<User>,
        enrolled: bool,
    },
    Refused(AuthError),
}

pub fn keying_material(connection: &Connection) -> Result<[u8; EXPORTER_BYTES]> {
    let mut material = [0u8; EXPORTER_BYTES];
    connection
        .export_keying_material(&mut material, EXPORTER_LABEL, EXPORTER_CONTEXT)
        .map_err(|_| anyhow::anyhow!("the connection exports no keying material"))?;
    Ok(material)
}

/// The signature is checked before the database is touched. The key is then
/// matched against the allowlist, or the invite presented with it is redeemed.
pub fn decide(
    state: &State,
    auth: &Auth,
    server_nonce: &[u8; NONCE_BYTES],
    exporter: &[u8; EXPORTER_BYTES],
    peer: IpAddr,
) -> Result<Decision> {
    if !signature_is_valid(auth, server_nonce, exporter) {
        return Ok(refused(AuthError::BadSignature));
    }

    let invite_code = auth.invite_code.as_deref();
    if invite_code.is_some() && !state.redemptions().permits(peer) {
        tracing::warn!(%peer, "refusing redemption: the failure budget is exhausted");
        return Ok(refused(AuthError::BadInvite));
    }

    match state.database.admit(&auth.pubkey, invite_code)? {
        Admission::NotAllowlisted => Ok(refused(AuthError::UnknownKey)),
        Admission::InviteRefused => {
            state.redemptions().record_failure(peer);
            Ok(refused(AuthError::BadInvite))
        }
        Admission::Admitted {
            user,
            users,
            enrolled,
        } => Ok(Decision::Admitted {
            user,
            users,
            enrolled,
        }),
    }
}

fn refused(error: AuthError) -> Decision {
    tracing::info!(?error, "refused a client");
    Decision::Refused(error)
}

fn signature_is_valid(
    auth: &Auth,
    server_nonce: &[u8; NONCE_BYTES],
    exporter: &[u8; EXPORTER_BYTES],
) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&auth.pubkey) else {
        return false;
    };
    key.verify_strict(
        &signing_input(server_nonce, &auth.client_nonce, exporter),
        &Signature::from_bytes(&auth.signature),
    )
    .is_ok()
}
