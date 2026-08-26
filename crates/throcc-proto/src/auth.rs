pub const AUTH_DOMAIN: &[u8] = b"throcc-auth-v1";

pub const EXPORTER_LABEL: &[u8] = b"throcc-auth-exporter-v1";
pub const EXPORTER_CONTEXT: &[u8] = b"";
pub const EXPORTER_BYTES: usize = 32;

pub const NONCE_BYTES: usize = 32;
pub const SIGNING_INPUT_BYTES: usize =
    AUTH_DOMAIN.len() + NONCE_BYTES + NONCE_BYTES + EXPORTER_BYTES;

/// The bytes an `Auth` signature covers. Both sides must build them identically.
pub fn signing_input(
    server_nonce: &[u8; NONCE_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
    exporter: &[u8; EXPORTER_BYTES],
) -> [u8; SIGNING_INPUT_BYTES] {
    let mut input = [0u8; SIGNING_INPUT_BYTES];
    let mut written = 0;
    for part in [AUTH_DOMAIN, server_nonce, client_nonce, exporter] {
        input[written..written + part.len()].copy_from_slice(part);
        written += part.len();
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_element_reaches_the_signed_bytes() {
        let input = signing_input(&[1u8; 32], &[2u8; 32], &[3u8; 32]);
        assert!(input.starts_with(AUTH_DOMAIN));
        assert_eq!(input[AUTH_DOMAIN.len()..AUTH_DOMAIN.len() + 32], [1u8; 32]);
        assert_eq!(input[input.len() - 32..], [3u8; 32]);
    }

    #[test]
    fn swapping_the_nonces_changes_the_signed_bytes() {
        assert_ne!(
            signing_input(&[1u8; 32], &[2u8; 32], &[3u8; 32]),
            signing_input(&[2u8; 32], &[1u8; 32], &[3u8; 32])
        );
    }
}
