use std::net::SocketAddr;

use throcc_proto::ids::{Hash, RoomId, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{AuthError, Request, Response, valid_name};
use throcc_proto::{DEFAULT_PORT, Fingerprint};
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

    /// What went wrong signing in to `address`, in a sentence for the user.
    pub fn explain(&self, address: &str) -> String {
        match self {
            Self::NotEnrolled(address) => {
                format!("You haven’t joined {address} yet. Ask someone there for an invite code.")
            }
            Self::Login(LoginError::Refused(AuthError::BadInvite) | LoginError::Pake(_)) => {
                "That code didn’t work. It may be mistyped, used up or expired, or something between you and the server isn’t the server. Ask for a fresh one.".into()
            }
            Self::Login(LoginError::Refused(AuthError::RateLimited)) => {
                "Too many wrong codes from your network. Wait ten minutes, then try again.".into()
            }
            Self::Login(LoginError::Refused(AuthError::ServerFull)) => {
                "The server is full right now. Try again in a while.".into()
            }
            Self::Login(LoginError::Refused(AuthError::UnknownKey)) => {
                "This server doesn’t know you anymore. Join again with a fresh invite code.".into()
            }
            Self::Connect(ConnectError::Resolve(address)) => {
                format!("Couldn’t find {address}. Check the address.")
            }
            Self::Connect(ConnectError::Connection(_)) => unreachable_hint(address),
            other => other.to_string(),
        }
    }
}

/// Why the server at `address` didn't answer, and what to check.
pub fn unreachable_hint(address: &str) -> String {
    let port = port_of(address);
    format!("Couldn’t reach the server. Check the address, and that UDP port {port} is open.")
}

/// The port a connection to `address` uses: the one it names, else the default.
fn port_of(address: &str) -> u16 {
    if let Ok(socket) = address.parse::<SocketAddr>() {
        return socket.port();
    }
    match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => port.parse().unwrap_or(DEFAULT_PORT),
        _ => DEFAULT_PORT,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_the_addresses_own_or_the_default() {
        assert_eq!(port_of("voice.example.net"), DEFAULT_PORT);
        assert_eq!(port_of("voice.example.net:9000"), 9000);
        assert_eq!(port_of("192.0.2.40:9001"), 9001);
        assert_eq!(port_of("[2001:db8::1]:9002"), 9002);
        assert_eq!(port_of("2001:db8::1"), DEFAULT_PORT);
        assert_eq!(port_of("192.0.2.40"), DEFAULT_PORT);
        assert!(unreachable_hint("host:9000").contains("UDP port 9000"));
    }
}
