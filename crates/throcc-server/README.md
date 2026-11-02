# throcc-server

The server: sign-in, rooms and presence, the key relay, and an SFU that forwards encrypted media by its plaintext header. It keeps its state in one data directory: `server_key` and a SQLite database.

## Flow

`Server::run` in `lib.rs` accepts QUIC connections. `session.rs` greets each one and hands it to `auth.rs` to sign in or enroll. After that it runs one control loop for requests and events, and `sfu.rs` runs one ingress loop for the connection's media datagrams. Requests go to the `Hub` in `rooms.rs`, which owns all shared state behind one mutex and writes through to `database.rs`.

## Modules

| Module | Holds |
| --- | --- |
| `main.rs`, `lib.rs` | Command-line arguments, binding, the accept loop and connection caps, and `admin_invite` behind `throcc-server admin-invite` |
| `files.rs` | The data directory and its private files: created at 0700 and 0600, tightened if they are looser, refused if another user owns them |
| `identity.rs`, `tls.rs` | `server_key`, the certificate derived from it, and the QUIC configuration |
| `session.rs` | One connection: handshake, control-stream read and write loops |
| `auth.rs` | Sign-in for known keys, and enrollment with an invite through the PAKE |
| `rooms.rs` | The `Hub`: rooms, membership, presence, invites, profiles, key relay, request handling |
| `room_password.rs` | Argon2id hashes of room passwords, which run on the blocking pool, a few at a time, never under the hub lock; and the backoff for wrong guesses |
| `limits.rs` | Token buckets, per-source connection caps, the new-connection rate and the enrollment limiter, which counts failed runs per source, network and region |
| `refusals.rs` | Counts of connections refused or failed before sign-in, logged every 10 s instead of one line each |
| `sfu.rs` | Ingress: the per-connection rate limits, the per-track audio allowance, the ownership check, the forwarding table and keyframe-request merging |
| `egress.rs` | One task per receiver: audio first, a turn per track, then video in whole units within a 150 ms budget; link stats |
| `database.rs` | The SQLite schema, its migrations and every query. WAL with `synchronous=FULL`, so a commit survives a power loss and a room's epoch never rolls back |

## Admin invites

The server never mints an invite by itself. `throcc-server admin-invite [--data <dir>]` calls `admin_invite` in `lib.rs`, which opens the data directory's `server_key` and database next to a server that may be running, stores an invite without a minter, and prints:

```
invite       MNR-WFFAV
expires      2026-11-03 09:39 UTC
fingerprint  69bc f041 9c8c ad2a 61dc e085 1c32 e666 6dc3 b538 dcdc bf42 840d bcce ed7e fbb4
```

An invite without a minter enrolls an admin; it works once, lasts a day and has the usual three tries and rate limits. The running server reads invites from the database at each enrollment, so it accepts the invite without a restart. The command refuses a data directory where the server hasn't started yet instead of creating one: a mistyped `--data` would otherwise print an invite and a fingerprint no server knows. It never creates or migrates the database, and refuses one whose schema version differs from its own. Both connections wait up to 5 s for each other's writes, and write transactions take the write lock up front, so neither fails on the other's commit.

## Media limits

Ingress polices each sender; egress shares each receiver's link. A well-behaved client never reaches any of these.

- A connection that sends more than 25 Mbps is closed.
- Datagrams beyond 6,000 a second per connection are dropped and counted. 25 Mbps of full-size datagrams is about 2,600.
- Each audio track may send 150 packets and 80 KiB a second, with bursts of 2,500 packets and 256 KiB. That covers Opus at its 510 kbps ceiling, and a flush of a client's whole 64 KiB datagram send buffer after a stall. Packets over it are dropped and counted, and the connection stays open. This is admission policing of a sender that doesn't conform, not an egress drop policy.
- Egress never drops audio by policy. Each receiver queues audio per track and sends one packet per track in turn. Its 2,000-packet cap guards against a receiver that stops reading; at the cap the longest track's oldest packet goes, so a track that floods only loses its own packets.
- Egress drops video in whole units. Beyond the 150 ms budget it drops only units that haven't started sending. A unit that has started is dropped only at the 8 MiB hard cap, a guard against a receiver that stops reading, or when it can't arrive whole anyway: one of its fragments is lost, none comes for a second, or it grows past 4,096 fragments.
- Work per packet is logarithmic in the queue, and the periodic sweeps are bounded, so tiny datagrams can't make a receiver's task expensive.

The server never depends on codecs, capture, audio or media crypto. `scripts/check-server-deps.sh` checks this. Its integration tests live in `throcc-cli/tests/`.
