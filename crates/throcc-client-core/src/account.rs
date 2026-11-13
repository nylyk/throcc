use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Request, Response, valid_name};
use tracing::warn;

use crate::connection::{self, ConnectError, Connected};
use crate::control::{self, ControlLink, LinkError, LoginError, Requester};
use crate::floors::EpochFloors;
use crate::keystore::{IdentityMissing, Keystore};

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
    EncryptionKey(String),
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

    /// Whether the identity the known servers know us by is gone from the keychain.
    pub fn identity_missing(&self) -> bool {
        matches!(self, Self::Store(e) if e.is::<IdentityMissing>())
    }
}

/// Signs in to a server this install has enrolled with, then publishes the encryption key.
pub async fn sign_in(
    keystore: &Keystore,
    address: &str,
    want_room: Option<RoomId>,
) -> Result<ControlLink, SignInError> {
    let connected = reach(keystore, address).await?;
    sign_in_over(keystore, connected, want_room).await
}

/// Connects to a server this install has enrolled with, checking its pinned key. Nobody
/// is signed in yet, so no other device of ours is pushed off.
pub async fn reach(keystore: &Keystore, address: &str) -> Result<Connected, SignInError> {
    let pin = keystore
        .known_servers
        .get(address)
        .ok_or_else(|| SignInError::NotEnrolled(address.to_owned()))?;
    Ok(connection::connect(address, Some(pin)).await?)
}

/// Signs in over `connected`, then publishes the encryption key.
pub async fn sign_in_over(
    keystore: &Keystore,
    connected: Connected,
    want_room: Option<RoomId>,
) -> Result<ControlLink, SignInError> {
    let link = control::login(connected, keystore.identity(), want_room).await?;
    let server = &link.connected.fingerprint;
    publish_encryption_key(keystore, &link.requester, server, link.welcome.me).await?;
    Ok(link)
}

/// Enrolls with an invite code, as `name` if the account is new, under a new encryption key,
/// and pins the server the PAKE proved.
pub async fn enroll(
    keystore: &mut Keystore,
    address: &str,
    code: &InviteCode,
    name: &str,
) -> Result<ControlLink, SignInError> {
    // The user id the server gives may have stood for an earlier account of ours, as on a
    // server rebuilt with the same key. No sender key recorded for that account opens with a
    // new encryption key.
    keystore.renew_encryption_key()?;
    let connected = connection::connect(address, None).await?;
    let mut link = control::enroll(connected, keystore.identity(), code, name).await?;
    let server = &link.connected.fingerprint;
    keystore.known_servers.trust(address, *server)?;
    // The invite is spent by now, so this can't fail the enrollment. Floors left behind
    // only keep rooms silent until they pass them; the new encryption key already keeps
    // the old account's sender keys shut.
    if let Err(e) = EpochFloors::forget(keystore.dir(), server, link.welcome.me) {
        warn!("dropping the epoch floors of an earlier account: {e:#}");
    }
    publish_encryption_key(keystore, &link.requester, server, link.welcome.me).await?;
    sign_profile_if_kept(keystore, &mut link, valid_name(name).unwrap_or(name), None).await;
    Ok(link)
}

/// Signs `name` and `avatar` if they are the profile the server keeps for us and it holds
/// no signature for it yet, so others can tell we chose them.
pub async fn sign_profile_if_kept(
    keystore: &Keystore,
    link: &mut ControlLink,
    name: &str,
    avatar: Option<Hash>,
) {
    let kept = &link.welcome.profile;
    if kept.sig.is_some() || (kept.name.as_str(), kept.avatar) != (name, avatar) {
        return;
    }
    let server = &link.connected.fingerprint;
    let sig = keystore.profile_sig(server, link.welcome.me, name, avatar);
    let request = Request::SetProfile {
        name: name.to_owned(),
        avatar,
        sig,
    };
    match link.requester.request(request).await {
        Ok(Response::Ok) => link.welcome.profile.sig = Some(sig),
        other => warn!("signing our profile: {other:?}"),
    }
}

/// Gives `server` our encryption key, signed for it and for us as `me`.
pub(crate) async fn publish_encryption_key(
    keystore: &Keystore,
    requester: &Requester,
    server: &Fingerprint,
    me: UserId,
) -> Result<(), SignInError> {
    let signed = keystore.signed_encryption_key(server, me);
    let response = requester.request(Request::SetEncryptionKey(signed)).await?;
    control::expect_ok(response).map_err(SignInError::EncryptionKey)
}
