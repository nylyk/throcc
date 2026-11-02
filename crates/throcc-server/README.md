# throcc-server

The server: sign-in, rooms and presence, the key relay, and an SFU that forwards encrypted media by its plaintext header. It keeps its state in one data directory: `server_key` and a SQLite database.

## Flow

`Server::run` in `lib.rs` accepts QUIC connections. `session.rs` greets each one and hands it to `auth.rs` to sign in or enroll. After that it runs one control loop for requests and events, and `sfu.rs` runs one ingress loop for the connection's media datagrams. Requests go to the `Hub` in `rooms.rs`, which owns all shared state behind one mutex and writes through to `database.rs`.

## Modules

| Module | Holds |
| --- | --- |
| `main.rs`, `lib.rs` | Command-line arguments, binding, the accept loop and connection caps |
| `identity.rs`, `tls.rs` | `server_key`, the certificate derived from it, and the QUIC configuration |
| `session.rs` | One connection: handshake, control-stream read and write loops |
| `auth.rs` | Sign-in for known keys, and enrollment with an invite through the PAKE |
| `rooms.rs` | The `Hub`: rooms, membership, presence, invites, profiles, key relay, request handling |
| `room_password.rs` | Argon2id hashes of room passwords |
| `limits.rs` | Token buckets and per-source connection caps |
| `sfu.rs` | Ingress: the per-connection rate limit, the ownership check, the forwarding table and keyframe-request merging |
| `egress.rs` | One task per receiver: audio first, video in whole units within a 150 ms budget, link stats |
| `database.rs` | The SQLite schema, its migrations and every query |

The server never depends on codecs, capture, audio or media crypto. `scripts/check-server-deps.sh` checks this. Its integration tests live in `throcc-cli/tests/`.
