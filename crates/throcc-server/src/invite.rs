use std::collections::HashMap;

use std::net::IpAddr;
use std::time::Duration;

use rand::RngExt as _;
use sha2::{Digest, Sha256};

pub const CODE_LENGTH: usize = 6;
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// 36 does not divide 256, so a bare modulo would skew the first four symbols.
/// 252 is the largest multiple of 36 that fits in a byte.
const REJECT_FROM: u8 = 252;

/// A fresh code of six symbols, roughly 31 bits.
pub fn generate_code() -> String {
    let mut random = rand::rng();
    let mut code = String::with_capacity(CODE_LENGTH);
    while code.len() < CODE_LENGTH {
        let draw: u8 = random.random();
        if draw < REJECT_FROM {
            code.push(ALPHABET[(draw % 36) as usize] as char);
        }
    }
    code
}

/// Stored in the database in place of the code. The code is normalized first,
/// so a lower-case or padded one cannot fail to match.
pub fn hash(code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(code.trim().to_uppercase().as_bytes());
    hasher.finalize().into()
}

pub const FAILURES_PER_ADDRESS: u32 = 5;
/// A per-address limit alone is defeated by rotating addresses, so a server-wide
/// budget backs it up. Exceeding it stops all redemption until enough of it
/// decays back.
pub const SERVER_WIDE_FAILURE_BUDGET: u32 = 100;
/// One recorded failure is forgotten per interval. It bounds a sustained
/// guessing rate at four attempts an hour, and clears honest typos on their own.
pub const DECAY_INTERVAL: Duration = Duration::from_mins(15);

#[derive(Default)]
pub struct RedemptionLimiter {
    failures_by_address: HashMap<IpAddr, u32>,
    total_failures: u32,
}

impl RedemptionLimiter {
    pub fn permits(&self, address: IpAddr) -> bool {
        self.total_failures < SERVER_WIDE_FAILURE_BUDGET
            && self.failures_by_address.get(&address).copied().unwrap_or(0) < FAILURES_PER_ADDRESS
    }

    pub fn record_failure(&mut self, address: IpAddr) {
        let for_address = self.failures_by_address.entry(address).or_default();
        *for_address += 1;
        self.total_failures += 1;
        tracing::warn!(
            %address,
            from_this_address = *for_address,
            server_wide = self.total_failures,
            "invite redemption failed"
        );

        if self.total_failures == SERVER_WIDE_FAILURE_BUDGET {
            tracing::error!(
                "the server-wide invite failure budget is exhausted; \
                 redemption is refused until it decays back"
            );
        }
    }

    /// One failure is forgotten, server-wide and for every address that carries
    /// any. An address at zero is dropped.
    pub fn decay(&mut self) {
        self.total_failures = self.total_failures.saturating_sub(1);
        self.failures_by_address.retain(|_, failures| {
            *failures -= 1;
            *failures > 0
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_six_symbols_from_the_alphabet() {
        for _ in 0..64 {
            let code = generate_code();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.bytes().all(|symbol| ALPHABET.contains(&symbol)));
        }
    }

    #[test]
    fn codes_differ() {
        let first = generate_code();
        assert!((0..16).any(|_| generate_code() != first));
    }

    #[test]
    fn case_and_padding_do_not_change_the_hash() {
        assert_eq!(hash("K7QM2X"), hash("  k7qm2x\n"));
        assert_ne!(hash("K7QM2X"), hash("K7QM2Y"));
    }

    #[test]
    fn every_symbol_is_reachable() {
        let drawn: std::collections::HashSet<u8> = (0..2000)
            .flat_map(|_| generate_code().into_bytes())
            .collect();
        assert_eq!(drawn.len(), ALPHABET.len(), "the alphabet must be uniform");
    }

    #[test]
    fn an_address_is_cut_off_before_the_server_wide_budget() {
        let mut limiter = RedemptionLimiter::default();
        let address: IpAddr = "203.0.113.7".parse().unwrap();
        let other: IpAddr = "203.0.113.8".parse().unwrap();

        for _ in 0..FAILURES_PER_ADDRESS {
            assert!(limiter.permits(address));
            limiter.record_failure(address);
        }
        assert!(!limiter.permits(address));
        assert!(limiter.permits(other));
    }

    #[test]
    fn decay_readmits_an_address_and_forgets_it_once_clear() {
        let mut limiter = RedemptionLimiter::default();
        let address: IpAddr = "203.0.113.7".parse().unwrap();

        for _ in 0..FAILURES_PER_ADDRESS {
            limiter.record_failure(address);
        }
        assert!(!limiter.permits(address));

        limiter.decay();
        assert!(limiter.permits(address));

        for _ in 0..FAILURES_PER_ADDRESS {
            limiter.decay();
        }
        assert!(limiter.failures_by_address.is_empty());
    }

    #[test]
    fn the_server_wide_budget_refuses_every_address() {
        let mut limiter = RedemptionLimiter::default();
        for failure in 0..SERVER_WIDE_FAILURE_BUDGET {
            let address: IpAddr = format!("192.0.2.{}", failure % 200).parse().unwrap();
            limiter.record_failure(address);
        }
        assert!(!limiter.permits("198.51.100.1".parse().unwrap()));

        limiter.decay();
        assert!(limiter.permits("198.51.100.1".parse().unwrap()));
    }
}
