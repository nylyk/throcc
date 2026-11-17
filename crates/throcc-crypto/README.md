# throcc-crypto

The cryptography of enrollment, sign-in and end-to-end media encryption. It does no I/O.

| Module | Holds |
| --- | --- |
| `pake.rs` | The PAKE behind enrollment with an invite code, bound to the TLS session through its transcript |
| `identity.rs` | Ed25519 identity keys, and the messages they sign to log in, to vouch for an encryption key, to give a name and picture, and to name the address a device reached a server at |
| `hpke.rs` | X25519 encryption keys, and HPKE auth-mode wrapping of a sender secret to one room member. Behind the `media` feature |
| `media.rs` | AES-128-GCM per datagram under a track key derived from the sender secret. Behind the `media` feature |
| `safety.rs` | The 60-digit safety number of two identity keys |
| `transfer.rs` | Sealing an identity export: a random 60-bit code in Crockford base 32, Argon2id over it and a fresh salt, then AES-256-GCM with the header as associated data. Behind the `transfer` feature |

The server builds this crate without the `media` and `transfer` features, so it has neither HPKE nor the media AEAD and can never open media. `cargo test -p throcc-crypto` turns both on through a dev-dependency on the crate itself, so its tests always run. All of these constructions are security-critical: fix real issues with great care and without changing the protocol.
