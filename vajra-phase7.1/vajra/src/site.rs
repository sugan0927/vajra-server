//! Multi-site routing (Phase 1a: static-only).
//!
//! A single Vajra process can serve multiple hostnames by matching the
//! request's `Host:` header against the `[[site]]` table in the config.
//! Each entry owns its own static-file cache, rooted at its own directory.
//!
//! If no `[[site]]` blocks are configured, this table is empty and the
//! worker falls back to the legacy single-`[static]` behaviour.

use crate::config::{Balance, ProxySettings, SiteMode, SiteSettings, StaticSettings, UpAddr};
use crate::php::PhpConfig;
use crate::static_files::FileCache;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct SiteEntry {
    pub host: String,
    pub aliases: Vec<String>,
    pub root: PathBuf,
    pub index: String,
    pub files: FileCache,
    /// Phase 1b: `Some` if this site's mode is php/wordpress.
    pub php: Option<PhpConfig>,
    /// Index into the combined `[[proxy]]` table (user proxies followed
    /// by synthetic per-site PHP routes).  None when `php` is None.
    pub php_route_idx: Option<usize>,
}

pub struct SiteTable {
    sites: Vec<SiteEntry>,
    /// host (lowercase, no port, no trailing dot) -> index into `sites`
    by_host: HashMap<String, usize>,
}

impl SiteTable {
    pub fn empty() -> Self {
        Self {
            sites: Vec::new(),
            by_host: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Build the table from `[[site]]` config blocks. Every `root` must
    /// exist as a directory; the file cache is built eagerly so the first
    /// request does not pay the setup cost.
    pub fn build(cfgs: &[SiteSettings]) -> Result<(Self, Vec<ProxySettings>), String> {
        let mut sites = Vec::with_capacity(cfgs.len());
        let mut by_host: HashMap<String, usize> = HashMap::new();
        let mut synthetic_proxies: Vec<ProxySettings> = Vec::new();

        for (i, c) in cfgs.iter().enumerate() {
            let canon = normalize_host(&c.host);
            if canon.is_empty() {
                return Err(format!("site[{i}]: host is empty"));
            }

            // Build a StaticSettings to validate the root and build the cache.
            let mut st = StaticSettings::new(&c.root)
                .map_err(|e| format!("site {canon}: {e}"))?;
            if !c.index.is_empty() {
                st.index = c.index.clone();
            }
            let files = FileCache::new(&st);

            // Canonical host
            if by_host.insert(canon.clone(), i).is_some() {
                return Err(format!("site {canon}: duplicate host"));
            }

            // Aliases
            let mut aliases = Vec::with_capacity(c.aliases.len());
            for a in &c.aliases {
                let al = normalize_host(a);
                if al.is_empty() {
                    continue;
                }
                if al == canon {
                    continue; // canonical already covered
                }
                if by_host.insert(al.clone(), i).is_some() {
                    return Err(format!("site {canon}: duplicate alias {al}"));
                }
                aliases.push(al);
            }

            // Phase 1b: mode=php/wordpress sites get a synthetic
            // ProxySettings with php: Some(PhpConfig) so the worker's
            // combined ProxyTable can dispatch FastCGI by route index.
            let (php, php_route_idx) = match c.mode {
                SiteMode::Static => (None, None),
                SiteMode::Php | SiteMode::WordPress => {
                    let sock = c.php_sock.as_deref().ok_or_else(|| {
                        format!("site {canon}: mode={:?} requires php_sock", c.mode)
                    })?;
                    let upstream = parse_php_upstream(sock)
                        .map_err(|e| format!("site {canon}: php_sock: {e}"))?;
                    let cfg = PhpConfig::wordpress(&c.root);
                    let idx = synthetic_proxies.len();
                    synthetic_proxies.push(ProxySettings {
                        prefix: "/".into(),
                        upstreams: vec![upstream],
                        balance: Balance::RoundRobin,
                        strip_prefix: false,
                        timeout_secs: 60,
                        max_fails: 3,
                        fail_timeout_secs: 30,
                        cache: false,
                        cache_default_ttl_secs: 0,
                        php: Some(cfg.clone()),
                    });
                    (Some(cfg), Some(idx))
                }
            };

            sites.push(SiteEntry {
                host: canon,
                aliases,
                root: c.root.clone(),
                index: st.index.clone(),
                files,
                php,
                php_route_idx,
            });
        }

        Ok((Self { sites, by_host }, synthetic_proxies))
    }

    /// Exact host match (after normalisation). `None` if no site matches.
    pub fn lookup(&self, host_header: &str) -> Option<&SiteEntry> {
        let h = normalize_host(host_header);
        self.by_host.get(&h).and_then(|&i| self.sites.get(i))
    }

    pub fn lookup_mut(&mut self, host_header: &str) -> Option<&mut SiteEntry> {
        let h = normalize_host(host_header);
        let idx = self.by_host.get(&h).copied()?;
        self.sites.get_mut(idx)
    }

    /// Iterate all canonical hosts (for logging / metrics).
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.sites.iter().map(|s| s.host.as_str())
    }

    /// Mutable iteration (used by the worker to tick per-site caches).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut SiteEntry> {
        self.sites.iter_mut()
    }

    /// Tick `FileCache::set_now` on every site. Called once per event-loop
    /// batch by the worker.
    pub fn set_now(&mut self, now: u64) {
        for s in &mut self.sites {
            s.files.set_now(now);
        }
    }
}

/// Lowercase, strip the `:port` suffix, strip a trailing dot (FQDN form).
/// Handles IPv6 literals `[::1]:8080`.
pub fn normalize_host(raw: &str) -> String {
    let h = raw.trim();

    // A bare host can be:
    //   hostname             (0 colons)
    //   hostname:port        (1 colon, port numeric)
    //   IPv4 address:port    (1 colon, port numeric)
    //   bare IPv6            (>= 2 colons, no brackets)
    //   [IPv6]               (bracketed)
    //   [IPv6]:port          (bracketed, port after `]`)
    let colon_count = h.bytes().filter(|&b| b == b':').count();

    let h = if let Some(rest) = h.strip_prefix('[') {
        // Bracketed IPv6: take everything up to `]`, ignore any :port after it.
        match rest.find(']') {
            Some(end) => &rest[..end],
            None => h,
        }
    } else if colon_count >= 2 {
        // Bare IPv6 — do not treat any `:` as a port separator.
        h
    } else if let Some(colon) = h.rfind(':') {
        let before = &h[..colon];
        let after = &h[colon + 1..];
        if !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit()) {
            before
        } else {
            h
        }
    } else {
        h
    };

    // Trailing dot (FQDN form: `example.com.`)
    let h = h.strip_suffix('.').unwrap_or(h);
    h.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_port_and_lowercases() {
        assert_eq!(normalize_host("Example.COM:8080"), "example.com");
        assert_eq!(normalize_host("example.com."), "example.com");
        assert_eq!(normalize_host("Example.com"), "example.com");
        assert_eq!(normalize_host("[::1]:80"), "::1");
        assert_eq!(normalize_host("::1"), "::1");
        assert_eq!(normalize_host("host:notaport"), "host:notaport");
        assert_eq!(normalize_host("  spaced.example  "), "spaced.example");
    }
}


/// Parse a `php_sock` string into an [`UpAddr`]:
///   * `unix:/run/php/...sock`            -> Unix socket
///   * `127.0.0.1:9000` / `[::1]:9000`    -> TCP
fn parse_php_upstream(s: &str) -> Result<UpAddr, String> {
    if let Some(path) = s.strip_prefix("unix:") {
        return Ok(UpAddr::Unix(PathBuf::from(path)));
    }
    s.parse::<std::net::SocketAddr>()
        .map(UpAddr::Tcp)
        .map_err(|e| format!("invalid upstream {s:?}: {e}"))
}
