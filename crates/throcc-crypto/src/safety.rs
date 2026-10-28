use sha2::{Digest, Sha512};
use throcc_proto::messages::PublicKey;

const ITERATIONS: usize = 5200;
const VERSION: [u8; 2] = [0, 0];

/// A 60-digit safety number for two identity keys, the same from either side.
/// Signal's numeric fingerprint construction, with the key as its own stable identifier:
/// `SHA-512(version || key || key)`, then 5200 rounds of `SHA-512(hash || key)`.
pub fn safety_number(a: &PublicKey, b: &PublicKey) -> String {
    let (a, b) = (half(a), half(b));
    if a <= b { a + &b } else { b + &a }
}

/// The safety number in 12 groups of 5 digits.
pub fn groups(number: &str) -> Vec<&str> {
    (0..number.len())
        .step_by(5)
        .filter_map(|i| number.get(i..i + 5))
        .collect()
}

fn half(key: &PublicKey) -> String {
    let mut hash = Sha512::new()
        .chain_update(VERSION)
        .chain_update(key)
        .chain_update(key)
        .finalize();
    for _ in 0..ITERATIONS {
        hash = Sha512::new()
            .chain_update(hash)
            .chain_update(key)
            .finalize();
    }
    hash[..30]
        .chunks(5)
        .map(|chunk| {
            let value = chunk.iter().fold(0u64, |acc, b| acc << 8 | u64::from(*b));
            format!("{:05}", value % 100_000)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting() -> PublicKey {
        std::array::from_fn(|i| i as u8)
    }

    #[test]
    fn matches_independent_vector() {
        let expected = "175484055375002258331488126542671466019735658033095130103269";
        assert_eq!(safety_number(&[1; 32], &counting()), expected);
        assert_eq!(safety_number(&counting(), &[1; 32]), expected);
    }

    #[test]
    fn groups_into_twelve() {
        let number = safety_number(&[1; 32], &[2; 32]);
        let groups = groups(&number);
        assert_eq!(groups.len(), 12);
        assert!(
            groups
                .iter()
                .all(|g| g.len() == 5 && g.bytes().all(|b| b.is_ascii_digit()))
        );
    }
}
