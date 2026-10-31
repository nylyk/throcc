use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PakeError {
    #[error("malformed PAKE message")]
    Malformed,
    #[error("key confirmation failed: wrong invite code, or someone is between you and the server")]
    Confirmation,
}

/// Everything both sides must agree on for enrollment to succeed.
pub struct Transcript<'a> {
    pub server_nonce: &'a [u8; 32],
    pub client_nonce: &'a [u8; 32],
    pub code_id: &'a str,
    pub pubkey: &'a [u8; 32],
    pub spki_hash: &'a [u8; 32],
    pub exporter: &'a [u8; 32],
    pub pake_a: &'a [u8],
    pub pake_b: &'a [u8],
}

impl Transcript<'_> {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = b"throcc/enroll/1".to_vec();
        for part in [
            self.server_nonce.as_slice(),
            self.client_nonce,
            self.code_id.as_bytes(),
            self.pubkey,
            self.spki_hash,
            self.exporter,
            self.pake_a,
            self.pake_b,
        ] {
            out.extend_from_slice(&(part.len() as u32).to_be_bytes());
            out.extend_from_slice(part);
        }
        out
    }
}

/// The client's half of a PAKE run, waiting for the server's reply.
pub struct ClientPake {
    state: Spake2<Ed25519Group>,
}

impl ClientPake {
    /// Starts a run with the invite's secret. Returns the state and `pake_a`.
    pub fn start(code_id: &str, secret: &str) -> (Self, Vec<u8>) {
        let (id_a, id_b) = identities(code_id);
        let (state, pake_a) = Spake2::<Ed25519Group>::start_a(
            &Password::new(secret.as_bytes()),
            &Identity::new(&id_a),
            &Identity::new(&id_b),
        );
        (Self { state }, pake_a)
    }

    /// Checks the server's `confirm_b` and returns `confirm_a`.
    pub fn finish(
        self,
        transcript: &Transcript,
        confirm_b: &[u8; 32],
    ) -> Result<[u8; 32], PakeError> {
        let key = Zeroizing::new(
            self.state
                .finish(transcript.pake_b)
                .map_err(|_| PakeError::Malformed)?,
        );
        let keys = ConfirmKeys::derive(&key, transcript);
        verify(&keys.b, transcript, confirm_b)?;
        Ok(mac(&keys.a, transcript))
    }
}

/// The server's half of a PAKE run.
pub struct ServerPake {
    key: Zeroizing<Vec<u8>>,
    pub pake_b: Vec<u8>,
}

impl ServerPake {
    pub fn respond(code_id: &str, secret: &str, pake_a: &[u8]) -> Result<Self, PakeError> {
        let (id_a, id_b) = identities(code_id);
        let (state, pake_b) = Spake2::<Ed25519Group>::start_b(
            &Password::new(secret.as_bytes()),
            &Identity::new(&id_a),
            &Identity::new(&id_b),
        );
        let key = Zeroizing::new(state.finish(pake_a).map_err(|_| PakeError::Malformed)?);
        Ok(Self { key, pake_b })
    }

    pub fn confirm_b(&self, transcript: &Transcript) -> [u8; 32] {
        mac(&ConfirmKeys::derive(&self.key, transcript).b, transcript)
    }

    pub fn check_confirm_a(
        &self,
        transcript: &Transcript,
        confirm_a: &[u8; 32],
    ) -> Result<(), PakeError> {
        verify(
            &ConfirmKeys::derive(&self.key, transcript).a,
            transcript,
            confirm_a,
        )
    }
}

fn identities(code_id: &str) -> (Vec<u8>, Vec<u8>) {
    (
        [b"throcc/client/".as_slice(), code_id.as_bytes()].concat(),
        [b"throcc/server/".as_slice(), code_id.as_bytes()].concat(),
    )
}

struct ConfirmKeys {
    a: Zeroizing<[u8; 32]>,
    b: Zeroizing<[u8; 32]>,
}

impl ConfirmKeys {
    fn derive(pake_key: &[u8], transcript: &Transcript) -> Self {
        let salt = Sha256::digest(transcript.to_bytes());
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), pake_key);
        let mut keys = Self {
            a: Zeroizing::new([0; 32]),
            b: Zeroizing::new([0; 32]),
        };
        hkdf.expand(b"throcc/confirm-a", keys.a.as_mut())
            .expect("32 bytes is a valid HKDF length");
        hkdf.expand(b"throcc/confirm-b", keys.b.as_mut())
            .expect("32 bytes is a valid HKDF length");
        keys
    }
}

fn transcript_hmac(key: &[u8; 32], transcript: &Transcript) -> Hmac<Sha256> {
    <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!())
        .chain_update(transcript.to_bytes())
}

fn mac(key: &[u8; 32], transcript: &Transcript) -> [u8; 32] {
    transcript_hmac(key, transcript)
        .finalize()
        .into_bytes()
        .into()
}

fn verify(key: &[u8; 32], transcript: &Transcript, tag: &[u8; 32]) -> Result<(), PakeError> {
    transcript_hmac(key, transcript)
        .verify_slice(tag)
        .map_err(|_| PakeError::Confirmation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript<'a>(
        pake_a: &'a [u8],
        pake_b: &'a [u8],
        exporter: &'a [u8; 32],
    ) -> Transcript<'a> {
        Transcript {
            server_nonce: &[1; 32],
            client_nonce: &[2; 32],
            code_id: "K7Q",
            pubkey: &[3; 32],
            spki_hash: &[4; 32],
            exporter,
            pake_a,
            pake_b,
        }
    }

    fn run(
        client_secret: &str,
        server_secret: &str,
        client_exporter: [u8; 32],
    ) -> Result<(), PakeError> {
        let (client, pake_a) = ClientPake::start("K7Q", client_secret);
        let server = ServerPake::respond("K7Q", server_secret, &pake_a)?;
        let server_view = transcript(&pake_a, &server.pake_b, &[5; 32]);
        let confirm_b = server.confirm_b(&server_view);
        let client_view = transcript(&pake_a, &server.pake_b, &client_exporter);
        let confirm_a = client.finish(&client_view, &confirm_b)?;
        server.check_confirm_a(&server_view, &confirm_a)
    }

    #[test]
    fn matching_secrets_confirm() {
        assert_eq!(run("M2XPD", "M2XPD", [5; 32]), Ok(()));
    }

    #[test]
    fn wrong_secret_fails_at_the_client() {
        assert_eq!(run("M2XPE", "M2XPD", [5; 32]), Err(PakeError::Confirmation));
    }

    #[test]
    fn different_tls_exporter_fails_at_the_client() {
        assert_eq!(run("M2XPD", "M2XPD", [6; 32]), Err(PakeError::Confirmation));
    }

    #[test]
    fn garbage_pake_a_is_malformed() {
        assert!(matches!(
            ServerPake::respond("K7Q", "M2XPD", &[0; 7]),
            Err(PakeError::Malformed)
        ));
    }

    #[test]
    fn confirmation_tags_are_pinned() {
        let t = transcript(&[6; 32], &[7; 32], &[5; 32]);
        let keys = ConfirmKeys::derive(&[9; 32], &t);
        let hex = |tag: [u8; 32]| tag.map(|b| format!("{b:02x}")).concat();
        assert_eq!(
            hex(mac(&keys.a, &t)),
            "b2b93f7d6ed5a2f63448f19105da720986925d0178d6e2abefbd5470a6eaa3c3"
        );
        assert_eq!(
            hex(mac(&keys.b, &t)),
            "ea8af693a860f75877ec752cac0d6e7b086e0969a426b46105d53522875da3d7"
        );
    }
}
