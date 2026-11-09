//! Pins the encoded bytes of control messages. Postcard encodes enum variants by index and
//! fields by position, so a reordering would break deployed peers while still round-tripping.

use throcc_proto::framing;
use throcc_proto::ids::{Epoch, Hash, MediaId, Role, RoomId, UserId};
use throcc_proto::messages::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn pinned<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
    message: T,
    expected: &str,
) {
    let frame = framing::encode(&message).unwrap();
    let body = &frame[framing::LEN_BYTES..];
    assert_eq!(hex(body), expected, "{message:?}");
    assert_eq!(framing::decode::<T>(body).unwrap(), message);
}

#[test]
fn control_messages_encode_to_pinned_bytes() {
    pinned(
        ServerHello {
            nonce: [5; 32],
            min_protocol: 1,
            max_protocol: 300,
        },
        "050505050505050505050505050505050505050505050505050505050505050501ac02",
    );
    pinned(
        ClientHello {
            protocol: 1,
            sign_in: SignIn::Auth {
                pubkey: [1; 32],
                client_nonce: [2; 32],
                want_room: Some(RoomId(3)),
                signature: [4; 64],
            },
        },
        "01000101010101010101010101010101010101010101010101010101010101010101020202020202020202020202020202020202020202020202020202020202020201034004040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404040404",
    );
    pinned(
        ClientMessage::Request {
            id: 7,
            request: Request::JoinRoom {
                room: RoomId(3),
                password: Some("pw".into()),
            },
        },
        "0007000301027077",
    );
    pinned(
        ClientMessage::Request {
            id: 8,
            request: Request::LeaveRoom,
        },
        "000801",
    );
    pinned(
        ClientMessage::Request {
            id: 300,
            request: Request::SetMedia(MediaState {
                mic: true,
                deafened: false,
                share: Some(ShareConfig {
                    audio: true,
                    kbps: 8000,
                    codec: Codec::Vp9,
                    width: 2560,
                    height: 1440,
                    fps: 60,
                }),
            }),
        },
        "00ac020601000101c03e038014a00b3c",
    );
    pinned(
        ClientMessage::Request {
            id: 1,
            request: Request::RequestKeyframe {
                media_id: MediaId(5),
                kind: Recovery::Idr,
            },
        },
        "0001080501",
    );
    pinned(
        ClientMessage::Request {
            id: 1,
            request: Request::KickFromRoom {
                user: UserId(2),
                room: RoomId(3),
            },
        },
        "0001140203",
    );
    pinned(
        ServerMessage::Response {
            id: 9,
            response: Response::Err {
                code: ErrorCode::WrongPassword,
                message: "no".into(),
            },
        },
        "00090107026e6f",
    );
    pinned(
        ServerMessage::Event(Event::PresenceChanged(Presence {
            user: UserId(2),
            name: "a".into(),
            avatar: Some(Hash([6; 32])),
            room: None,
            mic: true,
            deafened: false,
            sharing: true,
            role: Role::Admin,
        })),
        "01000201610106060606060606060606060606060606060606060606060606060606060606060001000102",
    );
    pinned(
        ServerMessage::Event(Event::KickedFromRoom {
            by: UserId(1),
            room: RoomId(2),
        }),
        "01100102",
    );
    pinned(AuthResult::Err(AuthError::RateLimited), "0105");
    pinned(
        Event::PeersChanged {
            room: RoomId(1),
            epoch: Epoch(2),
            peers: vec![Peer {
                user: UserId(3),
                tracks: Tracks {
                    mic: MediaId(4),
                    shares: vec![ShareTracks {
                        video: MediaId(5),
                        audio: MediaId(6),
                    }],
                },
                share: None,
                identity_key: [7; 32],
                encryption_key: SignedEncryptionKey {
                    key: [8; 32],
                    not_after: 1_700_000_000,
                    sig: [9; 64],
                },
            }],
        },
        "060102010304010506000707070707070707070707070707070707070707070707070707070707070707080808080808080808080808080808080808080808080808080808080808080880e2cfaa064009090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909090909",
    );
}

#[test]
fn a_hello_from_any_version_shows_its_protocol() {
    let future = [0xac, 0x02, 0xff, 0xff, 0xff];
    assert_eq!(ClientHello::protocol_of(&future).unwrap(), 300);
    assert!(framing::decode::<ClientHello>(&future).is_err());
    assert!(ClientHello::protocol_of(&[]).is_err());
}

#[test]
fn stored_role_ranks_are_pinned() {
    for (role, rank) in [(Role::User, 0), (Role::Manager, 1), (Role::Admin, 2)] {
        assert_eq!(role.rank(), rank);
        assert_eq!(Role::from_rank(rank), Some(role));
    }
    assert_eq!(Role::from_rank(3), None);
}

#[test]
fn hash_displays_as_the_avatar_cache_file_name() {
    assert_eq!(
        Hash::of(b"").to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
