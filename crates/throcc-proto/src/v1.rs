use serde::Serialize;

use crate::framing::{self, FrameError};
use crate::ids::{Hash, Role, RoomId, UserId};
use crate::messages::{
    AuthResult, Event, Placement, Presence, Room, ServerMessage, ServerProfile, UserProfile,
    Welcome,
};

/// What protocol 1 tells a connection that another one replaced.
pub const SIGNED_IN_ELSEWHERE: &str = "you signed in from somewhere else";

/// The close code protocol 1 gets for [`SIGNED_IN_ELSEWHERE`], having no code of its own.
pub const SIGNED_IN_ELSEWHERE_CLOSE: u32 = crate::transport::close::REMOVED;

/// Postcard's index of `AuthResult::Ok`.
const AUTH_OK: u32 = 0;
/// Postcard's index of `ServerMessage::Event`.
const EVENT: u32 = 1;
/// Postcard's index of `Event::PresenceChanged`.
const PRESENCE_CHANGED: u32 = 0;

/// The frame of `message` in the shapes of protocol 1: a presence without its identity
/// key, and `RemovedFromServer` in place of `SignedInElsewhere`.
pub fn encode_server_message(message: &ServerMessage) -> Result<Vec<u8>, FrameError> {
    let ServerMessage::Event(event) = message else {
        return framing::encode(message);
    };
    match event {
        Event::PresenceChanged(presence) => {
            framing::encode(&(EVENT, PRESENCE_CHANGED, PresenceV1::from(presence)))
        }
        Event::SignedInElsewhere => {
            framing::encode(&ServerMessage::Event(Event::RemovedFromServer {
                reason: SIGNED_IN_ELSEWHERE.to_owned(),
            }))
        }
        Event::PresenceEnded(_)
        | Event::RoomCreated(_)
        | Event::RoomRenamed { .. }
        | Event::RoomDeleted(_)
        | Event::RoomLockChanged { .. }
        | Event::PeersChanged { .. }
        | Event::SenderKeyPublished { .. }
        | Event::KeyframeRequested { .. }
        | Event::ReceiveStats { .. }
        | Event::UserAdded(_)
        | Event::RoleChanged { .. }
        | Event::UserRemoved(_)
        | Event::InviteSpent { .. }
        | Event::RemovedFromServer { .. }
        | Event::ServerProfileChanged(_)
        | Event::KickedFromRoom { .. }
        | Event::AudiencesChanged(_) => framing::encode(message),
    }
}

/// The frame of `result` in the shapes of protocol 1: presences without identity keys.
pub fn encode_auth_result(result: &AuthResult) -> Result<Vec<u8>, FrameError> {
    match result {
        AuthResult::Ok(welcome) => framing::encode(&(AUTH_OK, WelcomeV1::from(&**welcome))),
        AuthResult::Err(_) => framing::encode(result),
    }
}

#[derive(Serialize)]
struct PresenceV1<'a> {
    user: UserId,
    name: &'a str,
    avatar: Option<Hash>,
    room: Option<RoomId>,
    mic: bool,
    deafened: bool,
    sharing: bool,
    role: Role,
}

impl<'a> From<&'a Presence> for PresenceV1<'a> {
    fn from(presence: &'a Presence) -> Self {
        Self {
            user: presence.user,
            name: &presence.name,
            avatar: presence.avatar,
            room: presence.room,
            mic: presence.mic,
            deafened: presence.deafened,
            sharing: presence.sharing,
            role: presence.role,
        }
    }
}

#[derive(Serialize)]
struct WelcomeV1<'a> {
    me: UserId,
    role: Role,
    rooms: &'a [Room],
    presence: Vec<PresenceV1<'a>>,
    placement: &'a Placement,
    server: &'a ServerProfile,
    profile: &'a UserProfile,
}

impl<'a> From<&'a Welcome> for WelcomeV1<'a> {
    fn from(welcome: &'a Welcome) -> Self {
        Self {
            me: welcome.me,
            role: welcome.role,
            rooms: &welcome.rooms,
            presence: welcome.presence.iter().map(PresenceV1::from).collect(),
            placement: &welcome.placement,
            server: &welcome.server,
            profile: &welcome.profile,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Epoch;
    use crate::messages::Audiences;

    fn presence() -> Presence {
        Presence {
            user: UserId(7),
            name: "Zoë".into(),
            avatar: Some(Hash([9; 32])),
            room: Some(RoomId(3)),
            mic: true,
            deafened: false,
            sharing: true,
            role: Role::Manager,
            identity_key: [5; 32],
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The frames below are what a protocol 1 server wrote for the same messages.
    #[test]
    fn a_protocol_1_session_gets_the_frames_protocol_1_defined() {
        let changed = ServerMessage::Event(Event::PresenceChanged(presence()));
        assert_eq!(
            hex(&encode_server_message(&changed).unwrap()),
            "0000002f010007045a6fc3ab01090909090909090909090909090909090909090909090909090909\
             0909090909010301000101"
        );

        let welcome = Welcome {
            me: UserId(7),
            role: Role::Admin,
            rooms: vec![Room {
                id: RoomId(3),
                name: "Lobby".into(),
                locked: true,
            }],
            presence: vec![presence()],
            placement: Placement {
                room: None,
                epoch: Epoch(0),
                tracks: None,
                peers: vec![],
                audiences: Audiences::default(),
            },
            server: ServerProfile {
                name: Some("owls".into()),
                avatar: None,
            },
            profile: UserProfile {
                name: "Zoë".into(),
                avatar: None,
            },
        };
        assert_eq!(
            hex(&encode_auth_result(&AuthResult::Ok(Box::new(welcome))).unwrap()),
            "0000004d0007020103054c6f626279010107045a6fc3ab0109090909090909090909090909090909\
             0909090909090909090909090909090901030100010100000000000001046f776c7300045a6fc3ab\
             00"
        );

        let replaced = ServerMessage::Event(Event::SignedInElsewhere);
        assert_eq!(
            hex(&encode_server_message(&replaced).unwrap()),
            "00000024010e21796f75207369676e656420696e2066726f6d20736f6d65776865726520656c7365"
        );
    }

    #[test]
    fn what_protocol_2_left_alone_is_framed_alike() {
        let ended = ServerMessage::Event(Event::PresenceEnded(UserId(7)));
        assert_eq!(
            encode_server_message(&ended).unwrap(),
            framing::encode(&ended).unwrap()
        );
        assert_eq!(hex(&framing::encode(&ended).unwrap()), "00000003010107");
    }
}
