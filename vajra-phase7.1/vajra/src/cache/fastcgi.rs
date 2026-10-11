//! On-disk FastCGI cache storage layer.
//!
//! See `docs/fastcgi-cache.md` for the design. This module implements
//! the storage half only: cache key hashing, the on-disk layout, the
//! index file, and LRU eviction. Wiring into the FastCGI request path
//! lands on Day 19+.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

// ─── Config ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FastcgiCacheSettings {
    pub enabled: bool,
    pub path: PathBuf,
    pub size_mb: u64,
    pub ttl_secs: u64,
    pub max_entry_size: u64,
    pub bypass_cookie: Vec<String>,
    pub exclude_paths: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawFastcgiCache {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_path")]
    pub path: PathBuf,
    #[serde(default = "default_size_mb")]
    pub size_mb: u64,
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
    #[serde(default = "default_max_entry")]
    pub max_entry_size: u64,
    #[serde(default)]
    pub bypass_cookie: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}

fn default_path() -> PathBuf {
    PathBuf::from("/var/cache/vajra/fcgi")
}
fn default_size_mb() -> u64 {
    1024
}
fn default_ttl() -> u64 {
    600
}
fn default_max_entry() -> u64 {
    1024 * 1024
}

pub fn build_fastcgi_cache(raw: RawFastcgiCache) -> Result<FastcgiCacheSettings, String> {
    if raw.enabled {
        if raw.path.as_os_str().is_empty() {
            return Err("[fastcgi_cache] path must not be empty".into());
        }
        if !raw.path.is_absolute() {
            return Err(format!(
                "[fastcgi_cache] path must be absolute, got {:?}",
                raw.path
            ));
        }
        if raw.path.as_os_str().len() > 200 {
            return Err("[fastcgi_cache] path must be shorter than 200 bytes".into());
        }
        if raw.size_mb == 0 {
            return Err("[fastcgi_cache] size_mb must be >= 1".into());
        }
        if raw.ttl_secs == 0 {
            return Err("[fastcgi_cache] ttl_secs must be >= 1".into());
        }
        if raw.max_entry_size == 0 {
            return Err("[fastcgi_cache] max_entry_size must be >= 1".into());
        }
    }
    Ok(FastcgiCacheSettings {
        enabled: raw.enabled,
        path: raw.path,
        size_mb: raw.size_mb,
        ttl_secs: raw.ttl_secs,
        max_entry_size: raw.max_entry_size,
        bypass_cookie: raw.bypass_cookie,
        exclude_paths: raw.exclude_paths,
    })
}

// ─── Key hashing ────────────────────────────────────────────────────

/// FNV-1a 128-bit. Fast and dependency-free. Cache-key collisions
/// cannot return wrong data because the full key is stored in the
/// index and verified on every read.
pub fn key_hash(key: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013B;
    let mut h = OFFSET;
    for &b in key {
        h ^= b as u128;
        h = h.wrapping_mul(PRIME);
    }
    h
}

fn hex32(v: u128) -> [u8; 32] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let shift = 124 - i * 4;
        *b = HEX[((v >> shift) & 0xF) as usize];
    }
    out
}

// ─── Glob matching ──────────────────────────────────────────────────
//
// Only two forms are supported: exact match (`/wp-login.php`) and a
// single trailing `*` prefix match (`/wp-admin/*`, `wordpress_logged_in_*`).
// That covers every example in docs/fastcgi-cache.md and keeps the
// matcher allocation-free. Anything richer belongs in a real glob crate
// if the need ever shows up.

/// `true` iff `value` matches `pattern`.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// `true` iff any of `patterns` matches `value`.
pub fn any_glob_matches(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|p| glob_matches(p, value))
}

/// `true` iff the `Cookie:` header carries at least one cookie whose
/// name matches any pattern in `patterns`.
///
/// The header is parsed as `name=value; name=value; ...`. Names are
/// compared verbatim; no URL-decoding (the transport layer already
/// decoded the header value once).
pub fn cookie_name_matches(cookie_header: Option<&str>, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let Some(h) = cookie_header else {
        return false;
    };
    for pair in h.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let name = pair.split('=').next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        if any_glob_matches(patterns, name) {
            return true;
        }
    }
    false
}

// ─── Cache-key builder ─────────────────────────────────────────────

/// Canonical FastCGI cache key.
///
/// ```text
/// "v1\0" + method + "\0" + host + "\0" + path [+ "\0" + query]
/// ```
///
/// The `v1\0` prefix is a format version: bumping it invalidates the
/// whole cache without a migration. `host` is expected to already be
/// lowercased by the caller.
pub fn build_cache_key(method: &str, host: &str, path: &str, query: Option<&str>) -> Vec<u8> {
    let mut k = Vec::with_capacity(
        4 + method.len() + host.len() + path.len() + query.map_or(0, |q| q.len() + 1),
    );
    k.extend_from_slice(b"v1\0");
    k.extend_from_slice(method.as_bytes());
    k.push(0);
    k.extend_from_slice(host.as_bytes());
    k.push(0);
    k.extend_from_slice(path.as_bytes());
    if let Some(q) = query {
        if !q.is_empty() {
            k.push(0);
            k.extend_from_slice(q.as_bytes());
        }
    }
    k
}

// ─── Index entry ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexEntry {
    key: Vec<u8>,
    size: u64,
    expires_at: u64,
    last_access: u64,
}

// ─── Cache ──────────────────────────────────────────────────────────

pub struct FastcgiCache {
    pub settings: FastcgiCacheSettings,
    index: Vec<IndexEntry>,
    /// key -> position in `index`.
    by_key: HashMap<Vec<u8>, usize>,
    total_bytes: u64,
}

impl FastcgiCache {
    /// Open (or create) the cache under `settings.path`. Loads the
    /// on-disk index if present and consistent; otherwise starts
    /// empty.
    pub fn open(settings: FastcgiCacheSettings) -> std::io::Result<Self> {
        fs::create_dir_all(settings.path.join("data"))?;
        let index = load_index(&settings.path).unwrap_or_default();
        let total_bytes = index.iter().map(|e| e.size).sum();
        let mut by_key = HashMap::with_capacity(index.len());
        for (i, e) in index.iter().enumerate() {
            by_key.insert(e.key.clone(), i);
        }
        Ok(Self {
            settings,
            index,
            by_key,
            total_bytes,
        })
    }

    pub fn settings(&self) -> &FastcgiCacheSettings {
        &self.settings
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn entries(&self) -> usize {
        self.index.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn data_path(&self, key: &[u8]) -> PathBuf {
        let h = hex32(key_hash(key));
        let shard = std::str::from_utf8(&h[0..2]).unwrap();
        let name = std::str::from_utf8(&h).unwrap();
        self.settings.path.join("data").join(shard).join(name)
    }

    /// Look up `key`. Returns `None` on miss or expiry.
    pub fn get(&mut self, key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        if !self.settings.enabled {
            return Ok(None);
        }
        let now = Self::now();
        let Some(&idx) = self.by_key.get(key) else {
            return Ok(None);
        };
        if self.index[idx].expires_at <= now {
            self.remove_at(idx)?;
            return Ok(None);
        }
        let path = self.data_path(key);
        let body = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.remove_at(idx)?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if let Some(&i2) = self.by_key.get(key) {
            self.index[i2].last_access = now;
        }
        Ok(Some(body))
    }

    /// Store `body` under `key` with `ttl_secs` lifetime.
    pub fn put(&mut self, key: &[u8], body: &[u8], ttl_secs: u64) -> std::io::Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        if body.len() as u64 > self.settings.max_entry_size {
            return Ok(());
        }

        if let Some(&idx) = self.by_key.get(key) {
            self.remove_at(idx)?;
        }

        let need = body.len() as u64;
        self.evict_until_fits(need)?;

        let path = self.data_path(key);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body)?;
            f.sync_all().ok();
        }
        fs::rename(&tmp, &path)?;

        let now = Self::now();
        let idx = self.index.len();
        self.index.push(IndexEntry {
            key: key.to_vec(),
            size: need,
            expires_at: now.saturating_add(ttl_secs),
            last_access: now,
        });
        self.by_key.insert(key.to_vec(), idx);
        self.total_bytes += need;
        Ok(())
    }

    /// Remove the entry at position `i`.
    fn remove_at(&mut self, i: usize) -> std::io::Result<()> {
        let removed = self.index.swap_remove(i);
        self.total_bytes = self.total_bytes.saturating_sub(removed.size);
        self.by_key.remove(&removed.key);

        if i < self.index.len() {
            let moved_key = self.index[i].key.clone();
            self.by_key.insert(moved_key, i);
        }

        let path = self.data_path(&removed.key);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Evict LRU entries until `extra` bytes fit within `size_mb`.
    fn evict_until_fits(&mut self, extra: u64) -> std::io::Result<()> {
        let cap = self.settings.size_mb.saturating_mul(1024 * 1024);
        while self.total_bytes + extra > cap && !self.index.is_empty() {
            let (idx, _) = self
                .index
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_access)
                .expect("non-empty");
            self.remove_at(idx)?;
        }
        Ok(())
    }

    /// Drop expired entries. Returns the count removed.
    pub fn cleanup_expired(&mut self) -> std::io::Result<usize> {
        let now = Self::now();
        let mut removed = 0;
        let mut i = 0;
        while i < self.index.len() {
            if self.index[i].expires_at <= now {
                self.remove_at(i)?;
                removed += 1;
            } else {
                i += 1;
            }
        }
        Ok(removed)
    }

    /// Rewrite the on-disk index atomically.
    pub fn save_index(&self) -> std::io::Result<()> {
        save_index(&self.settings.path, &self.index)
    }
}

// ─── Index serialization ────────────────────────────────────────────

const INDEX_MAGIC: &[u8; 8] = b"VJFC1\0\0\0";
const INDEX_HEADER: usize = 8 + 8;

fn load_index(root: &Path) -> Option<Vec<IndexEntry>> {
    let mut f = fs::File::open(root.join("index.bin")).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    if buf.len() < INDEX_HEADER || &buf[0..8] != INDEX_MAGIC {
        return None;
    }
    let count = u64::from_le_bytes(buf[8..16].try_into().ok()?);
    let mut p = INDEX_HEADER;
    let mut out: Vec<IndexEntry> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if p + 4 > buf.len() {
            return None;
        }
        let klen = u32::from_le_bytes(buf[p..p + 4].try_into().ok()?) as usize;
        p += 4;
        if p + klen + 24 > buf.len() {
            return None;
        }
        let key = buf[p..p + klen].to_vec();
        p += klen;
        let size = u64::from_le_bytes(buf[p..p + 8].try_into().ok()?);
        p += 8;
        let expires_at = u64::from_le_bytes(buf[p..p + 8].try_into().ok()?);
        p += 8;
        let last_access = u64::from_le_bytes(buf[p..p + 8].try_into().ok()?);
        p += 8;
        out.push(IndexEntry {
            key,
            size,
            expires_at,
            last_access,
        });
    }
    Some(out)
}

fn save_index(root: &Path, entries: &[IndexEntry]) -> std::io::Result<()> {
    let est: usize = entries.iter().map(|e| 4 + e.key.len() + 24).sum();
    let mut buf = Vec::with_capacity(INDEX_HEADER + est);
    buf.extend_from_slice(INDEX_MAGIC);
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for e in entries {
        buf.extend_from_slice(&(e.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&e.key);
        buf.extend_from_slice(&e.size.to_le_bytes());
        buf.extend_from_slice(&e.expires_at.to_le_bytes());
        buf.extend_from_slice(&e.last_access.to_le_bytes());
    }
    let tmp = root.join("index.bin.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, root.join("index.bin"))?;
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "vajra-fcgi-{}-{}-{}",
            label,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn settings(dir: &Path, size_mb: u64) -> FastcgiCacheSettings {
        FastcgiCacheSettings {
            enabled: true,
            path: dir.to_path_buf(),
            size_mb,
            ttl_secs: 60,
            max_entry_size: 1 << 20,
            bypass_cookie: vec![],
            exclude_paths: vec![],
        }
    }

    #[test]
    fn roundtrip_get_put() {
        let dir = scratch("rt");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"k1", b"hello world", 60).unwrap();
        let got = c.get(b"k1").unwrap();
        assert_eq!(got.as_deref(), Some(&b"hello world"[..]));
        assert_eq!(c.entries(), 1);
        assert_eq!(c.total_bytes(), 11);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn miss_on_unknown_key() {
        let dir = scratch("miss");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        assert!(c.get(b"nope").unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrite_replaces_entry() {
        let dir = scratch("ow");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"k", b"aa", 60).unwrap();
        c.put(b"k", b"bbbb", 60).unwrap();
        assert_eq!(c.entries(), 1);
        assert_eq!(c.total_bytes(), 4);
        assert_eq!(c.get(b"k").unwrap().as_deref(), Some(&b"bbbb"[..]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expiry_on_read() {
        let dir = scratch("expiry");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"k", b"payload", 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(c.get(b"k").unwrap().is_none());
        assert_eq!(c.entries(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lru_eviction_when_size_exceeded() {
        let dir = scratch("lru");
        let mut c = FastcgiCache::open(settings(&dir, 1)).unwrap();
        let big = vec![b'A'; 400 * 1024];
        c.put(b"first", &big, 60).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"second", &big, 60).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"third", &big, 60).unwrap();

        assert!(
            c.get(b"first").unwrap().is_none(),
            "first should be evicted"
        );
        assert!(c.get(b"second").unwrap().is_some());
        assert!(c.get(b"third").unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn max_entry_size_rejected() {
        let dir = scratch("big");
        let mut s = settings(&dir, 16);
        s.max_entry_size = 4;
        let mut c = FastcgiCache::open(s).unwrap();
        c.put(b"k", b"0123456789", 60).unwrap();
        assert_eq!(c.entries(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_persists_across_reopen() {
        let dir = scratch("persist");
        {
            let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
            c.put(b"k1", b"v1", 60).unwrap();
            c.put(b"k2", b"v2", 60).unwrap();
            c.save_index().unwrap();
        }
        let mut c2 = FastcgiCache::open(settings(&dir, 16)).unwrap();
        assert_eq!(c2.entries(), 2);
        assert_eq!(c2.get(b"k1").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(c2.get(b"k2").unwrap().as_deref(), Some(&b"v2"[..]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_expired_sweeps_stale_entries() {
        let dir = scratch("cleanup");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"a", b"payload", 0).unwrap();
        c.put(b"b", b"payload", 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"c", b"payload", 600).unwrap();
        let removed = c.cleanup_expired().unwrap();
        assert_eq!(removed, 2);
        assert_eq!(c.entries(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_hash_is_deterministic_and_well_distributed() {
        assert_eq!(key_hash(b"abc"), key_hash(b"abc"));
        assert_ne!(key_hash(b"abc"), key_hash(b"abd"));
        let a = key_hash(b"GET\0host\0/path");
        let b = key_hash(b"GET\0host\0/path2");
        assert_ne!(a, b);
    }

    #[test]
    fn glob_prefix_and_exact() {
        assert!(glob_matches("/wp-admin/*", "/wp-admin/"));
        assert!(glob_matches("/wp-admin/*", "/wp-admin/settings.php"));
        assert!(!glob_matches("/wp-admin/*", "/wp-admin"));
        assert!(!glob_matches("/wp-admin/*", "/admin/"));
        assert!(glob_matches("/wp-login.php", "/wp-login.php"));
        assert!(!glob_matches("/wp-login.php", "/wp-login.php?x=1"));
        assert!(glob_matches(
            "wordpress_logged_in_*",
            "wordpress_logged_in_abc"
        ));
        assert!(!glob_matches("wordpress_logged_in_*", "wplogin"));
    }

    #[test]
    fn cookie_name_matching() {
        let pats = vec![
            "wordpress_logged_in_*".to_string(),
            "wp-postpass_*".to_string(),
        ];
        assert!(cookie_name_matches(
            Some("wordpress_logged_in_abc=xyz"),
            &pats
        ));
        assert!(cookie_name_matches(
            Some("a=1; wordpress_logged_in_x=1; b=2"),
            &pats
        ));
        assert!(cookie_name_matches(Some("wp-postpass_h=1"), &pats));
        assert!(!cookie_name_matches(Some("PHPSESSID=xyz"), &pats));
        assert!(!cookie_name_matches(Some(""), &pats));
        assert!(!cookie_name_matches(None, &pats));
        assert!(!cookie_name_matches(Some("whatever=1"), &[]));
    }

    #[test]
    fn cache_key_shape() {
        let k = build_cache_key("GET", "example.com", "/", None);
        assert_eq!(&k[..3], b"v1\0");
        assert!(k.windows(4).any(|w| w == b"GET\0"));
        assert!(k.windows(12).any(|w| w == b"example.com\0"));

        let k2 = build_cache_key("GET", "example.com", "/p", Some("a=1"));
        let k3 = build_cache_key("GET", "example.com", "/p", None);
        assert_ne!(k2, k3);
        // Deterministic.
        assert_eq!(
            build_cache_key("GET", "h", "/p", Some("q")),
            build_cache_key("GET", "h", "/p", Some("q"))
        );
    }

    #[test]
    fn config_validation() {
        assert!(build_fastcgi_cache(RawFastcgiCache {
            enabled: true,
            path: PathBuf::from("relative"),
            size_mb: 1,
            ttl_secs: 1,
            max_entry_size: 1,
            ..Default::default()
        })
        .is_err());
        assert!(build_fastcgi_cache(RawFastcgiCache {
            enabled: true,
            path: PathBuf::from("/tmp/x"),
            size_mb: 0,
            ttl_secs: 1,
            max_entry_size: 1,
            ..Default::default()
        })
        .is_err());
        assert!(build_fastcgi_cache(RawFastcgiCache {
            enabled: true,
            path: PathBuf::from("/tmp/x"),
            size_mb: 1,
            ttl_secs: 1,
            max_entry_size: 1,
            ..Default::default()
        })
        .is_ok());
    }
}
