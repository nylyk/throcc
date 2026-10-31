use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tokio::sync::Notify;

use crate::auth::SourceKey;

/// Holds up to `capacity` tokens and refills `per_second` of them.
pub struct TokenBucket {
    capacity: f64,
    per_second: f64,
    tokens: f64,
    updated: Instant,
}

impl TokenBucket {
    pub fn full(capacity: f64, per_second: f64, now: Instant) -> Self {
        Self {
            capacity,
            per_second,
            tokens: capacity,
            updated: now,
        }
    }

    /// Takes `cost` tokens if the bucket holds that many.
    pub fn take(&mut self, cost: f64, now: Instant) -> bool {
        self.refill(now);
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }

    pub fn is_full(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens >= self.capacity
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        self.updated = now;
    }
}

/// One token bucket per key. Buckets that have refilled completely are forgotten.
pub struct Buckets<K> {
    capacity: f64,
    per_second: f64,
    buckets: HashMap<K, TokenBucket>,
}

impl<K: Hash + Eq + Copy> Buckets<K> {
    pub fn new(capacity: f64, per_second: f64) -> Self {
        Self {
            capacity,
            per_second,
            buckets: HashMap::new(),
        }
    }

    pub fn take(&mut self, key: K, cost: f64, now: Instant) -> bool {
        self.buckets.retain(|_, bucket| !bucket.is_full(now));
        self.buckets
            .entry(key)
            .or_insert_with(|| TokenBucket::full(self.capacity, self.per_second, now))
            .take(cost, now)
    }
}

/// How many holders a [`Gate`] admits at once.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub per_source: usize,
    pub per_network: usize,
    pub total: usize,
}

#[derive(Default)]
struct Counts {
    total: usize,
    sources: HashMap<SourceKey, usize>,
    networks: HashMap<SourceKey, usize>,
}

/// Bounds concurrent holders per source address, per network and in total.
pub struct Gate {
    caps: Caps,
    counts: Mutex<Counts>,
    freed: Notify,
}

impl Gate {
    pub fn new(caps: Caps) -> Arc<Self> {
        Arc::new(Self {
            caps,
            counts: Mutex::default(),
            freed: Notify::new(),
        })
    }

    /// Admits `source` if no cap is reached. The pass is released when dropped.
    pub fn try_enter(self: &Arc<Self>, source: SourceKey) -> Option<Pass> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let network = source.network();
        let full = counts.total >= self.caps.total
            || counts.sources.get(&source).copied().unwrap_or(0) >= self.caps.per_source
            || counts.networks.get(&network).copied().unwrap_or(0) >= self.caps.per_network;
        if full {
            return None;
        }
        counts.total += 1;
        *counts.sources.entry(source).or_default() += 1;
        *counts.networks.entry(network).or_default() += 1;
        Some(Pass {
            gate: self.clone(),
            source,
        })
    }

    /// Waits until `source` is admitted.
    pub async fn enter(self: &Arc<Self>, source: SourceKey) -> Pass {
        loop {
            let mut freed = std::pin::pin!(self.freed.notified());
            freed.as_mut().enable();
            if let Some(pass) = self.try_enter(source) {
                return pass;
            }
            freed.await;
        }
    }

    fn leave(&self, source: SourceKey) {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.total -= 1;
        decrement(&mut counts.sources, source);
        decrement(&mut counts.networks, source.network());
        drop(counts);
        self.freed.notify_waiters();
    }
}

fn decrement(counts: &mut HashMap<SourceKey, usize>, key: SourceKey) {
    if let Some(count) = counts.get_mut(&key) {
        *count -= 1;
        if *count == 0 {
            counts.remove(&key);
        }
    }
}

/// A place in a [`Gate`], held until dropped.
pub struct Pass {
    gate: Arc<Gate>,
    source: SourceKey,
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.gate.leave(self.source);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn source(ip: &str) -> SourceKey {
        SourceKey::of(ip.parse().unwrap())
    }

    #[test]
    fn a_bucket_refills_at_its_rate_up_to_its_capacity() {
        let start = Instant::now();
        let mut bucket = TokenBucket::full(2.0, 1.0, start);
        assert!(bucket.take(1.0, start));
        assert!(bucket.take(1.0, start));
        assert!(!bucket.take(1.0, start));
        assert!(bucket.take(1.0, start + Duration::from_secs(1)));
        assert!(!bucket.take(1.0, start + Duration::from_secs(1)));
        assert!(bucket.is_full(start + Duration::from_secs(60)));
        assert!(!bucket.take(3.0, start + Duration::from_secs(60)));
    }

    #[test]
    fn buckets_are_per_key_and_full_ones_are_forgotten() {
        let start = Instant::now();
        let mut buckets = Buckets::new(1.0, 1.0);
        assert!(buckets.take(1, 1.0, start));
        assert!(!buckets.take(1, 1.0, start));
        assert!(buckets.take(2, 1.0, start));
        assert!(buckets.take(3, 1.0, start + Duration::from_secs(5)));
        assert_eq!(buckets.buckets.len(), 1);
    }

    #[test]
    fn a_gate_caps_each_source_each_network_and_the_total() {
        let gate = Gate::new(Caps {
            per_source: 2,
            per_network: 3,
            total: 4,
        });
        let a = gate.try_enter(source("192.0.2.1")).unwrap();
        let _b = gate.try_enter(source("192.0.2.1")).unwrap();
        assert!(gate.try_enter(source("192.0.2.1")).is_none());
        let _c = gate.try_enter(source("192.0.2.2")).unwrap();
        assert!(gate.try_enter(source("192.0.2.3")).is_none());
        let _d = gate.try_enter(source("198.51.100.1")).unwrap();
        assert!(gate.try_enter(source("203.0.113.1")).is_none());
        drop(a);
        assert!(gate.try_enter(source("192.0.2.1")).is_some());
    }

    #[test]
    fn ipv6_networks_are_48_bits_wide() {
        let gate = Gate::new(Caps {
            per_source: 1,
            per_network: 2,
            total: 10,
        });
        let _a = gate.try_enter(source("2001:db8:1:1::1")).unwrap();
        let _b = gate.try_enter(source("2001:db8:1:2::1")).unwrap();
        assert!(gate.try_enter(source("2001:db8:1:3::1")).is_none());
        assert!(gate.try_enter(source("2001:db8:2:1::1")).is_some());
    }

    #[tokio::test]
    async fn entering_waits_for_a_pass_to_be_dropped() {
        let gate = Gate::new(Caps {
            per_source: 1,
            per_network: 1,
            total: 1,
        });
        let held = gate.try_enter(source("192.0.2.1")).unwrap();
        let waiting = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter(source("192.0.2.1")).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        drop(held);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
    }
}
