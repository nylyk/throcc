# throcc-proto

The wire types that the server and clients share. It is plain data and pure functions, with no async runtime, no media and no I/O, so every other crate can depend on it.

| Module | Holds |
| --- | --- |
| `lib.rs` | ALPN `throcc/1`, the default port 8476 and the SNI placeholder |
| `messages.rs` | The control protocol: handshake, requests, responses, events, `PROTOCOL` and the oldest one a server accepts, `MIN_PROTOCOL`, and the name and password rules. Both hellos stay decodable by every version, so mismatched peers can tell which of them is outdated |
| `v1.rs` | The frames a protocol 1 session gets for what protocol 2 changed: a presence without its identity key, and `RemovedFromServer` in place of `SignedInElsewhere`. Its tests pin the bytes a protocol 1 server wrote |
| `framing.rs` | Control-stream framing: a `u32` length and a postcard body, capped at 1 MiB |
| `frame.rs` | The 13-byte media header, its flags and the datagram size budget |
| `ids.rs` | `UserId`, `RoomId`, `MediaId`, `Epoch`, `Hash` and `Role` |
| `permissions.rs` | What each role may do to which other roles. The server enforces it; clients offer only what it allows |
| `invite.rs` | Invite codes such as `K7Q-M2XPD` in Crockford base32 |
| `fingerprint.rs` | Server and identity fingerprints (SHA-256 of an SPKI) |
| `transport.rs` | QUIC settings both sides use, and the connection close codes |
| `limits.rs` | The limits both sides hold to: 30 people and 5 screen shares in a room, 50 rooms and 250 people signed in on a server, with room for 10 admins past that. The server enforces them; clients refuse a server that goes past them |

Any change to `messages.rs` bumps `PROTOCOL`. A server keeps speaking the protocol before, so a changed message also needs its old shape in a module like `v1.rs`. New enum variants go last, because postcard encodes a variant by its index. The media header in `frame.rs` is security-critical, and changing its layout changes the protocol.
