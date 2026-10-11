//! Unit tests for the token-bucket rate limiter.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use vajra::rate_limit::{Decision, RateLimitSettings, RateLimiter};

fn settings(rps: u32, burst: u32, max: usize) -> RateLimitSettings {
    RateLimitSettings {
        enabled: true,
        requests_per_second: rps,
        burst,
        whitelist_ips: vec![],
        max_tracked_ips: max,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
        overrides: vec![],
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn disabled_limiter_always_allows() {
    let mut rl = RateLimiter::new(RateLimitSettings::default());
    assert!(rl.is_disabled());
    let now = Instant::now();
    for _ in 0..1000 {
        assert_eq!(rl.check(ip("1.2.3.4"), now), Decision::Allow);
    }
}

#[test]
fn burst_then_throttle() {
    let mut rl = RateLimiter::new(settings(1, 3, 100));
    let t0 = Instant::now();
    for _ in 0..3 {
        assert_eq!(rl.check(ip("1.2.3.4"), t0), Decision::Allow);
    }
    match rl.check(ip("1.2.3.4"), t0) {
        Decision::Deny { retry_after_secs } => assert!(retry_after_secs >= 1),
        Decision::Allow => panic!("4th request should be denied"),
    }
}

#[test]
fn refill_over_time() {
    let mut rl = RateLimiter::new(settings(10, 10, 100));
    let t0 = Instant::now();
    for _ in 0..10 {
        assert_eq!(rl.check(ip("1.2.3.4"), t0), Decision::Allow);
    }
    assert!(matches!(rl.check(ip("1.2.3.4"), t0), Decision::Deny { .. }));
    // 200 ms at 10 rps = 2 tokens.
    let t1 = t0 + Duration::from_millis(200);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert!(matches!(rl.check(ip("1.2.3.4"), t1), Decision::Deny { .. }));
}

#[test]
fn burst_cap_enforced() {
    let mut rl = RateLimiter::new(settings(1, 3, 100));
    let t0 = Instant::now();
    assert_eq!(rl.check(ip("1.2.3.4"), t0), Decision::Allow);
    let t1 = t0 + Duration::from_secs(3600);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert!(matches!(rl.check(ip("1.2.3.4"), t1), Decision::Deny { .. }));
}

#[test]
fn whitelisted_ip_not_tracked() {
    let mut cfg = settings(1, 1, 100);
    cfg.whitelist_ips = vec!["10.0.0.0/8".parse().unwrap()];
    let mut rl = RateLimiter::new(cfg);
    let now = Instant::now();
    for _ in 0..100 {
        assert_eq!(rl.check(ip("10.1.2.3"), now), Decision::Allow);
    }
    assert!(
        rl.buckets.is_empty(),
        "whitelisted IP should not be tracked"
    );
}

#[test]
fn retry_after_is_ceil_and_at_least_1() {
    let mut rl = RateLimiter::new(settings(2, 1, 100));
    let t0 = Instant::now();
    assert_eq!(rl.check(ip("1.2.3.4"), t0), Decision::Allow);
    match rl.check(ip("1.2.3.4"), t0) {
        Decision::Deny { retry_after_secs } => assert_eq!(retry_after_secs, 1),
        Decision::Allow => panic!("should deny"),
    }
}

#[test]
fn cap_reached_denies_new_ip() {
    let mut rl = RateLimiter::new(settings(100, 100, 2));
    let now = Instant::now();
    assert_eq!(rl.check(ip("1.1.1.1"), now), Decision::Allow);
    assert_eq!(rl.check(ip("2.2.2.2"), now), Decision::Allow);
    assert_eq!(rl.buckets.len(), 2);
    assert!(matches!(
        rl.check(ip("3.3.3.3"), now),
        Decision::Deny { .. }
    ));
    assert_eq!(rl.check(ip("1.1.1.1"), now), Decision::Allow);
}

#[test]
fn cleanup_evicts_idle_buckets() {
    let mut rl = RateLimiter::new(settings(1, 1, 100));
    let t0 = Instant::now();
    for i in 1..=5u8 {
        let ip = IpAddr::from([10, 0, 0, i]);
        assert_eq!(rl.check(ip, t0), Decision::Allow);
    }
    assert_eq!(rl.buckets.len(), 5);
    let t1 = t0 + Duration::from_secs(100);
    rl.cleanup(t1);
    assert_eq!(rl.buckets.len(), 0);
}

#[test]
fn config_validation() {
    use vajra::rate_limit::{build_rate_limit, RawRateLimit};

    let err = build_rate_limit(RawRateLimit {
        enabled: true,
        requests_per_second: 0,
        burst: 10,
        max_tracked_ips: 100,
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.contains("requests_per_second"), "{err}");

    let err = build_rate_limit(RawRateLimit {
        enabled: true,
        requests_per_second: 10,
        burst: 0,
        max_tracked_ips: 100,
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.contains("burst"), "{err}");

    let err = build_rate_limit(RawRateLimit {
        enabled: true,
        requests_per_second: 10,
        burst: 10,
        max_tracked_ips: 100,
        whitelist_ips: vec!["not-a-cidr".into()],
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.contains("whitelist_ips"), "{err}");
}

#[test]
fn bucket_key_masks_ipv6_only_when_enabled() {
    use vajra::rate_limit::bucket_key;
    let a: IpAddr = "2001:db8::1".parse().unwrap();
    assert_eq!(bucket_key(a, false), a);
    let masked = bucket_key(a, true);
    assert_eq!(masked, "2001:db8::".parse::<IpAddr>().unwrap());
    let v4: IpAddr = "1.2.3.4".parse().unwrap();
    assert_eq!(bucket_key(v4, true), v4);
}

#[test]
fn ipv6_64_aggregation_shares_bucket() {
    let mut cfg = settings(1, 1, 100);
    cfg.aggregate_ipv6_64 = true;
    let mut rl = RateLimiter::new(cfg);
    let t0 = Instant::now();
    let a: IpAddr = "2001:db8::1".parse().unwrap();
    let b: IpAddr = "2001:db8::2".parse().unwrap();
    let c: IpAddr = "2001:db8:0:1::1".parse().unwrap();
    assert_eq!(rl.check(a, t0), Decision::Allow);
    assert!(matches!(rl.check(b, t0), Decision::Deny { .. }));
    assert_eq!(rl.check(c, t0), Decision::Allow);
    assert_eq!(rl.buckets.len(), 2);
}

#[test]
fn ipv6_aggregation_off_by_default() {
    let mut rl = RateLimiter::new(settings(1, 1, 100));
    let t0 = Instant::now();
    let a: IpAddr = "2001:db8::1".parse().unwrap();
    let b: IpAddr = "2001:db8::2".parse().unwrap();
    assert_eq!(rl.check(a, t0), Decision::Allow);
    assert_eq!(rl.check(b, t0), Decision::Allow);
    assert_eq!(rl.buckets.len(), 2);
}

#[test]
fn worker_count_shards_rps() {
    // Two workers: each sees rps/2, so a 4-rps config becomes 2 rps
    // per worker.
    let mut rl = RateLimiter::with_worker_count(settings(4, 4, 100), 2);
    let t0 = Instant::now();
    for _ in 0..4 {
        assert_eq!(rl.check(ip("1.2.3.4"), t0), Decision::Allow);
    }
    assert!(matches!(rl.check(ip("1.2.3.4"), t0), Decision::Deny { .. }));
    // After 500 ms at 2 rps, exactly 1 token is available.
    let t1 = t0 + Duration::from_millis(500);
    assert_eq!(rl.check(ip("1.2.3.4"), t1), Decision::Allow);
    assert!(matches!(rl.check(ip("1.2.3.4"), t1), Decision::Deny { .. }));
}

#[test]
fn per_route_override_has_own_bucket() {
    let mut cfg = settings(10, 10, 100);
    cfg.overrides = vec![vajra::rate_limit::RateLimitOverride {
        path: "/api".into(),
        requests_per_second: 1,
        burst: 1,
    }];
    let mut rl = RateLimiter::new(cfg);
    let t0 = Instant::now();

    // Default path: full 10-token burst, consumed independently.
    for _ in 0..10 {
        assert_eq!(rl.check_with_path(ip("1.2.3.4"), t0, "/"), Decision::Allow);
    }
    assert!(matches!(
        rl.check_with_path(ip("1.2.3.4"), t0, "/"),
        Decision::Deny { .. }
    ));

    // /api override: its own 1-token bucket.
    assert_eq!(
        rl.check_with_path(ip("1.2.3.4"), t0, "/api/foo"),
        Decision::Allow
    );
    assert!(matches!(
        rl.check_with_path(ip("1.2.3.4"), t0, "/api/foo"),
        Decision::Deny { .. }
    ));

    // Two namespaces → two buckets.
    assert_eq!(rl.buckets.len(), 2);
}

#[test]
fn override_longest_prefix_wins() {
    let mut cfg = settings(1, 10, 100);
    cfg.overrides = vec![
        vajra::rate_limit::RateLimitOverride {
            path: "/api".into(),
            requests_per_second: 1,
            burst: 1,
        },
        vajra::rate_limit::RateLimitOverride {
            path: "/api/v2".into(),
            requests_per_second: 1,
            burst: 3,
        },
    ];
    let mut rl = RateLimiter::new(cfg);
    let t0 = Instant::now();

    // /api/v2 has burst 3.
    for _ in 0..3 {
        assert_eq!(
            rl.check_with_path(ip("9.9.9.9"), t0, "/api/v2/x"),
            Decision::Allow
        );
    }
    assert!(matches!(
        rl.check_with_path(ip("9.9.9.9"), t0, "/api/v2/x"),
        Decision::Deny { .. }
    ));

    // /api/other has burst 1.
    assert_eq!(
        rl.check_with_path(ip("9.9.9.9"), t0, "/api/other"),
        Decision::Allow
    );
    assert!(matches!(
        rl.check_with_path(ip("9.9.9.9"), t0, "/api/other"),
        Decision::Deny { .. }
    ));
}

#[test]
fn config_validation_overrides() {
    use vajra::rate_limit::{build_rate_limit, RawRateLimit, RawRateLimitOverride};

    // Missing leading slash
    let err = build_rate_limit(RawRateLimit {
        enabled: true,
        requests_per_second: 1,
        burst: 1,
        max_tracked_ips: 1,
        overrides: vec![RawRateLimitOverride {
            path: "api".into(),
            requests_per_second: 1,
            burst: 1,
        }],
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.contains("path must start"), "{err}");

    // Zero rps in override
    let err = build_rate_limit(RawRateLimit {
        enabled: true,
        requests_per_second: 1,
        burst: 1,
        max_tracked_ips: 1,
        overrides: vec![RawRateLimitOverride {
            path: "/api".into(),
            requests_per_second: 0,
            burst: 1,
        }],
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.contains("overrides[0].requests_per_second"), "{err}");
}
