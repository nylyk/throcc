use throcc_proto::Fingerprint;
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::Request;

use crate::connection::{self, ConnectError};
use crate::control::{self, Link, LinkError, LoginError, Requester};
use crate::floors::EpochFloors;
use crate::keystore::Keystore;

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("you haven't joined {0} yet; you need an invite code")]
    NotEnrolled(String),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Login(#[from] LoginError),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("the server refused the encryption key: {0}")]
    EncKey(String),
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

impl SignInError {
    /// Both fingerprints, when the server's key differs from the pinned one.
    pub fn pin_mismatch(&self) -> Option<(Fingerprint, Fingerprint)> {
        match self {
            Self::Connect(ConnectError::PinMismatch { expected, actual }) => {
                Some((*expected, *actual))
            }
            _ => None,
        }
    }
}

/// Signs in to a server this install has enrolled with, then publishes the encryption key.
pub async fn sign_in(
    keystore: &Keystore,
    address: &str,
    want_room: Option<RoomId>,
) -> Result<Link, SignInError> {
    let pin = keystore
        .known_servers
        .get(address)
        .ok_or_else(|| SignInError::NotEnrolled(address.to_owned()))?;
    let connected = connection::connect(address, Some(pin)).await?;
    let link = control::login(connected, keystore.identity(), want_room).await?;
    let server = &link.connected.fingerprint;
    publish_enc_key(keystore, &link.requester, server, link.welcome.me).await?;
    Ok(link)
}

/// Enrolls with an invite code and pins the server the PAKE proved. Epoch floors kept for
/// that server go: they were for an earlier account, as on a server rebuilt with the same
/// key, whose user ids and rooms now stand for others.
pub async fn enroll(
    keystore: &mut Keystore,
    address: &str,
    code: &InviteCode,
) -> Result<Link, SignInError> {
    let connected = connection::connect(address, None).await?;
    let link = control::enroll(connected, keystore.identity(), code).await?;
    let server = &link.connected.fingerprint;
    keystore.known_servers.trust(address, *server)?;
    EpochFloors::forget(keystore.dir(), server)?;
    publish_enc_key(keystore, &link.requester, server, link.welcome.me).await?;
    Ok(link)
}

/// Gives `server` our encryption key, signed for it and for us as `me`.
pub(crate) async fn publish_enc_key(
    keystore: &Keystore,
    requester: &Requester,
    server: &Fingerprint,
    me: UserId,
) -> Result<(), SignInError> {
    let signed = keystore.signed_enc_key(server, me);
    let response = requester.request(Request::SetEncKey(signed)).await?;
    control::expect_ok(response).map_err(SignInError::EncKey)
}
