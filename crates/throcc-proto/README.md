# throcc-proto

The wire types that the server and clients share. It is plain data and pure functions, with no async runtime, no media and no I/O, so every other crate can depend on it.

| Module | Holds |
| --- | --- |
| `lib.rs` | ALPN `throcc/1`, the default port 8476 and the SNI placeholder |
| `messages.rs` | The control protocol: handshake, requests, responses, events, and `PROTOCOL`, its version |
| `framing.rs` | Control-stream framing: a `u32` length and a postcard body, capped at 1 MiB |
| `frame.rs` | The 13-byte media header, its flags and the datagram size budget |
| `ids.rs` | `UserId`, `RoomId`, `MediaId`, `Epoch`, `Hash` and `Role` |
| `invite.rs` | Invite codes such as `K7Q-M2XPD` in Crockford base32 |
| `fingerprint.rs` | Server and identity fingerprints (SHA-256 of an SPKI) |
| `transport.rs` | QUIC settings both sides use, and the connection close codes |

Any change to `messages.rs` bumps `PROTOCOL`. New enum variants go last, because postcard encodes a variant by its index. The media header in `frame.rs` is frozen.
