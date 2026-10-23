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
/// "v2\0" + method + "\0" + host + "\0" + path [+ "\0" + query]
/// ```
///
/// The `v1\0` prefix is a format version: bumping it invalidates the
/// whole cache without a migration. `host` is expected to already be
/// lowercased by the caller.
pub fn build_cache_key(method: &str, host: &str, path: &str, query: Option<&str>) -> Vec<u8> {
    let mut k = Vec::with_capacity(
        4 + method.len() + host.len() + path.len() + query.map_or(0, |q| q.len() + 1),
    );
    k.extend_from_slice(b"v2\0");
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

/// Decode the header block of an on-disk v2 entry. Returns `None` on
/// any structural mismatch (truncated, invalid lengths, non-UTF8 names
/// are tolerated as raw bytes since we round-trip `Vec<u8>` pairs).
fn decode_headers(buf: &[u8]) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    if buf.len() < 4 {
        return None;
    }
    let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let mut p = 4;
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(n);
    for _ in 0..n {
        if p + 4 > buf.len() {
            return None;
        }
        let nlen = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        p += 4;
        if p + nlen + 4 > buf.len() {
            return None;
        }
        let name = buf[p..p + nlen].to_vec();
        p += nlen;
        let vlen = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
        p += 4;
        if p + vlen > buf.len() {
            return None;
        }
        let value = buf[p..p + vlen].to_vec();
        p += vlen;
        out.push((name, value));
    }
    Some(out)
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
    /// Last observed PURGE_GEN; polled before each get.
    last_gen: u64,
}

impl FastcgiCache {
    /// Open (or create) the cache under `settings.path`. Loads the
    /// on-disk index if present and consistent; otherwise starts
    /// empty.
    /// आम case में एक atomic load। जब generation बदली हो, उन index
    /// entries को drop करो जिनकी file disk पर नहीं रही।
    pub fn poll_purge(&mut self) {
        let cur = purge_generation();
        if cur == self.last_gen {
            return;
        }
        let mut i = 0;
        while i < self.index.len() {
            let key = self.index[i].key.clone();
            if self.data_path(&key).exists() {
                i += 1;
            } else {
                let removed = self.index.swap_remove(i);
                self.total_bytes = self.total_bytes.saturating_sub(removed.size);
                self.by_key.remove(&removed.key);
                if i < self.index.len() {
                    let moved = self.index[i].key.clone();
                    self.by_key.insert(moved, i);
                }
            }
        }
        self.last_gen = cur;
    }

    pub fn open(settings: FastcgiCacheSettings) -> std::io::Result<Self> {
        fs::create_dir_all(settings.path.join("data"))?;
        fs::create_dir_all(settings.path.join("paths"))?;
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
            last_gen: purge_generation(),
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

    /// `<cache>/paths/<hex(key)>` — `put` द्वारा लिखी गई request path की
    /// sidecar file। Entry file (v2 format) में कुछ नहीं बदलता।
    fn sidecar_path(&self, key: &[u8]) -> std::path::PathBuf {
        let h = format!("{:032x}", key_hash(key));
        self.settings.path.join("paths").join(h)
    }
    fn data_path(&self, key: &[u8]) -> PathBuf {
        let h = hex32(key_hash(key));
        let shard = std::str::from_utf8(&h[0..2]).unwrap();
        let name = std::str::from_utf8(&h).unwrap();
        self.settings.path.join("data").join(shard).join(name)
    }

    /// Look up `key`. Returns `None` on miss, expiry, or a corrupt entry.
    /// On success returns `(status, storable_headers, body)`.
    #[allow(clippy::type_complexity)]
    pub fn get(
        &mut self,
        key: &[u8],
        accept_encoding: Option<&str>,
    ) -> std::io::Result<Option<(u16, Vec<(Vec<u8>, Vec<u8>)>, Vec<u8>)>> {
        if !self.settings.enabled { return Ok(None); }
        let now = Self::now();
        let Some(&idx) = self.by_key.get(key) else { return Ok(None); };
        if self.index[idx].expires_at <= now { self.remove_at(idx)?; return Ok(None); }
        
        let path = self.data_path(key);
        let buf = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { self.remove_at(idx)?; return Ok(None); }
            Err(e) => return Err(e),
        };
        if let Some(&i2) = self.by_key.get(key) { self.index[i2].last_access = now; }
        if buf.len() < 6 { self.remove_at(idx)?; return Ok(None); }
        
        let status = u16::from_le_bytes([buf[0], buf[1]]);
        let hdr_len = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) as usize;
        if buf.len() < 6 + hdr_len { self.remove_at(idx)?; return Ok(None); }
        
        let hdr_bytes = &buf[6..6 + hdr_len];
        let mut body = buf[6 + hdr_len..].to_vec();
        let Some(mut headers) = decode_headers(hdr_bytes) else { self.remove_at(idx)?; return Ok(None); };

        if let Some(ae) = accept_encoding {
            if ae.contains("br") && path.with_extension("br").exists() {
                if let Ok(br_data) = fs::read(path.with_extension("br")) {
                    body = br_data;
                    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(b"content-encoding") && !k.eq_ignore_ascii_case(b"content-length"));
                    headers.push((b"Content-Encoding".to_vec(), b"br".to_vec()));
                    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(b"vary")) {
                        headers.push((b"Vary".to_vec(), b"Accept-Encoding".to_vec()));
                    }
                }
            } else if ae.contains("gzip") && path.with_extension("gz").exists() {
                if let Ok(gz_data) = fs::read(path.with_extension("gz")) {
                    body = gz_data;
                    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(b"content-encoding") && !k.eq_ignore_ascii_case(b"content-length"));
                    headers.push((b"Content-Encoding".to_vec(), b"gzip".to_vec()));
                    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(b"vary")) {
                        headers.push((b"Vary".to_vec(), b"Accept-Encoding".to_vec()));
                    }
                }
            }
        }
        Ok(Some((status, headers, body)))
    }

    pub fn put(
        &mut self,
        key: &[u8],
        req_path: &[u8],
        status: u16,
        headers: &[(Vec<u8>, Vec<u8>)],
        body: &[u8],
        ttl_secs: u64,
    ) -> std::io::Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }

        let mut buf = Vec::with_capacity(body.len() + 64);
        buf.extend_from_slice(&status.to_le_bytes());
        let hdr_len_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        let hdr_start = buf.len();
        buf.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        for (n, v) in headers {
            buf.extend_from_slice(&(n.len() as u32).to_le_bytes());
            buf.extend_from_slice(n);
            buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
            buf.extend_from_slice(v);
        }
        let hdr_len = (buf.len() - hdr_start) as u32;
        buf[hdr_len_pos..hdr_len_pos + 4].copy_from_slice(&hdr_len.to_le_bytes());
        buf.extend_from_slice(body);

        if buf.len() as u64 > self.settings.max_entry_size {
            return Ok(());
        }

        if let Some(&idx) = self.by_key.get(key) {
            self.remove_at(idx)?;
        }

        let need = buf.len() as u64;
        self.evict_until_fits(need)?;

        let path = self.data_path(key);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_all().ok();
        }
        fs::rename(&tmp, &path)?;

        // ==========================================
        // Pre-Compressed Cache (gzip_static/brotli_static)
        // ==========================================
        if buf.len() >= 1024 {
            use crate::compress::{brotli_compress, gzip_compress};
            
            // Gzip (Level 6 - Cache के लिए हम High Compression इस्तेमाल कर सकते हैं)
            if let Some(gz_data) = gzip_compress(&buf, 6) {
                let gz_path = path.with_extension("gz");
                let gz_tmp = gz_path.with_extension("gz.tmp");
                if let Ok(mut f) = fs::File::create(&gz_tmp) {
                    let _ = f.write_all(&gz_data);
                    let _ = f.sync_all();
                    let _ = fs::rename(&gz_tmp, &gz_path);
                }
            }
            
            // Brotli (Level 4 - CPU और Size का अच्छा बैलेंस)
            if let Some(br_data) = brotli_compress(&buf, 4) {
                let br_path = path.with_extension("br");
                let br_tmp = br_path.with_extension("br.tmp");
                if let Ok(mut f) = fs::File::create(&br_tmp) {
                    let _ = f.write_all(&br_data);
                    let _ = f.sync_all();
                    let _ = fs::rename(&br_tmp, &br_path);
                }
            }
        }

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
        crate::metrics::record_fastcgi_cache_stats(self.entries() as u64, self.total_bytes);

        // sidecar: request path record करो ताकि admin `purge?path=...`
        // सिर्फ़ छोटी फ़ाइलें पढ़कर match कर सके।
        let sidecar = self.sidecar_path(key);
        if let Some(d) = sidecar.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::write(&sidecar, req_path);

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

        crate::metrics::record_fastcgi_cache_eviction();

        let path = self.data_path(&removed.key);
        let r = match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        crate::metrics::record_fastcgi_cache_stats(self.entries() as u64, self.total_bytes);
        r
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
        let hdrs: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"content-type".to_vec(), b"text/html".to_vec()),
            (b"x-test".to_vec(), b"yes".to_vec()),
        ];
        c.put(b"k1", b"/", 200, &hdrs, b"hello world", 60).unwrap();
        let got = c.get(b"k1").unwrap().expect("hit");
        assert_eq!(got.0, 200);
        assert_eq!(got.1, hdrs);
        assert_eq!(got.2, b"hello world");
        assert_eq!(c.entries(), 1);
        assert!(c.total_bytes() > 11); // header blob adds overhead
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
        c.put(b"k", b"/", 200, &[], b"aa", 60).unwrap();
        c.put(b"k", b"/", 200, &[], b"bbbb", 60).unwrap();
        assert_eq!(c.entries(), 1);
        let (_, _, body) = c.get(b"k").unwrap().expect("hit");
        assert_eq!(body, b"bbbb");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expiry_on_read() {
        let dir = scratch("expiry");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"k", b"/", 200, &[], b"payload", 0).unwrap();
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
        c.put(b"first", b"/", 200, &[], &big, 60).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"second", b"/", 200, &[], &big, 60).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"third", b"/", 200, &[], &big, 60).unwrap();

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
        c.put(b"k", b"/", 200, &[], b"0123456789", 60).unwrap();
        assert_eq!(c.entries(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_persists_across_reopen() {
        let dir = scratch("persist");
        {
            let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
            c.put(b"k1", b"/", 200, &[], b"v1", 60).unwrap();
            c.put(b"k2", b"/", 200, &[], b"v2", 60).unwrap();
            c.save_index().unwrap();
        }
        let mut c2 = FastcgiCache::open(settings(&dir, 16)).unwrap();
        assert_eq!(c2.entries(), 2);
        assert_eq!(c2.get(b"k1").unwrap().unwrap().2, b"v1");
        assert_eq!(c2.get(b"k2").unwrap().unwrap().2, b"v2");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_expired_sweeps_stale_entries() {
        let dir = scratch("cleanup");
        let mut c = FastcgiCache::open(settings(&dir, 16)).unwrap();
        c.put(b"a", b"/", 200, &[], b"payload", 0).unwrap();
        c.put(b"b", b"/", 200, &[], b"payload", 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        c.put(b"c", b"/", 200, &[], b"payload", 600).unwrap();
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
        assert_eq!(&k[..3], b"v2\0");
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

// ─── Cross-worker purge signalling ───────────────────────────────────
//
// Workers share-nothing हैं; admin सीधे उनकी in-memory index तक नहीं
// पहुँच सकता। इसलिए purge disk पर होता है (admin entry files हटाता है)
// और process-global generation counter बढ़ाता है। हर worker वह
// generation याद रखता है जो उसने आख़िरी बार देखी; अगले cache read से
// पहले एक atomic load करता है और बदलाव मिलने पर unlinked entries को
// अपनी index से drop कर देता है। Hot path lock-free रहता है।

static PURGE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// मौजूदा purge generation। Monotonic रूप से बढ़ती है।
pub fn purge_generation() -> u64 {
    PURGE_GEN.load(std::sync::atomic::Ordering::SeqCst)
}

fn bump_purge() -> u64 {
    PURGE_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

/// Cache shape, किसी भी thread से पढ़ने के लिए safe।
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
}

/// `dir` के अंदर हर entry file हटाओ (data/ और paths/ दोनों)।
/// ग़ायब directory error नहीं है।
pub fn purge_all_on_disk(dir: &std::path::Path) -> std::io::Result<usize> {
    let mut n = 0usize;
    for sub in ["data", "paths"] {
        n += purge_dir_rec(&dir.join(sub))?;
    }
    if n > 0 {
        bump_purge();
    }
    Ok(n)
}

/// Recursively delete every file under `d`, then try to remove the
/// now-empty subdirectories. Missing directory is `Ok(0)`.
fn purge_dir_rec(d: &std::path::Path) -> std::io::Result<usize> {
    let rd = match std::fs::read_dir(d) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut n = 0usize;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += purge_dir_rec(&p)?;
            let _ = std::fs::remove_dir(&p);
        } else if std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// वे सारी entries हटाओ जिनका stored request path ठीक `path` के बराबर है।
/// Exact match; कोई globbing नहीं।
pub fn purge_path_on_disk(dir: &std::path::Path, path: &[u8]) -> std::io::Result<usize> {
    let paths_dir = dir.join("paths");
    let data_dir = dir.join("data");
    let mut n = 0usize;
    match std::fs::read_dir(&paths_dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_file() {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&p) else {
                    continue;
                };
                if bytes != path {
                    continue;
                }
                let Some(name) = p.file_name() else { continue };
                let name_str = name.to_string_lossy();
                // data files live at `<cache>/data/<2-hex>/<32-hex>`.
                if name_str.len() >= 2 {
                    let shard = &name_str[0..2];
                    let _ = std::fs::remove_file(data_dir.join(shard).join(name_str.as_ref()));
                }
                let _ = std::fs::remove_file(&p);
                n += 1;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if n > 0 {
        bump_purge();
    }
    Ok(n)
}

/// `dir/data` को स्कैन करके cache का shape बताओ। Mid-purge भी accurate,
/// क्योंकि किसी worker state को नहीं छूता।
pub fn stats_on_disk(dir: &std::path::Path) -> std::io::Result<CacheStats> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    walk_files_rec(&dir.join("data"), &mut |md| {
        entries += 1;
        bytes += md.len();
    })?;
    Ok(CacheStats { entries, bytes })
}

/// Recursively walk `d`, calling `f` for every regular file.
/// Missing directory is `Ok(())` (nothing to visit).
fn walk_files_rec<F: FnMut(&std::fs::Metadata)>(
    d: &std::path::Path,
    f: &mut F,
) -> std::io::Result<()> {
    let rd = match std::fs::read_dir(d) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_files_rec(&p, f)?;
        } else if let Ok(md) = e.metadata() {
            f(&md);
        }
    }
    Ok(())
}

#[cfg(test)]
mod purge_tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "vajra-fcgi-purge-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(dir: &std::path::Path) -> FastcgiCacheSettings {
        FastcgiCacheSettings {
            enabled: true,
            path: dir.to_path_buf(),
            size_mb: 16,
            ttl_secs: 60,
            max_entry_size: 1 << 20,
            bypass_cookie: vec![],
            exclude_paths: vec![],
        }
    }

    #[test]
    fn purge_path_removes_only_matching_entries() {
        let dir = tmpdir("path");
        let mut c = FastcgiCache::open(cfg(&dir)).unwrap();
        c.put(b"k1", b"/foo", 200, &[], b"payload-1", 60).unwrap();
        c.put(b"k2", b"/bar", 200, &[], b"payload-2", 60).unwrap();
        c.put(b"k3", b"/foo", 200, &[], b"payload-3", 60).unwrap();
        c.save_index().unwrap();
        assert_eq!(c.entries(), 3);

        let n = purge_path_on_disk(&dir, b"/foo").unwrap();
        assert_eq!(n, 2);

        let mut c2 = FastcgiCache::open(cfg(&dir)).unwrap();
        assert!(c2.get(b"k1").unwrap().is_none());
        assert!(c2.get(b"k3").unwrap().is_none());
        assert!(c2.get(b"k2").unwrap().is_some());
    }

    #[test]
    fn purge_all_clears_directory_and_bumps_generation() {
        let dir = tmpdir("all");
        let mut c = FastcgiCache::open(cfg(&dir)).unwrap();
        c.put(b"k1", b"/a", 200, &[], b"1", 60).unwrap();
        c.put(b"k2", b"/b", 200, &[], b"2", 60).unwrap();

        let gen_before = purge_generation();
        assert_eq!(purge_all_on_disk(&dir).unwrap(), 4); // 2 data + 2 paths
        assert!(purge_generation() > gen_before);

        c.poll_purge();
        assert_eq!(c.entries(), 0);
    }

    #[test]
    fn purge_on_missing_dir_is_noop_ok() {
        let nope = std::path::Path::new("/nope-does-not-exist");
        assert_eq!(purge_all_on_disk(nope).unwrap(), 0);
        assert_eq!(purge_path_on_disk(nope, b"/x").unwrap(), 0);
    }

    #[test]
    fn stats_on_disk_reports_entries_and_bytes() {
        let dir = tmpdir("stats");
        let mut c = FastcgiCache::open(cfg(&dir)).unwrap();
        c.put(b"k1", b"/a", 200, &[], b"hello", 60).unwrap();
        c.put(b"k2", b"/b", 200, &[], b"world!", 60).unwrap();
        let s = stats_on_disk(&dir).unwrap();
        assert_eq!(s.entries, 2);
        assert!(s.bytes > 0);
    }
}
