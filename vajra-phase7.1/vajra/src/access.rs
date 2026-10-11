//! IP allow/deny lists + trusted-proxy handling.
//!
//! Semantics (in order of precedence):
//!
//! 1. If the effective client IP matches any `deny_ips` entry → **deny**.
//! 2. Else if `allow_ips` is non-empty and the IP matches none → **deny**.
//! 3. Else → **allow**.
//!
//! The *effective client IP* is:
//!
//! * the direct peer IP, if the peer is not a trusted proxy;
//! * else, the rightmost `X-Forwarded-For` entry that is itself **not** a
//!   trusted proxy (walking right-to-left);
//! * else `X-Real-IP`, if present and parseable;
//! * else the direct peer IP.
//!
//! A malformed `X-Forwarded-For` entry is skipped silently; if every entry
//! is unusable (or all are trusted), we fall back to the peer.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use serde::Deserialize;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AccessDecision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessSettings {
    pub allow_ips: Vec<IpNet>,
    pub deny_ips: Vec<IpNet>,
    pub trusted_proxies: Vec<IpNet>,
}

impl AccessSettings {
    /// No filtering at all — http.rs can skip the check entirely.
    pub fn is_empty(&self) -> bool {
        self.allow_ips.is_empty() && self.deny_ips.is_empty() && self.trusted_proxies.is_empty()
    }

    /// Is `ip` in the trusted-proxy set?
    pub fn is_trusted_proxy(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        self.trusted_proxies.iter().any(|n| contains(n, ip))
    }

    /// Given the direct peer and any forwarding headers, recover the client IP.
    pub fn effective_client_ip(
        &self,
        peer: IpAddr,
        xff: Option<&str>,
        x_real_ip: Option<&str>,
    ) -> IpAddr {
        if !self.is_trusted_proxy(peer) {
            return normalize(peer);
        }

        if let Some(xff) = xff {
            // Right-to-left: the closest-to-us entry is last.
            for tok in xff.split(',').rev() {
                if let Ok(ip) = tok.trim().parse::<IpAddr>() {
                    let ip = normalize(ip);
                    if !self.is_trusted_proxy(ip) {
                        return ip;
                    }
                }
            }
        }

        if let Some(raw) = x_real_ip {
            if let Ok(ip) = raw.trim().parse::<IpAddr>() {
                return normalize(ip);
            }
        }

        normalize(peer)
    }

    /// Apply allow/deny lists. Deny has strict precedence.
    pub fn evaluate(&self, ip: IpAddr) -> AccessDecision {
        let ip = normalize(ip);
        if self.deny_ips.iter().any(|n| contains(n, ip)) {
            return AccessDecision::Deny;
        }
        if !self.allow_ips.is_empty() && !self.allow_ips.iter().any(|n| contains(n, ip)) {
            return AccessDecision::Deny;
        }
        AccessDecision::Allow
    }
}

/// Collapse `::ffff:a.b.c.d` to plain IPv4 so V4 rules match clients
/// presented as IPv4-mapped IPv6 by some kernels/proxies.
pub(crate) fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// `IpNet::contains` requires same-family; try the given address, then
/// its IPv4-mapped form if any.
pub(crate) fn contains(net: &IpNet, ip: IpAddr) -> bool {
    if net.contains(&ip) {
        return true;
    }
    if let IpAddr::V6(v6) = ip {
        if let Some(v4) = v6.to_ipv4_mapped() {
            return net.contains(&IpAddr::V4(v4));
        }
    }
    false
}

/// Raw shape as it appears in TOML. Parsing to `IpNet` happens in
/// `build_access` so errors carry a useful field name.
#[derive(Debug, Default, Deserialize)]
pub struct RawAccess {
    #[serde(default)]
    pub allow_ips: Vec<String>,
    #[serde(default)]
    pub deny_ips: Vec<String>,
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

pub fn build_access(raw: RawAccess) -> Result<AccessSettings, String> {
    fn parse_list(field: &str, list: Vec<String>) -> Result<Vec<IpNet>, String> {
        list.into_iter()
            .map(|s| {
                parse_one_net(&s).map_err(|e| format!("[access] {field}: invalid CIDR {s:?}: {e}"))
            })
            .collect()
    }
    Ok(AccessSettings {
        allow_ips: parse_list("allow_ips", raw.allow_ips)?,
        deny_ips: parse_list("deny_ips", raw.deny_ips)?,
        trusted_proxies: parse_list("trusted_proxies", raw.trusted_proxies)?,
    })
}

/// Accept both CIDR (`10.0.0.0/8`, `2001:db8::/32`) and bare host
/// addresses (`10.0.0.1`, `::1`), the latter with an implied full-length
/// prefix. `ipnet::IpNet::from_str` deliberately rejects the bare form;
/// we want the friendlier behaviour for config files.
pub fn parse_one_net(s: &str) -> Result<IpNet, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty string".into());
    }
    if t.contains('/') {
        return t.parse::<IpNet>().map_err(|e| e.to_string());
    }
    // Bare host — try v4 first, then v6.
    if let Ok(v4) = t.parse::<Ipv4Addr>() {
        return Ok(IpNet::new(IpAddr::V4(v4), 32).expect("valid v4 /32"));
    }
    if let Ok(v6) = t.parse::<Ipv6Addr>() {
        return Ok(IpNet::new(IpAddr::V6(v6), 128).expect("valid v6 /128"));
    }
    Err("not an IP or CIDR".into())
}

/// Convenience for tests and programmatic config.
pub fn parse_nets(items: &[&str]) -> Vec<IpNet> {
    items
        .iter()
        .map(|s| parse_one_net(s).expect("test CIDR"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_v4_in_v6() {
        let mapped: IpAddr = "::ffff:10.1.2.3".parse().unwrap();
        assert_eq!(normalize(mapped), "10.1.2.3".parse::<IpAddr>().unwrap());
        let pure: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(normalize(pure), pure);
    }

    #[test]
    fn contains_handles_v4_rule_vs_v4_mapped_input() {
        let net: IpNet = "10.0.0.0/8".parse().unwrap();
        let mapped: IpAddr = "::ffff:10.5.5.5".parse().unwrap();
        assert!(contains(&net, mapped));
    }

    #[test]
    fn bare_host_parses_as_full_prefix() {
        assert_eq!(
            parse_one_net("10.0.0.1").unwrap().to_string(),
            "10.0.0.1/32"
        );
        assert_eq!(parse_one_net("::1").unwrap().to_string(), "::1/128");
        assert_eq!(
            parse_one_net("10.0.0.0/8").unwrap().to_string(),
            "10.0.0.0/8"
        );
        assert!(parse_one_net("not-an-ip").is_err());
    }
}
