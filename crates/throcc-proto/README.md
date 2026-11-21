# throcc-proto

The wire types that the server and clients share. It is plain data and pure functions, with no async runtime, no media and no I/O, so every other crate can depend on it.

| Module | Holds |
| --- | --- |
| `lib.rs` | ALPN `throcc/1`, the default port 8476 and the SNI placeholder |
| `messages.rs` | The control protocol: handshake, requests, responses, events, `PROTOCOL` and the oldest one a server accepts, `MIN_PROTOCOL`, and the name and password rules. Both hellos stay decodable by every version, so mismatched peers can tell which of them is outdated. A sign-in goes `ClientHello`, the PAKE for an enrollment, then the server's `AccountStep` with our user id and the addresses our devices signed for it, our `ProfileStep` (the `SignedProfile` and the address we reached it at, both signed for that server and user), and the `Welcome`. Before a device has pinned the server, the signed address matching the one it typed proves the identity joined that server at that address. `SignedProfile` carries the name, the picture's bytes and the identity key's signature over both for that server and user; `SetProfile` carries the same, and every `Presence` the signature, so other members can check it |
| `framing.rs` | Control-stream framing: a `u32` length and a postcard body, capped at 1 MiB |
| `frame.rs` | The 13-byte media header, its flags and the datagram size budget |
| `ids.rs` | `UserId`, `RoomId`, `MediaId`, `Epoch`, `Hash` and `Role` |
| `permissions.rs` | What each role may do to which other roles. The server enforces it; clients offer only what it allows |
| `invite.rs` | Invite codes such as `K7Q-M2XPD` in Crockford base32, and the invite links that carry one with its server address |
| `fingerprint.rs` | Server and identity fingerprints (SHA-256 of an SPKI) |
| `transport.rs` | QUIC settings both sides use, and the connection close codes |
| `limits.rs` | The limits both sides hold to: 30 people and 5 screen shares in a room, 50 rooms and 250 people signed in on a server, with room for 10 admins past that, server addresses up to 255 bytes and the newest 8 signed addresses per account. The server enforces them; clients refuse a server that goes past them |

Any change to `messages.rs` bumps `PROTOCOL`. A server keeps speaking the protocol before, so a changed message also needs its old shape kept for those sessions; protocol 2 dropped protocol 1 instead, and protocol 3 dropped protocol 2. New enum variants go last, because postcard encodes a variant by its index. The media header in `frame.rs` is security-critical, and changing its layout changes the protocol.
