//! Rate-limit smoke tests exercising the public surface.
//! Full server-integrated tests land once the harness is extracted.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use vajra::rate_limit::{Decision, RateLimitSettings, RateLimiter};

#[test]
fn full_burst_over_time_simulates_a_client() {
    let mut rl = RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 1,
        burst: 3,
        whitelist_ips: vec![],
        max_tracked_ips: 10,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
    });
    let t0 = Instant::now();
    let ip: IpAddr = "9.9.9.9".parse().unwrap();
    for _ in 0..3 {
        assert_eq!(rl.check(ip, t0), Decision::Allow);
    }
    assert!(matches!(rl.check(ip, t0), Decision::Deny { .. }));
    let t1 = t0 + Duration::from_secs(1);
    assert_eq!(rl.check(ip, t1), Decision::Allow);
    assert!(matches!(rl.check(ip, t1), Decision::Deny { .. }));
}

#[test]
fn distinct_ips_get_independent_buckets() {
    let mut rl = RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 1,
        burst: 1,
        whitelist_ips: vec![],
        max_tracked_ips: 10,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
    });
    let now = Instant::now();
    let a: IpAddr = "1.1.1.1".parse().unwrap();
    let b: IpAddr = "2.2.2.2".parse().unwrap();
    assert_eq!(rl.check(a, now), Decision::Allow);
    assert_eq!(rl.check(b, now), Decision::Allow);
    assert!(matches!(rl.check(a, now), Decision::Deny { .. }));
    assert!(matches!(rl.check(b, now), Decision::Deny { .. }));
}
