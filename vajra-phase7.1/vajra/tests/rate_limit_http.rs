//! End-to-end rate-limit tests driving `http::process` with a real `Ctx`.
//!
//! These cover what the unit tests in `tests/rate_limit.rs` cannot:
//! the 429 response body actually leaving the server with a
//! `Retry-After` header, the whitelist path bypassing bucket
//! allocation through the request pipeline, and per-path overrides
//! being selected from the real `req.path` string.
//!
//! TODO(day12-followup): a `[access]` deny-list + `[rate_limit]` test
//! asserting that access wins (403 before 429). That needs the
//! `AccessSettings` constructor, which is not part of this crate's
//! public surface yet.

use std::net::IpAddr;

use vajra::cache::Cache;
use vajra::config::{CacheSettings, ProxySettings};
use vajra::date::DATE_LEN;
use vajra::http::{self, Ctx};
use vajra::observe::Observer;
use vajra::proxy::ProxyTable;
use vajra::rate_limit::{RateLimitOverride, RateLimitSettings, RateLimiter};

const DATE: [u8; DATE_LEN] = [b'D'; DATE_LEN];

fn table() -> ProxyTable {
    let p = ProxySettings::simple("/api/", "127.0.0.1:9".parse().unwrap(), false, 5);
    ProxyTable::new(&[p], 1 << 20)
}

/// Extract the HTTP status code from the first response line.
fn status_of(out: &[u8]) -> u16 {
    let line_end = out
        .windows(2)
        .position(|w| w == b"\r\n")
        .expect("no CRLF in response");
    let line = std::str::from_utf8(&out[..line_end]).expect("non-UTF8 status line");
    line.split_whitespace()
        .nth(1)
        .expect("status line has no code")
        .parse()
        .expect("status code not numeric")
}

/// Find a header value by case-insensitive name.
fn header_value<'a>(out: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(out).ok()?;
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(": ") {
            if k.eq_ignore_ascii_case(name) {
                return Some(v);
            }
        }
    }
    None
}

const GET_ROOT: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
const GET_API: &[u8] = b"GET /api/foo HTTP/1.1\r\nHost: x\r\n\r\n";

#[test]
fn fourth_request_after_burst_gets_429_with_retry_after() {
    let table = table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 1,
        burst: 3,
        whitelist_ips: vec![],
        max_tracked_ips: 100,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
        overrides: vec![],
    });
    let ip: IpAddr = "203.0.113.5".parse().unwrap();

    for i in 0..3 {
        let mut ctx = Ctx {
            files: None,
            proxies: &table,
            cache: &mut cache,
            obs: &mut obs,
            now: 10,
            max_body: 1024,
            secure: false,
            client_ip: Some(ip),
            sec: None,
            access: None,
            rate_limit: Some(&mut rl),
        };
        let mut out = Vec::new();
        let _ = http::process(GET_ROOT, &mut out, &DATE, &mut ctx);
        let s = status_of(&out);
        assert_ne!(
            s,
            429,
            "request #{i} unexpectedly rate-limited: {}",
            String::from_utf8_lossy(&out)
        );
    }

    let mut ctx = Ctx {
        files: None,
        proxies: &table,
        cache: &mut cache,
        obs: &mut obs,
        now: 10,
        max_body: 1024,
        secure: false,
        client_ip: Some(ip),
        sec: None,
        access: None,
        rate_limit: Some(&mut rl),
    };
    let mut out = Vec::new();
    let _ = http::process(GET_ROOT, &mut out, &DATE, &mut ctx);

    assert_eq!(
        status_of(&out),
        429,
        "4th request should be 429, got body: {}",
        String::from_utf8_lossy(&out)
    );
    let ra = header_value(&out, "Retry-After").expect("Retry-After header missing");
    let n: u32 = ra.parse().expect("Retry-After not numeric");
    assert!(n >= 1, "Retry-After must be >= 1 second, got {n}");
}

#[test]
fn whitelisted_ip_is_never_denied_and_creates_no_buckets() {
    let table = table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 1,
        burst: 1,
        whitelist_ips: vec!["10.0.0.0/8".parse().unwrap()],
        max_tracked_ips: 100,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
        overrides: vec![],
    });
    let ip: IpAddr = "10.1.2.3".parse().unwrap();

    for _ in 0..50 {
        let mut ctx = Ctx {
            files: None,
            proxies: &table,
            cache: &mut cache,
            obs: &mut obs,
            now: 10,
            max_body: 1024,
            secure: false,
            client_ip: Some(ip),
            sec: None,
            access: None,
            rate_limit: Some(&mut rl),
        };
        let mut out = Vec::new();
        let _ = http::process(GET_ROOT, &mut out, &DATE, &mut ctx);
        assert_ne!(
            status_of(&out),
            429,
            "whitelisted client was throttled: {}",
            String::from_utf8_lossy(&out)
        );
    }
    assert!(
        rl.buckets.is_empty(),
        "whitelisted IP must not populate the bucket map, got {}",
        rl.buckets.len()
    );
}

#[test]
fn per_route_override_has_independent_budget_through_http() {
    let table = table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 100,
        burst: 100,
        whitelist_ips: vec![],
        max_tracked_ips: 100,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
        overrides: vec![RateLimitOverride {
            path: "/api".into(),
            requests_per_second: 1,
            burst: 1,
        }],
    });
    let ip: IpAddr = "198.51.100.7".parse().unwrap();

    // First /api request consumes the 1-token bucket → allowed.
    {
        let mut ctx = Ctx {
            files: None,
            proxies: &table,
            cache: &mut cache,
            obs: &mut obs,
            now: 10,
            max_body: 1024,
            secure: false,
            client_ip: Some(ip),
            sec: None,
            access: None,
            rate_limit: Some(&mut rl),
        };
        let mut out = Vec::new();
        let _ = http::process(GET_API, &mut out, &DATE, &mut ctx);
        // /api is a proxied route here; with no live upstream the
        // response body may be empty. Assert only that the limiter
        // did not emit 429.
        assert!(
            !out.windows(3).any(|w| w == b"429"),
            "first /api request should pass, got: {}",
            String::from_utf8_lossy(&out)
        );
    }

    // Second /api request hits the empty override bucket → 429.
    {
        let mut ctx = Ctx {
            files: None,
            proxies: &table,
            cache: &mut cache,
            obs: &mut obs,
            now: 10,
            max_body: 1024,
            secure: false,
            client_ip: Some(ip),
            sec: None,
            access: None,
            rate_limit: Some(&mut rl),
        };
        let mut out = Vec::new();
        let _ = http::process(GET_API, &mut out, &DATE, &mut ctx);
        assert_eq!(
            status_of(&out),
            429,
            "/api should be exhausted after 1 request: {}",
            String::from_utf8_lossy(&out)
        );
    }

    // Default path has its own 100-token budget → allowed.
    {
        let mut ctx = Ctx {
            files: None,
            proxies: &table,
            cache: &mut cache,
            obs: &mut obs,
            now: 10,
            max_body: 1024,
            secure: false,
            client_ip: Some(ip),
            sec: None,
            access: None,
            rate_limit: Some(&mut rl),
        };
        let mut out = Vec::new();
        let _ = http::process(GET_ROOT, &mut out, &DATE, &mut ctx);
        assert_ne!(
            status_of(&out),
            429,
            "default path should be unaffected by /api exhaustion: {}",
            String::from_utf8_lossy(&out)
        );
    }

    // Two namespaces → two buckets for the same IP.
    assert_eq!(
        rl.buckets.len(),
        2,
        "expected exactly two namespaces, got {}",
        rl.buckets.len()
    );
}
