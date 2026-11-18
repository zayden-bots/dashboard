use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};

use governor::clock::Clock;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use super::AppApiError;

const PRUNE_AT: usize = 4096;
const PRUNE_EVERY: u32 = 256;
const ENTRY_BURST: u32 = 10;
const PROXY_PER_MINUTE: u32 = 60;
const PROXY_BURST: u32 = 20;
const MATCH_REPORTS_PER_HOUR: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    variant_size_differences,
    reason = "a 64-bit IPv6 prefix next to a 32-bit IPv4 address; the key is 16 bytes either way"
)]
pub enum ClientKey {
    V4(Ipv4Addr),
    V6Prefix(u64),
    Unknown,
}

#[must_use]
pub fn client_key(ip: Option<IpAddr>) -> ClientKey {
    match ip {
        Some(IpAddr::V4(addr)) => ClientKey::V4(addr),
        Some(IpAddr::V6(addr)) => addr.to_ipv4_mapped().map_or_else(
            || {
                ClientKey::V6Prefix(
                    u64::try_from(u128::from(addr) >> 64).unwrap_or_default(),
                )
            },
            ClientKey::V4,
        ),
        None => ClientKey::Unknown,
    }
}

pub struct KeyedLimit<K: Hash + Eq + Clone> {
    limiter: DefaultKeyedRateLimiter<K>,
    checks: AtomicU32,
}

impl<K: Hash + Eq + Clone> KeyedLimit<K> {
    #[must_use]
    pub fn new(quota: Quota) -> Self {
        Self { limiter: RateLimiter::keyed(quota), checks: AtomicU32::new(0) }
    }

    pub fn check(&self, key: &K) -> Result<(), AppApiError> {
        let outcome = self.limiter.check_key(key).map_err(|not_until| {
            let wait = not_until.wait_time_from(self.limiter.clock().now());
            let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            AppApiError::RateLimited {
                retry_after_secs: u32::try_from(secs.max(1)).unwrap_or(u32::MAX),
            }
        });
        if self.checks.fetch_add(1, Ordering::Relaxed).is_multiple_of(PRUNE_EVERY)
            && self.limiter.len() > PRUNE_AT
        {
            self.limiter.retain_recent();
        }
        outcome
    }
}

pub struct AppLimits {
    pub device_start: KeyedLimit<ClientKey>,
    pub code_entry: KeyedLimit<i64>,
    pub proxy_miss: KeyedLimit<i64>,
    pub match_report: KeyedLimit<i64>,
}

const fn nonzero(n: u32) -> NonZeroU32 {
    match NonZeroU32::new(n) {
        Some(n) => n,
        None => NonZeroU32::MIN,
    }
}

const fn entry_quota() -> Quota {
    Quota::per_minute(nonzero(1)).allow_burst(nonzero(ENTRY_BURST))
}

const fn proxy_quota() -> Quota {
    Quota::per_minute(nonzero(PROXY_PER_MINUTE)).allow_burst(nonzero(PROXY_BURST))
}

const fn match_report_quota() -> Quota {
    Quota::per_hour(nonzero(MATCH_REPORTS_PER_HOUR))
}

impl AppLimits {
    #[must_use]
    pub fn new() -> Self {
        Self {
            device_start: KeyedLimit::new(entry_quota()),
            code_entry: KeyedLimit::new(entry_quota()),
            proxy_miss: KeyedLimit::new(proxy_quota()),
            match_report: KeyedLimit::new(match_report_quota()),
        }
    }
}

impl Default for AppLimits {
    fn default() -> Self {
        Self::new()
    }
}
