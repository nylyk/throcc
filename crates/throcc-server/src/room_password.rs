use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use throcc_crypto::identity::random;

/// Hashes `password` with Argon2id and a fresh salt, as a PHC string.
pub fn hash(password: &str) -> anyhow::Result<String> {
    let salt: [u8; 16] = random();
    let hash = Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing a room password: {e}"))?;
    Ok(hash.to_string())
}

pub fn matches(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_password_matches_its_hash() {
        let hashed = hash("correct horse").unwrap();
        assert!(matches("correct horse", &hashed));
        assert!(!matches("correct horsE", &hashed));
        assert_ne!(hashed, hash("correct horse").unwrap());
    }
}
