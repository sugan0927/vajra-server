//! Trusted-proxy / XFF integration for the rate limiter.
//!
//! When the direct peer is in `[access] trusted_proxies`, Vajra resolves
//! the effective client IP from `X-Forwarded-For` / `X-Real-IP`, and the
//! rate limiter keys buckets on that resolved address. When the peer is
//! not trusted, forwarding headers are ignored.

use std::net::IpAddr;

use vajra::access::AccessSettings;
use vajra::cache::Cache;
use vajra::config::CacheSettings;
use vajra::date::DATE_LEN;
use vajra::http::{self, Ctx};
use vajra::observe::Observer;
use vajra::proxy::ProxyTable;
use vajra::rate_limit::{RateLimitSettings, RateLimiter};

const DATE: [u8; DATE_LEN] = [b'D'; DATE_LEN];

fn empty_table() -> ProxyTable {
    ProxyTable::new(&[], 1 << 20)
}

fn trust(peer_cidr: &str) -> AccessSettings {
    AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: vec![peer_cidr.parse().unwrap()],
    }
}

fn fresh_rl() -> RateLimiter {
    RateLimiter::new(RateLimitSettings {
        enabled: true,
        requests_per_second: 1,
        burst: 1,
        whitelist_ips: vec![],
        max_tracked_ips: 100,
        cleanup_interval_secs: 60,
        aggregate_ipv6_64: false,
        overrides: vec![],
    })
}

const REQ_XFF_SINGLE: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 198.51.100.7\r\n\r\n";
const REQ_XFF_CHAIN: &[u8] =
    b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 9.9.9.9, 198.51.100.7\r\n\r\n";
const REQ_NO_XFF: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

fn drive(
    table: &ProxyTable,
    cache: &mut Cache,
    obs: &mut Observer,
    access: &AccessSettings,
    rl: &mut RateLimiter,
    peer: IpAddr,
    req: &[u8],
) -> Vec<u8> {
    let mut ctx = Ctx {
        sites: None,  // FIX: Phase 1a
        files: None,
        proxies: table,
        cache,
        obs,
        now: 10,
        max_body: 1024,
        secure: false,
        client_ip: Some(peer),
        sec: None,
        access: Some(access),
        rate_limit: Some(rl),
        compression: None,
        fastcgi_cache: None,
    };
    let mut out = Vec::new();
    let _ = http::process(req, &mut out, &DATE, &mut ctx);
    out
}

fn is_429(out: &[u8]) -> bool {
    // Only inspect the HTTP status line (up to the first CRLF).
    // A plain `windows(3).any(|w| w == b"429")` also matches
    // `Content-Length: 429`, `Age: 429`, timestamps like `08:04:29`,
    // and any body that happens to contain those bytes, producing
    // rare but real false positives (see flaky
    // `untrusted_peer_ignores_xff` before 2026-10-14).
    let end = out.windows(2).position(|w| w == b"\r\n").unwrap_or(out.len());
    let line = &out[..end];
    line.ends_with(b" 429") || line.ends_with(b" 429 Too Many Requests")
}

fn only_key(rl: &RateLimiter) -> IpAddr {
    assert_eq!(rl.buckets.len(), 1, "expected exactly one bucket");
    rl.buckets.keys().next().unwrap().0
}

#[test]
fn untrusted_peer_ignores_xff() {
    let table = empty_table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = fresh_rl();
    let access = trust("10.0.0.0/8");
    let peer: IpAddr = "203.0.113.5".parse().unwrap();

    // Two requests, same untrusted peer, same XFF; burst = 1.
    let out1 = drive(
        &table,
        &mut cache,
        &mut obs,
        &access,
        &mut rl,
        peer,
        REQ_XFF_SINGLE,
    );
    assert!(!is_429(&out1), "first request should pass");
    let out2 = drive(
        &table,
        &mut cache,
        &mut obs,
        &access,
        &mut rl,
        peer,
        REQ_XFF_SINGLE,
    );
    assert!(is_429(&out2), "second request should 429 (keyed on peer)");

    // Bucket is on the peer, not the XFF address.
    assert_eq!(only_key(&rl), peer);
}

#[test]
fn trusted_peer_keys_on_xff() {
    let table = empty_table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = fresh_rl();
    let access = trust("10.0.0.0/8");
    let peer: IpAddr = "10.1.2.3".parse().unwrap();

    let out = drive(
        &table,
        &mut cache,
        &mut obs,
        &access,
        &mut rl,
        peer,
        REQ_XFF_SINGLE,
    );
    assert!(!is_429(&out), "trusted-proxy request should pass");

    assert_eq!(only_key(&rl), "198.51.100.7".parse::<IpAddr>().unwrap());
}

#[test]
fn trusted_peer_xff_chain_picks_rightmost_untrusted() {
    let table = empty_table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = fresh_rl();
    let access = trust("10.0.0.0/8");
    let peer: IpAddr = "10.1.2.3".parse().unwrap();

    let _ = drive(
        &table,
        &mut cache,
        &mut obs,
        &access,
        &mut rl,
        peer,
        REQ_XFF_CHAIN,
    );

    // 9.9.9.9 is leftmost, 198.51.100.7 rightmost non-trusted hop.
    assert_eq!(only_key(&rl), "198.51.100.7".parse::<IpAddr>().unwrap());
}

#[test]
fn trusted_peer_without_xff_uses_peer() {
    let table = empty_table();
    let mut cache = Cache::new(&CacheSettings::default());
    let mut obs = Observer::new();
    let mut rl = fresh_rl();
    let access = trust("10.0.0.0/8");
    let peer: IpAddr = "10.1.2.3".parse().unwrap();

    let _ = drive(
        &table, &mut cache, &mut obs, &access, &mut rl, peer, REQ_NO_XFF,
    );
    assert_eq!(only_key(&rl), peer);
}
