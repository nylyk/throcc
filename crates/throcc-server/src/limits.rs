use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::lock;

const SOURCE_RUNS: usize = 10;
const NETWORK_RUNS: usize = 30;
const SOURCE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How often [`Buckets`] forgets its full buckets.
const BUCKET_SWEEP: Duration = Duration::from_secs(1);
/// How often [`SourceLimiter`] forgets sources whose runs have all left the window. The
/// sources it counts expire their own runs on every check.
const RUN_SWEEP: Duration = Duration::from_secs(60);

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
    next_sweep: Option<Instant>,
}

impl<K: Hash + Eq + Copy> Buckets<K> {
    pub fn new(capacity: f64, per_second: f64) -> Self {
        Self {
            capacity,
            per_second,
            buckets: HashMap::new(),
            next_sweep: None,
        }
    }

    pub fn take(&mut self, key: K, cost: f64, now: Instant) -> bool {
        // A full bucket behaves like a missing one, so forgetting it late changes nothing.
        if self.next_sweep.is_none_or(|t| now >= t) {
            self.buckets.retain(|_, bucket| !bucket.is_full(now));
            self.next_sweep = Some(now + BUCKET_SWEEP);
        }
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
        let mut counts = lock(&self.counts);
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
        let mut counts = lock(&self.counts);
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

/// A source address for rate limiting: the full IPv4 address, or the IPv6 /64.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKey {
    V4(u32),
    V6(u64),
}

impl SourceKey {
    pub fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(v4.into()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4.into()),
                None => Self::V6((u128::from(v6) >> 64) as u64),
            },
        }
    }

    /// The network around this source: its IPv4 /24, or its IPv6 /48.
    pub fn network(self) -> Self {
        match self {
            Self::V4(v4) => Self::V4(v4 & !0xff),
            Self::V6(prefix) => Self::V6(prefix & !0xffff),
        }
    }
}

/// Counts enrollment runs per source and per network in a sliding window. Successful runs
/// are forgiven.
#[derive(Default)]
pub struct SourceLimiter {
    sources: RunWindow,
    networks: RunWindow,
    next_sweep: Option<Instant>,
}

impl SourceLimiter {
    /// Records the start of a run, or returns false if the source or its network is over
    /// its limit.
    pub fn begin(&mut self, source: SourceKey, now: Instant) -> bool {
        if self.next_sweep.is_none_or(|t| now >= t) {
            self.sources.expire_all(now);
            self.networks.expire_all(now);
            self.next_sweep = Some(now + RUN_SWEEP);
        }
        let network = source.network();
        if self.sources.count(source, now) >= SOURCE_RUNS
            || self.networks.count(network, now) >= NETWORK_RUNS
        {
            return false;
        }
        self.sources.record(source, now);
        self.networks.record(network, now);
        true
    }

    pub fn forgive(&mut self, source: SourceKey) {
        self.sources.forgive(source);
        self.networks.forgive(source.network());
    }
}

#[derive(Default)]
struct RunWindow(HashMap<SourceKey, VecDeque<Instant>>);

impl RunWindow {
    fn expire_all(&mut self, now: Instant) {
        self.0.retain(|_, times| {
            expire(times, now);
            !times.is_empty()
        });
    }

    /// Runs of `key` still in the window.
    fn count(&mut self, key: SourceKey, now: Instant) -> usize {
        self.0.get_mut(&key).map_or(0, |times| {
            expire(times, now);
            times.len()
        })
    }

    fn record(&mut self, key: SourceKey, now: Instant) {
        self.0.entry(key).or_default().push_back(now);
    }

    fn forgive(&mut self, key: SourceKey) {
        if let Some(times) = self.0.get_mut(&key) {
            times.pop_back();
        }
    }
}

fn expire(times: &mut VecDeque<Instant>, now: Instant) {
    while times
        .front()
        .is_some_and(|t| now.saturating_duration_since(*t) > SOURCE_WINDOW)
    {
        times.pop_front();
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn ipv6_sources_group_by_64() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(SourceKey::of(a), SourceKey::of(b));
        assert_ne!(SourceKey::of(a), SourceKey::of(c));
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(SourceKey::of(mapped), SourceKey::of(v4));
    }

    #[test]
    fn limiter_blocks_after_repeated_failures() {
        let now = Instant::now();
        let mut limiter = SourceLimiter::default();
        let failing = source("192.0.2.1");
        for _ in 0..SOURCE_RUNS {
            assert!(limiter.begin(failing, now));
        }
        assert!(!limiter.begin(failing, now));
        limiter.forgive(failing);
        assert!(limiter.begin(failing, now));
        assert!(limiter.begin(source("192.0.2.2"), now));
    }

    #[test]
    fn limiter_caps_a_network_of_many_sources() {
        let now = Instant::now();
        let mut limiter = SourceLimiter::default();
        let sources = (0..NETWORK_RUNS as u16).map(|i| source(&format!("2001:db8:1:{i:x}::1")));
        for source in sources {
            assert!(limiter.begin(source, now));
        }
        assert!(!limiter.begin(source("2001:db8:1:ffff::1"), now));
        assert!(limiter.begin(source("2001:db8:2::1"), now));
        for last in 0..NETWORK_RUNS as u8 {
            limiter.begin(source(&format!("198.51.100.{last}")), now);
        }
        assert!(!limiter.begin(source("198.51.100.200"), now));
    }

    #[test]
    fn runs_leave_the_window_and_idle_sources_are_forgotten() {
        let start = Instant::now();
        let mut limiter = SourceLimiter::default();
        let busy = source("192.0.2.1");
        for _ in 0..SOURCE_RUNS {
            assert!(limiter.begin(busy, start));
        }
        for last in 0..100u8 {
            limiter.begin(source(&format!("203.0.113.{last}")), start);
        }
        let later = start + SOURCE_WINDOW + Duration::from_secs(1);
        assert!(limiter.begin(busy, later));
        assert_eq!(limiter.sources.0.len(), 1);
        assert_eq!(limiter.networks.0.len(), 1);
    }
}
