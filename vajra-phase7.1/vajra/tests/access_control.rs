//! Unit tests for `vajra::access` — no server, no I/O.

use std::net::IpAddr;

use vajra::access::{parse_nets, AccessDecision, AccessSettings};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("test IP")
}

// ─── allow/deny precedence ────────────────────────────────────────────

#[test]
fn empty_settings_allow_all() {
    let s = AccessSettings::default();
    assert!(s.is_empty());
    assert_eq!(s.evaluate(ip("1.2.3.4")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("2001:db8::1")), AccessDecision::Allow);
}

#[test]
fn deny_wins_over_allow() {
    let s = AccessSettings {
        allow_ips: parse_nets(&["10.0.0.0/8"]),
        deny_ips: parse_nets(&["10.1.2.0/24"]),
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("10.1.2.5")), AccessDecision::Deny);
    assert_eq!(s.evaluate(ip("10.2.3.4")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("11.0.0.1")), AccessDecision::Deny);
}

#[test]
fn allow_only_acts_as_whitelist() {
    let s = AccessSettings {
        allow_ips: parse_nets(&["192.168.0.0/16", "10.0.0.0/8"]),
        deny_ips: vec![],
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("10.5.5.5")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("192.168.1.1")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("8.8.8.8")), AccessDecision::Deny);
}

#[test]
fn single_ip_without_slash_is_a_host_rule() {
    let s = AccessSettings {
        allow_ips: parse_nets(&["203.0.113.7"]),
        deny_ips: vec![],
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("203.0.113.7")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("203.0.113.8")), AccessDecision::Deny);
}

#[test]
fn ipv6_cidr_works() {
    let s = AccessSettings {
        allow_ips: parse_nets(&["2001:db8::/32"]),
        deny_ips: vec![],
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("2001:db8::1")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("2001:db9::1")), AccessDecision::Deny);
}

#[test]
fn ipv4_mapped_ipv6_matches_v4_rule() {
    let s = AccessSettings {
        allow_ips: parse_nets(&["10.0.0.0/8"]),
        deny_ips: vec![],
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("::ffff:10.1.2.3")), AccessDecision::Allow);
    assert_eq!(s.evaluate(ip("::ffff:11.0.0.1")), AccessDecision::Deny);
}

#[test]
fn deny_all_range_blocks_everything() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: parse_nets(&["0.0.0.0/0", "::/0"]),
        trusted_proxies: vec![],
    };
    assert_eq!(s.evaluate(ip("1.2.3.4")), AccessDecision::Deny);
    assert_eq!(s.evaluate(ip("2001:db8::1")), AccessDecision::Deny);
}

// ─── trusted-proxy / XFF handling ─────────────────────────────────────

#[test]
fn untrusted_peer_ignores_xff() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1"]),
    };
    let client = s.effective_client_ip(ip("8.8.8.8"), Some("1.2.3.4"), None);
    assert_eq!(client, ip("8.8.8.8"));
}

#[test]
fn trusted_peer_uses_rightmost_untrusted_xff_entry() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1", "10.0.0.0/8"]),
    };
    // Chain:  client          proxy1      proxy2
    //        203.0.113.9     10.0.0.5    10.0.0.6
    // Peer = 127.0.0.1 (trusted). Walk right-to-left: 10.0.0.6 (trusted),
    // 10.0.0.5 (trusted), 203.0.113.9 (untrusted) → return it.
    let client = s.effective_client_ip(
        ip("127.0.0.1"),
        Some("203.0.113.9, 10.0.0.5, 10.0.0.6"),
        None,
    );
    assert_eq!(client, ip("203.0.113.9"));
}

#[test]
fn all_trusted_xff_falls_back_to_peer() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1", "10.0.0.0/8"]),
    };
    let client = s.effective_client_ip(ip("127.0.0.1"), Some("10.0.0.5, 10.0.0.6"), None);
    assert_eq!(client, ip("127.0.0.1"));
}

#[test]
fn x_real_ip_used_when_xff_absent() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1"]),
    };
    let client = s.effective_client_ip(ip("127.0.0.1"), None, Some("203.0.113.9"));
    assert_eq!(client, ip("203.0.113.9"));
}

#[test]
fn malformed_xff_entries_are_skipped() {
    let s = AccessSettings {
        allow_ips: vec![],
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1"]),
    };
    let client = s.effective_client_ip(
        ip("127.0.0.1"),
        Some("not-an-ip, 203.0.113.9, garbage"),
        None,
    );
    assert_eq!(client, ip("203.0.113.9"));
}

#[test]
fn spoofed_xff_from_untrusted_peer_is_ignored_by_evaluation() {
    // Even though the XFF header claims 10.0.0.1, the peer is untrusted,
    // so the effective client is the peer itself.
    let s = AccessSettings {
        allow_ips: parse_nets(&["10.0.0.0/8"]),
        deny_ips: vec![],
        trusted_proxies: parse_nets(&["127.0.0.1"]),
    };
    let client = s.effective_client_ip(ip("8.8.8.8"), Some("10.0.0.1"), None);
    assert_eq!(client, ip("8.8.8.8"));
    assert_eq!(s.evaluate(client), AccessDecision::Deny);
}
