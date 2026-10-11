//! Token-bucket rate limiting.
//!
//! **Status: design skeleton.** The types and signatures are final; the
//! hot-path body of [`RateLimiter::check`] and the `http.rs` wiring land
//! in Day 12. See `docs/rate-limiting.md` for the design rationale.
//!
//! Nothing in this file is called from the request path yet.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use ipnet::IpNet;
use serde::Deserialize;

/// Runtime configuration. `Default::default()` yields a disabled limiter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimitSettings {
    /// Master switch. If `false`, every `check()` returns `Allow`.
    pub enabled: bool,
    /// Sustained refill rate, tokens per second, per worker.
    pub requests_per_second: u32,
    /// Bucket capacity, i.e. maximum burst above the sustained rate.
    pub burst: u32,
    /// Client IPs exempt from limiting. Matched against the *effective*
    /// client IP, i.e. after trusted-proxy / XFF resolution.
    pub whitelist_ips: Vec<IpNet>,
    /// Hard cap on the number of buckets kept per worker. When reached,
    /// new client IPs are denied. See design doc §"Memory bounds".
    pub max_tracked_ips: usize,
    /// Interval between lazy cleanup sweeps. `0` disables the sweep.
    pub cleanup_interval_secs: u64,
}

/// TOML shape for `[rate_limit]`.
#[derive(Debug, Default, Deserialize)]
pub struct RawRateLimit {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub requests_per_second: u32,
    #[serde(default)]
    pub burst: u32,
    #[serde(default)]
    pub whitelist_ips: Vec<String>,
    #[serde(default)]
    pub max_tracked_ips: usize,
    #[serde(default)]
    pub cleanup_interval_secs: u64,
}

/// Parse and validate `[rate_limit]`. Called from `config.rs`.
pub fn build_rate_limit(raw: RawRateLimit) -> Result<RateLimitSettings, String> {
    fn parse_list(field: &str, list: Vec<String>) -> Result<Vec<IpNet>, String> {
        list.into_iter()
            .map(|s| {
                crate::access::parse_one_net(&s)
                    .map_err(|e| format!("[rate_limit] {field}: invalid CIDR {s:?}: {e}"))
            })
            .collect()
    }

    let whitelist_ips = parse_list("whitelist_ips", raw.whitelist_ips)?;

    if raw.enabled {
        if raw.requests_per_second == 0 {
            return Err("[rate_limit] requests_per_second must be >= 1 when enabled".into());
        }
        if raw.burst == 0 {
            return Err("[rate_limit] burst must be >= 1 when enabled".into());
        }
        if raw.max_tracked_ips == 0 {
            return Err("[rate_limit] max_tracked_ips must be >= 1 when enabled".into());
        }
    }

    Ok(RateLimitSettings {
        enabled: raw.enabled,
        requests_per_second: raw.requests_per_second,
        burst: raw.burst,
        whitelist_ips,
        max_tracked_ips: raw.max_tracked_ips,
        cleanup_interval_secs: raw.cleanup_interval_secs,
    })
}

/// One bucket per tracked client IP. Refilled lazily on each `check`.
#[derive(Debug)]
pub struct Bucket {
    /// Fractional so refills don't quantize below 1 token/s.
    pub tokens: f64,
    /// Monotonic time of the last refill.
    pub last_refill: Instant,
    /// Monotonic time of the last request, used by cleanup.
    pub last_seen: Instant,
}

/// Outcome of a single `check`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Request is within budget.
    Allow,
    /// Request is over budget. The caller emits 429 + `Retry-After`.
    Deny { retry_after_secs: u32 },
}

/// Per-worker token-bucket state.
#[derive(Debug)]
pub struct RateLimiter {
    pub settings: RateLimitSettings,
    /// Keyed on the effective client IP.
    pub buckets: HashMap<IpAddr, Bucket>,
    /// Timestamp of the last cleanup sweep.
    pub last_cleanup: Instant,
}

impl RateLimiter {
    /// Build a fresh limiter. Allocates the map with a capacity hint
    /// derived from `max_tracked_ips`.
    ///
    /// Day 12.
    pub fn new(_settings: RateLimitSettings) -> Self {
        todo!("Day 12")
    }

    /// Fast-path skip: `true` iff `enabled == false`.
    pub fn is_disabled(&self) -> bool {
        !self.settings.enabled
    }

    /// True if `ip` matches the whitelist.
    ///
    /// Day 12.
    pub fn is_whitelisted(&self, _ip: IpAddr) -> bool {
        todo!("Day 12")
    }

    /// Consume one token for `ip`, or deny.
    ///
    /// Refill math (see design doc §"Algorithm"):
    ///
    /// ```text
    /// tokens = min(burst, tokens + elapsed * rps)
    /// if tokens >= 1.0 { tokens -= 1.0; Allow }
    /// else { Deny { retry_after_secs: ceil((1 - tokens) / rps) } }
    /// ```
    ///
    /// Also triggers `cleanup(now)` on the configured interval.
    ///
    /// Day 12.
    pub fn check(&mut self, _ip: IpAddr, _now: Instant) -> Decision {
        todo!("Day 12")
    }

    /// Evict buckets whose `last_seen` is older than `2 * (burst / rps)`
    /// seconds — a bucket that would already be full again if kept.
    ///
    /// Day 12.
    pub fn cleanup(&mut self, _now: Instant) {
        todo!("Day 12")
    }
}

#[cfg(test)]
mod tests {
    // Test matrix is documented in docs/rate-limiting.md §"Testing plan".
    // Bodies land in Day 12.
}
