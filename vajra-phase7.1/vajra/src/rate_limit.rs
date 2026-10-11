//! Token-bucket rate limiting.
//! See `docs/rate-limiting.md` for the design rationale.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use ipnet::IpNet;
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimitSettings {
    pub enabled: bool,
    pub requests_per_second: u32,
    pub burst: u32,
    pub whitelist_ips: Vec<IpNet>,
    pub max_tracked_ips: usize,
    pub cleanup_interval_secs: u64,
}

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

pub fn build_rate_limit(raw: RawRateLimit) -> Result<RateLimitSettings, String> {
    let whitelist_ips = raw
        .whitelist_ips
        .into_iter()
        .map(|s| {
            crate::access::parse_one_net(&s)
                .map_err(|e| format!("[rate_limit] whitelist_ips: invalid CIDR {s:?}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

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

#[derive(Debug)]
pub struct Bucket {
    pub tokens: f64,
    pub last_refill: Instant,
    pub last_seen: Instant,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { retry_after_secs: u32 },
}

#[derive(Debug)]
pub struct RateLimiter {
    pub settings: RateLimitSettings,
    pub buckets: HashMap<IpAddr, Bucket>,
    pub last_cleanup: Instant,
}

impl RateLimiter {
    pub fn new(settings: RateLimitSettings) -> Self {
        let cap = settings.max_tracked_ips.min(4096);
        Self {
            settings,
            buckets: HashMap::with_capacity(cap),
            last_cleanup: Instant::now(),
        }
    }

    pub fn is_disabled(&self) -> bool {
        !self.settings.enabled
    }

    pub fn is_whitelisted(&self, ip: IpAddr) -> bool {
        let ip = crate::access::normalize(ip);
        self.settings
            .whitelist_ips
            .iter()
            .any(|n| crate::access::contains(n, ip))
    }

    pub fn check(&mut self, ip: IpAddr, now: Instant) -> Decision {
        if self.is_disabled() {
            return Decision::Allow;
        }
        if self.settings.cleanup_interval_secs > 0
            && now.duration_since(self.last_cleanup).as_secs()
                >= self.settings.cleanup_interval_secs
        {
            self.cleanup(now);
        }

        if self.is_whitelisted(ip) {
            return Decision::Allow;
        }

        let rps = self.settings.requests_per_second as f64;
        let burst = self.settings.burst as f64;

        if !self.buckets.contains_key(&ip) && self.buckets.len() >= self.settings.max_tracked_ips {
            return Decision::Deny {
                retry_after_secs: 1,
            };
        }

        let bucket = self.buckets.entry(ip).or_insert_with(|| Bucket {
            tokens: burst,
            last_refill: now,
            last_seen: now,
        });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rps).min(burst);
        bucket.last_refill = now;
        bucket.last_seen = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision::Allow
        } else {
            let needed = 1.0 - bucket.tokens;
            let retry = (needed / rps).ceil() as u32;
            Decision::Deny {
                retry_after_secs: retry.max(1),
            }
        }
    }

    pub fn cleanup(&mut self, now: Instant) {
        self.last_cleanup = now;
        let rps = self.settings.requests_per_second as f64;
        let burst = self.settings.burst as f64;
        if rps <= 0.0 {
            return;
        }
        let threshold_secs = 2.0 * burst / rps;
        self.buckets
            .retain(|_, b| now.duration_since(b.last_seen).as_secs_f64() < threshold_secs);
    }
}
