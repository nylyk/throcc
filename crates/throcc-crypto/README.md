# throcc-crypto

The cryptography of enrollment, sign-in and end-to-end media encryption. It does no I/O.

| Module | Holds |
| --- | --- |
| `pake.rs` | The PAKE behind enrollment with an invite code, bound to the TLS session through its transcript |
| `identity.rs` | Ed25519 identity keys, and the messages they sign to log in and to vouch for an encryption key |
| `hpke.rs` | X25519 encryption keys, and HPKE auth-mode wrapping of a sender secret to one room member |
| `media.rs` | AES-128-GCM per datagram under a track key derived from the sender secret. Behind the `media` feature |
| `safety.rs` | The 60-digit safety number of two identity keys |

The server builds this crate without the `media` feature, so it can never open media. The media AEAD and track-key derivation, HPKE sender-key wrapping and its info, the signed encryption key and the safety number are frozen; changing one needs a human decision.
