//! TOML configuration.
//!
//! ```toml
//! [server]
//! listen     = "0.0.0.0:8080"
//! workers    = 0          # 0 = one per CPU
//! pin        = true
//! grace_secs = 5          # graceful-shutdown deadline
//!
//! [limits]
//! max_conns                = 65536     # per worker
//! read_buf_size            = 8192
//! ring_entries             = 1024
//! max_body_bytes           = 1048576
//! max_proxy_response_bytes = 16777216
//!
//! [tls]
//! listen = "0.0.0.0:8443"
//! cert   = "cert.pem"
//! key    = "key.pem"
//!
//! [static]
//! root = "./public"
//!
//! [cache]                       # per-worker proxy cache limits
//! max_entries       = 10000
//! max_bytes         = 67108864
//! max_object_bytes  = 1048576
//!
//! [logging]
//! access_log = "logs/access.log"   # "-" = stdout; omit to disable
//!
//! [admin]                       # metrics + reload endpoint (no authentication!)
//! listen = "127.0.0.1:9100"
//!
//! [[proxy]]
//! prefix        = "/api/"
//! upstreams     = ["10.0.0.1:9000", "10.0.0.2:9000"]   # or: upstream = "10.0.0.1:9000"
//! balance       = "round_robin"    # round_robin | least_conn | ip_hash
//! strip_prefix  = false
//! timeout_secs  = 30
//! max_fails     = 3                # consecutive failures before an upstream is skipped
//! fail_timeout_secs = 10           # ... for this long
//! cache         = false
//! cache_default_ttl_secs = 0       # freshness when upstream sends no Cache-Control
//!
//! [php]                            # FastCGI / PHP-FPM; requires [static] (its root is the document root)
//! upstream      = "unix:/run/php/php8.3-fpm.sock"   # or "127.0.0.1:9000", or upstreams = [...]
//! index         = ["index.php"]    # directory index scripts
//! front_controller = "/index.php"  # "" disables (WordPress pretty permalinks need it)
//! extensions    = [".php"]
//! deny_exec     = ["/wp-content/uploads/"]   # scripts under these prefixes never run
//! script_root   = "/var/www/html"  # document root as PHP-FPM sees it (containers); default: static root
//! timeout_secs  = 60               # also: balance, max_fails, fail_timeout_secs
//! ```
//!
//! Unknown keys are rejected. Relative paths resolve against the config file's directory.
//!
//! ## Hot reload
//! [`Dynamic`] is the reloadable subset: static root, proxy routes, TLS
//! certificate/key, cache limits and the access-log path. Everything else
//! needs a restart; [`Settings::restart_required`] reports what changed.

use crate::cache::fastcgi::{build_fastcgi_cache, FastcgiCacheSettings, RawFastcgiCache};
use crate::compress::{build_compression, CompressionSettings, RawCompression};
use crate::php::PhpConfig;
use crate::rate_limit::{build_rate_limit, RateLimitSettings, RawRateLimit};
use crate::worker::Config as WorkerConfig;
use serde::Deserialize;
use serde::Serialize;

/// Access-log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}
use crate::access::{build_access, AccessSettings, RawAccess};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
const DEFAULT_INDEX: &str = "index.html";
const MAX_UPSTREAMS: usize = 64;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthConfig {
    #[serde(default = "default_health_listen")]
    pub listen: String,
    #[serde(default = "default_health_path")]
    pub path: String,
    #[serde(default = "default_health_ready_path")]
    pub ready_path: String,
    #[serde(default = "default_health_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub mariadb: Option<String>,
}

fn default_health_listen() -> String {
    "127.0.0.1:9201".into()
}
fn default_health_path() -> String {
    "/health".into()
}
fn default_health_ready_path() -> String {
    "/ready".into()
}
fn default_health_enabled() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub health: Option<HealthConfig>,
    pub listen: SocketAddr,
    /// 0 means "one per CPU" (resolved by `main`).
    pub workers: usize,
    pub pin: bool,
    pub grace_secs: u64,
    pub worker: WorkerConfig,
    pub static_files: Option<StaticSettings>,
    pub tls: Option<TlsSettings>,
    pub quic: Option<QuicSettings>,
    pub proxies: Vec<ProxySettings>,
    pub cache: CacheSettings,
    pub access_log: Option<String>,
    pub log_format: LogFormat,
    pub admin: Option<SocketAddr>,
    pub headers: HeaderSettings,
    pub access: AccessSettings,
    pub rate_limit: RateLimitSettings,
    pub compression: CompressionSettings,
    pub fastcgi_cache: FastcgiCacheSettings,

    /// Day 23: auto-backup policy. Executor is `vajra backup run`.
    pub backup: BackupConfig,
}

#[derive(Debug, Clone)]
pub struct StaticSettings {
    pub root: PathBuf,
    pub index: String,
    pub cache_ttl_secs: u64,
    pub max_cached_files: usize,
}

#[derive(Debug, Clone)]
pub struct TlsSettings {
    pub listen: SocketAddr,
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// HTTP/3 over QUIC (UDP). Uses the `[tls]` certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicSettings {
    pub listen: SocketAddr,
    /// Require a Retry round trip (address validation) before a handshake starts.
    pub retry: bool,
    pub idle_timeout_secs: u64,
    pub max_streams: u32,
    /// Send `Alt-Svc: h3=":port"` on HTTP/1.1 and HTTP/2 responses.
    pub advertise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Balance {
    RoundRobin,
    LeastConn,
    IpHash,
}

/// An upstream endpoint: a TCP address or a Unix domain socket (`unix:/run/php/php-fpm.sock`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UpAddr {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl From<SocketAddr> for UpAddr {
    fn from(a: SocketAddr) -> Self {
        UpAddr::Tcp(a)
    }
}

impl std::fmt::Display for UpAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpAddr::Tcp(a) => write!(f, "{a}"),
            UpAddr::Unix(p) => write!(f, "unix:{}", p.display()),
        }
    }
}

/// `host:port` or `unix:/absolute/path`.
fn parse_upstream(what: &str, s: &str) -> Result<UpAddr, String> {
    match s.strip_prefix("unix:") {
        Some(path) => {
            if !path.starts_with('/') || path.len() > 107 || path.contains('\0') {
                return Err(format!(
                    "{what} {s:?}: unix socket path must be absolute and shorter than 108 bytes"
                ));
            }
            Ok(UpAddr::Unix(PathBuf::from(path)))
        }
        None => Ok(UpAddr::Tcp(resolve_addr(what, s)?)),
    }
}

#[derive(Debug, Clone)]
pub struct ProxySettings {
    pub prefix: String,
    pub upstreams: Vec<UpAddr>,
    pub balance: Balance,
    pub strip_prefix: bool,
    pub timeout_secs: u64,
    pub max_fails: u32,
    pub fail_timeout_secs: u64,
    pub cache: bool,
    pub cache_default_ttl_secs: u64,
    /// `Some`: this route speaks FastCGI to PHP-FPM and is chosen by the PHP
    /// resolver instead of by `prefix`.
    pub php: Option<PhpConfig>,
}

impl ProxySettings {
    /// A single-upstream route with defaults for everything else.
    pub fn simple(
        prefix: &str,
        upstream: SocketAddr,
        strip_prefix: bool,
        timeout_secs: u64,
    ) -> Self {
        Self {
            prefix: prefix.to_string(),
            upstreams: vec![UpAddr::Tcp(upstream)],
            balance: Balance::RoundRobin,
            strip_prefix,
            timeout_secs,
            max_fails: 3,
            fail_timeout_secs: 10,
            cache: false,
            cache_default_ttl_secs: 0,
            php: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_object_bytes: usize,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_bytes: 64 * 1024 * 1024,
            max_object_bytes: 1024 * 1024,
        }
    }
}

/// Everything a worker can swap at runtime without dropping connections.
#[derive(Debug, Clone, Default)]
pub struct Dynamic {
    pub static_files: Option<StaticSettings>,
    pub proxies: Vec<ProxySettings>,
    /// `None` leaves the current TLS configuration untouched.
    pub tls: Option<TlsPaths>,
    pub access_log: Option<String>,
    pub log_format: LogFormat,
    pub cache: CacheSettings,
    pub headers: HeaderSettings,
    pub access: AccessSettings,
    pub rate_limit: RateLimitSettings,
    pub compression: CompressionSettings,
    pub fastcgi_cache: FastcgiCacheSettings,
}

impl StaticSettings {
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::build(root.to_path_buf(), DEFAULT_INDEX.to_string(), 2, 4096)
    }

    fn build(root: PathBuf, index: String, ttl: u64, max: usize) -> Result<Self, String> {
        let canon = std::fs::canonicalize(&root)
            .map_err(|e| format!("static.root {}: {e}", root.display()))?;
        if !canon.is_dir() {
            return Err(format!(
                "static.root {} is not a directory",
                canon.display()
            ));
        }
        if index.is_empty() || index.contains('/') {
            return Err("static.index must be a bare file name".into());
        }
        if max == 0 {
            return Err("static.max_cached_files must be >= 1".into());
        }
        Ok(Self {
            root: canon,
            index,
            cache_ttl_secs: ttl,
            max_cached_files: max,
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            health: None,
            listen: DEFAULT_LISTEN.parse().unwrap(),
            workers: 1,
            pin: true,
            grace_secs: 5,
            worker: WorkerConfig::default(),
            static_files: None,
            tls: None,
            quic: None,
            proxies: Vec::new(),
            cache: CacheSettings::default(),
            access_log: None,
            log_format: LogFormat::Text,
            admin: None,
            headers: HeaderSettings::default(),
            access: AccessSettings::default(),
            rate_limit: RateLimitSettings::default(),
            compression: CompressionSettings::default(),
            fastcgi_cache: FastcgiCacheSettings::default(),
            backup: BackupConfig::default(),
        }
    }
}

// ---- raw (serde) shapes ---------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    health: Option<HealthConfig>,
    #[serde(default)]
    server: RawServer,
    #[serde(default)]
    limits: RawLimits,
    #[serde(rename = "static")]
    static_files: Option<RawStatic>,
    tls: Option<RawTls>,
    quic: Option<RawQuic>,
    #[serde(default)]
    proxy: Vec<RawProxy>,
    php: Option<RawPhp>,
    #[serde(default)]
    cache: RawCache,
    #[serde(default)]
    logging: RawLogging,
    admin: Option<RawAdmin>,
    #[serde(default)]
    headers: Option<RawHeaders>,
    access: Option<RawAccess>,
    rate_limit: Option<RawRateLimit>,
    compression: Option<RawCompression>,
    fastcgi_cache: Option<RawFastcgiCache>,

    /// Day 26: [backup] section, parsed by the scheduler.
    #[serde(default)]
    backup: Option<BackupConfig>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServer {
    listen: String,
    workers: usize,
    pin: bool,
    grace_secs: u64,
}
impl Default for RawServer {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.into(),
            workers: 1,
            pin: true,
            grace_secs: 5,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawLimits {
    max_conns: usize,
    read_buf_size: usize,
    ring_entries: u32,
    max_body_bytes: usize,
    max_proxy_response_bytes: usize,
}
impl Default for RawLimits {
    fn default() -> Self {
        let d = WorkerConfig::default();
        Self {
            max_conns: d.max_conns,
            read_buf_size: d.read_buf_size,
            ring_entries: d.ring_entries,
            max_body_bytes: d.max_body_bytes,
            max_proxy_response_bytes: d.max_proxy_response_bytes,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStatic {
    root: String,
    #[serde(default = "default_index")]
    index: String,
    #[serde(default = "default_ttl")]
    cache_ttl_secs: u64,
    #[serde(default = "default_max_cached")]
    max_cached_files: usize,
}
fn default_index() -> String {
    DEFAULT_INDEX.into()
}
fn default_ttl() -> u64 {
    2
}
fn default_max_cached() -> usize {
    4096
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTls {
    listen: String,
    cert: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuic {
    listen: String,
    #[serde(default = "yes")]
    retry: bool,
    #[serde(default = "default_quic_idle")]
    idle_timeout_secs: u64,
    #[serde(default = "default_quic_streams")]
    max_streams: u32,
    #[serde(default = "yes")]
    advertise: bool,
}
fn yes() -> bool {
    true
}
fn default_quic_idle() -> u64 {
    30
}
fn default_quic_streams() -> u32 {
    100
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProxy {
    prefix: String,
    upstream: Option<String>,
    upstreams: Option<Vec<String>>,
    balance: Option<String>,
    #[serde(default)]
    strip_prefix: bool,
    #[serde(default = "default_proxy_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_max_fails")]
    max_fails: u32,
    #[serde(default = "default_fail_timeout")]
    fail_timeout_secs: u64,
    #[serde(default)]
    cache: bool,
    #[serde(default)]
    cache_default_ttl_secs: u64,
}
fn default_proxy_timeout() -> u64 {
    30
}
fn default_max_fails() -> u32 {
    3
}
fn default_fail_timeout() -> u64 {
    10
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPhp {
    upstream: Option<String>,
    upstreams: Option<Vec<String>>,
    #[serde(default = "default_php_index")]
    index: Vec<String>,
    /// `""` disables the front controller.
    #[serde(default = "default_front_controller")]
    front_controller: String,
    #[serde(default = "default_php_ext")]
    extensions: Vec<String>,
    #[serde(default = "default_deny_exec")]
    deny_exec: Vec<String>,
    /// Document root as PHP-FPM sees it (containers); defaults to `[static] root`.
    script_root: Option<String>,
    balance: Option<String>,
    #[serde(default = "default_php_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_max_fails")]
    max_fails: u32,
    #[serde(default = "default_fail_timeout")]
    fail_timeout_secs: u64,
}
fn default_php_index() -> Vec<String> {
    vec!["index.php".into()]
}
fn default_front_controller() -> String {
    "/index.php".into()
}
fn default_php_ext() -> Vec<String> {
    vec![".php".into()]
}
fn default_deny_exec() -> Vec<String> {
    vec!["/wp-content/uploads/".into()]
}
fn default_php_timeout() -> u64 {
    60
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawCache {
    max_entries: usize,
    max_bytes: usize,
    max_object_bytes: usize,
}
impl Default for RawCache {
    fn default() -> Self {
        let d = CacheSettings::default();
        Self {
            max_entries: d.max_entries,
            max_bytes: d.max_bytes,
            max_object_bytes: d.max_object_bytes,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawLogging {
    access_log: Option<String>,
    format: LogFormat,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAdmin {
    listen: String,
}

fn resolve(base: &Path, p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn resolve_addr(what: &str, s: &str) -> Result<SocketAddr, String> {
    s.to_socket_addrs()
        .map_err(|e| format!("{what} {s:?}: {e}"))?
        .next()
        .ok_or_else(|| format!("{what} {s:?} did not resolve"))
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml_str(&text, base).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn from_toml_str(text: &str, base_dir: &Path) -> Result<Self, String> {
        let raw: Raw = toml::from_str(text).map_err(|e| e.to_string())?;

        let listen: SocketAddr = raw
            .server
            .listen
            .parse()
            .map_err(|e| format!("server.listen {:?}: {e}", raw.server.listen))?;
        if !(1..=3600).contains(&raw.server.grace_secs) {
            return Err("server.grace_secs must be in 1..=3600".into());
        }

        let l = &raw.limits;
        if !(1..=1 << 20).contains(&l.max_conns) {
            return Err("limits.max_conns must be in 1..=1048576".into());
        }
        if !(1024..=1 << 20).contains(&l.read_buf_size) {
            return Err("limits.read_buf_size must be in 1024..=1048576".into());
        }
        if !(16..=32768).contains(&l.ring_entries) {
            return Err("limits.ring_entries must be in 16..=32768".into());
        }
        if l.max_body_bytes > 1 << 30 || l.max_proxy_response_bytes > 1 << 30 {
            return Err("limits.max_body_bytes / max_proxy_response_bytes must be <= 1 GiB".into());
        }

        let static_files = match raw.static_files {
            None => None,
            Some(s) => Some(StaticSettings::build(
                resolve(base_dir, &s.root),
                s.index,
                s.cache_ttl_secs,
                s.max_cached_files,
            )?),
        };

        let tls = match raw.tls {
            None => None,
            Some(t) => {
                let cert = resolve(base_dir, &t.cert);
                let key = resolve(base_dir, &t.key);
                for (what, p) in [("tls.cert", &cert), ("tls.key", &key)] {
                    if !p.is_file() {
                        return Err(format!("{what}: {} is not a file", p.display()));
                    }
                }
                Some(TlsSettings {
                    listen: resolve_addr("tls.listen", &t.listen)?,
                    cert,
                    key,
                })
            }
        };

        let quic = match raw.quic {
            None => None,
            Some(q) => {
                if tls.is_none() {
                    return Err(
                        "[quic] requires a [tls] section (the same certificate is used)".into(),
                    );
                }
                if !(1..=600).contains(&q.idle_timeout_secs) {
                    return Err("quic.idle_timeout_secs must be in 1..=600".into());
                }
                if !(1..=1000).contains(&q.max_streams) {
                    return Err("quic.max_streams must be in 1..=1000".into());
                }
                Some(QuicSettings {
                    listen: resolve_addr("quic.listen", &q.listen)?,
                    retry: q.retry,
                    idle_timeout_secs: q.idle_timeout_secs,
                    max_streams: q.max_streams,
                    advertise: q.advertise,
                })
            }
        };

        let mut proxies = Vec::with_capacity(raw.proxy.len());
        for p in raw.proxy {
            if !p.prefix.starts_with('/') {
                return Err(format!("proxy.prefix {:?} must start with '/'", p.prefix));
            }
            if !(1..=3600).contains(&p.timeout_secs) {
                return Err("proxy.timeout_secs must be in 1..=3600".into());
            }
            if p.max_fails == 0 || !(1..=3600).contains(&p.fail_timeout_secs) {
                return Err(
                    "proxy.max_fails must be >= 1 and fail_timeout_secs in 1..=3600".into(),
                );
            }
            let names: Vec<String> = match (p.upstream, p.upstreams) {
                (Some(u), None) => vec![u],
                (None, Some(us)) => us,
                _ => {
                    return Err(format!(
                        "proxy {:?}: set exactly one of `upstream` or `upstreams`",
                        p.prefix
                    ))
                }
            };
            if names.is_empty() || names.len() > MAX_UPSTREAMS {
                return Err(format!(
                    "proxy {:?}: need 1..={MAX_UPSTREAMS} upstreams",
                    p.prefix
                ));
            }
            let mut upstreams = Vec::with_capacity(names.len());
            for n in &names {
                let a = parse_upstream("proxy.upstream", n)?;
                if !upstreams.contains(&a) {
                    upstreams.push(a);
                }
            }
            let balance = match p.balance.as_deref().unwrap_or("round_robin") {
                "round_robin" => Balance::RoundRobin,
                "least_conn" => Balance::LeastConn,
                "ip_hash" => Balance::IpHash,
                other => {
                    return Err(format!(
                        "proxy.balance {other:?}: expected round_robin, least_conn or ip_hash"
                    ))
                }
            };
            proxies.push(ProxySettings {
                prefix: p.prefix,
                upstreams,
                balance,
                strip_prefix: p.strip_prefix,
                timeout_secs: p.timeout_secs,
                max_fails: p.max_fails,
                fail_timeout_secs: p.fail_timeout_secs,
                cache: p.cache,
                cache_default_ttl_secs: p.cache_default_ttl_secs,
                php: None,
            });
        }

        if let Some(php) = raw.php {
            let root = match &static_files {
                Some(sf) => sf.root.clone(),
                None => {
                    return Err(
                        "[php] requires a [static] section (its root is the PHP document root)"
                            .into(),
                    )
                }
            };
            if !(1..=3600).contains(&php.timeout_secs) {
                return Err("php.timeout_secs must be in 1..=3600".into());
            }
            if php.max_fails == 0 || !(1..=3600).contains(&php.fail_timeout_secs) {
                return Err("php.max_fails must be >= 1 and fail_timeout_secs in 1..=3600".into());
            }
            let names: Vec<String> = match (php.upstream, php.upstreams) {
                (Some(u), None) => vec![u],
                (None, Some(us)) => us,
                _ => return Err("php: set exactly one of `upstream` or `upstreams`".into()),
            };
            if names.is_empty() || names.len() > MAX_UPSTREAMS {
                return Err(format!("php: need 1..={MAX_UPSTREAMS} upstreams"));
            }
            let mut upstreams = Vec::with_capacity(names.len());
            for n in &names {
                let a = parse_upstream("php.upstream", n)?;
                if !upstreams.contains(&a) {
                    upstreams.push(a);
                }
            }
            let balance = match php.balance.as_deref().unwrap_or("round_robin") {
                "round_robin" => Balance::RoundRobin,
                "least_conn" => Balance::LeastConn,
                other => {
                    return Err(format!(
                        "php.balance {other:?}: expected round_robin or least_conn"
                    ))
                }
            };
            if php.index.is_empty() || php.index.iter().any(|i| i.is_empty() || i.contains('/')) {
                return Err("php.index must be a non-empty list of bare file names".into());
            }
            let front_controller = if php.front_controller.is_empty() {
                None
            } else {
                let f = php.front_controller;
                if !f.starts_with('/') || f.split('/').any(|seg| seg == "..") {
                    return Err(
                        "php.front_controller must be an absolute path below the root (or \"\")"
                            .into(),
                    );
                }
                Some(f)
            };
            let mut extensions = Vec::new();
            for e in &php.extensions {
                let e = e.to_ascii_lowercase();
                if e.len() < 2 || e.len() > 16 || !e.starts_with('.') || e.contains('/') {
                    return Err(format!("php.extensions: {e:?} must look like \".php\""));
                }
                extensions.push(e);
            }
            if extensions.is_empty() {
                return Err("php.extensions must not be empty".into());
            }
            let mut deny_exec = Vec::new();
            for d in &php.deny_exec {
                if !d.starts_with('/') {
                    return Err(format!("php.deny_exec: {d:?} must start with '/'"));
                }
                deny_exec.push(d.to_ascii_lowercase());
            }
            let fpm_root = match php.script_root {
                Some(r) => {
                    if !r.starts_with('/') {
                        return Err("php.script_root must be an absolute path".into());
                    }
                    r
                }
                None => root.to_string_lossy().into_owned(),
            };
            proxies.push(ProxySettings {
                prefix: String::new(),
                upstreams,
                balance,
                strip_prefix: false,
                timeout_secs: php.timeout_secs,
                max_fails: php.max_fails,
                fail_timeout_secs: php.fail_timeout_secs,
                cache: false,
                cache_default_ttl_secs: 0,
                php: Some(PhpConfig {
                    root,
                    fpm_root,
                    index: php.index,
                    front_controller,
                    extensions,
                    deny_exec,
                }),
            });
        }

        let c = &raw.cache;
        if c.max_object_bytes > c.max_bytes && c.max_bytes > 0 {
            return Err("cache.max_object_bytes must not exceed cache.max_bytes".into());
        }

        let access_log = raw.logging.access_log.map(|p| {
            if p == "-" {
                p
            } else {
                resolve(base_dir, &p).to_string_lossy().into_owned()
            }
        });
        let log_format = raw.logging.format;
        let admin = match raw.admin {
            None => None,
            Some(a) => Some(resolve_addr("admin.listen", &a.listen)?),
        };

        Ok(Self {
            headers: build_headers(raw.headers)?,
            access: build_access(raw.access.unwrap_or_default())?,
            rate_limit: build_rate_limit(raw.rate_limit.unwrap_or_default())?,
            compression: build_compression(raw.compression.unwrap_or_default())?,
            fastcgi_cache: build_fastcgi_cache(raw.fastcgi_cache.unwrap_or_default())?,
            backup: raw.backup.unwrap_or_default(),
            health: raw.health,
            listen,
            workers: raw.server.workers,
            pin: raw.server.pin,
            grace_secs: raw.server.grace_secs,
            worker: WorkerConfig {
                ring_entries: l.ring_entries,
                read_buf_size: l.read_buf_size,
                max_conns: l.max_conns,
                max_body_bytes: l.max_body_bytes,
                max_proxy_response_bytes: l.max_proxy_response_bytes,
            },
            static_files,
            tls,
            quic,
            proxies,
            cache: CacheSettings {
                max_entries: c.max_entries,
                max_bytes: c.max_bytes,
                max_object_bytes: c.max_object_bytes,
            },
            access_log,
            log_format,
            admin,
        })
    }

    /// The reloadable subset.
    pub fn dynamic(&self) -> Dynamic {
        Dynamic {
            static_files: self.static_files.clone(),
            proxies: self.proxies.clone(),
            tls: self.tls.as_ref().map(|t| TlsPaths {
                cert: t.cert.clone(),
                key: t.key.clone(),
            }),
            access_log: self.access_log.clone(),
            log_format: self.log_format,
            cache: self.cache,
            headers: self.headers.clone(),
            ..Default::default()
        }
    }

    /// Settings that differ between `self` (running) and `new` but cannot be
    /// applied without a restart.
    pub fn restart_required(&self, new: &Settings) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.listen != new.listen {
            v.push("server.listen");
        }
        if self.workers != new.workers {
            v.push("server.workers");
        }
        if self.pin != new.pin {
            v.push("server.pin");
        }
        if self.worker != new.worker {
            v.push("[limits]");
        }
        if self.tls.as_ref().map(|t| t.listen) != new.tls.as_ref().map(|t| t.listen) {
            v.push("tls.listen (adding/removing/moving the HTTPS listener)");
        }
        if self.quic != new.quic {
            v.push("[quic] (adding/removing/changing the HTTP/3 listener)");
        }
        if self.admin != new.admin {
            v.push("admin.listen");
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vajra-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn parse(t: &str) -> Result<Settings, String> {
        Settings::from_toml_str(t, Path::new("."))
    }

    #[test]
    fn empty_config_gives_defaults() {
        let s = parse("").unwrap();
        assert_eq!(s.listen, "0.0.0.0:8080".parse().unwrap());
        assert_eq!((s.workers, s.grace_secs), (1, 5));
        assert!(s.pin && s.static_files.is_none() && s.tls.is_none() && s.proxies.is_empty());
        assert!(s.access_log.is_none() && s.admin.is_none());
        assert_eq!(s.cache, CacheSettings::default());
    }

    #[test]
    fn php_section() {
        let d = tmpdir("php");
        let base = format!("[static]\nroot = {:?}\n", d.to_str().unwrap());
        let s = parse(&format!(
            "{base}[php]\nupstream = \"unix:/run/php/php8.3-fpm.sock\"\n"
        ))
        .unwrap();
        let p = s.proxies.last().unwrap();
        assert_eq!(
            p.upstreams,
            vec![UpAddr::Unix(PathBuf::from("/run/php/php8.3-fpm.sock"))]
        );
        assert_eq!(p.timeout_secs, 60);
        let php = p.php.as_ref().unwrap();
        assert_eq!(php.index, ["index.php"]);
        assert_eq!(php.front_controller.as_deref(), Some("/index.php"));
        assert_eq!(php.extensions, [".php"]);
        assert_eq!(php.deny_exec, ["/wp-content/uploads/"]);
        assert_eq!(php.root, php.root.canonicalize().unwrap());
        assert_eq!(php.fpm_root, php.root.to_string_lossy());

        let s = parse(&format!(
            "{base}[php]\nupstreams = [\"127.0.0.1:9000\", \"127.0.0.1:9001\"]\nfront_controller = \"\"\nextensions = [\".PHP\", \".phtml\"]\nscript_root = \"/var/www/html\"\ndeny_exec = [\"/Uploads/\"]\n"
        ))
        .unwrap();
        let php = s.proxies.last().unwrap().php.clone().unwrap();
        assert_eq!(php.front_controller, None);
        assert_eq!(php.extensions, [".php", ".phtml"]);
        assert_eq!(php.fpm_root, "/var/www/html");
        assert_eq!(php.deny_exec, ["/uploads/"]);
        assert_eq!(s.proxies.last().unwrap().upstreams.len(), 2);

        // Mixed with a normal proxy route: both survive.
        let s = parse(&format!(
            "{base}[[proxy]]\nprefix = \"/api/\"\nupstream = \"127.0.0.1:1\"\n[php]\nupstream = \"127.0.0.1:9000\"\n"
        ))
        .unwrap();
        assert_eq!(s.proxies.len(), 2);
        assert!(s.proxies[0].php.is_none() && s.proxies[1].php.is_some());
    }

    #[test]
    fn php_section_errors() {
        let d = tmpdir("phperr");
        let base = format!("[static]\nroot = {:?}\n", d.to_str().unwrap());
        assert!(
            parse("[php]\nupstream = \"127.0.0.1:9000\"").is_err(),
            "needs [static]"
        );
        for bad in [
            "", // no upstream
            "upstream = \"127.0.0.1:9000\"\nupstreams = [\"127.0.0.1:1\"]",
            "upstream = \"unix:relative.sock\"",
            "upstream = \"127.0.0.1:9000\"\nextensions = [\"php\"]",
            "upstream = \"127.0.0.1:9000\"\nextensions = []",
            "upstream = \"127.0.0.1:9000\"\nindex = [\"a/b.php\"]",
            "upstream = \"127.0.0.1:9000\"\nfront_controller = \"index.php\"",
            "upstream = \"127.0.0.1:9000\"\nfront_controller = \"/../x.php\"",
            "upstream = \"127.0.0.1:9000\"\ndeny_exec = [\"uploads\"]",
            "upstream = \"127.0.0.1:9000\"\nscript_root = \"rel\"",
            "upstream = \"127.0.0.1:9000\"\ntimeout_secs = 0",
            "upstream = \"127.0.0.1:9000\"\nbalance = \"ip_hash\"",
            "upstream = \"127.0.0.1:9000\"\nunknown = 1",
        ] {
            assert!(
                parse(&format!("{base}[php]\n{bad}\n")).is_err(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn unix_proxy_upstream() {
        let s = parse("[[proxy]]\nprefix = \"/a/\"\nupstream = \"unix:/tmp/app.sock\"").unwrap();
        assert_eq!(s.proxies[0].upstreams[0].to_string(), "unix:/tmp/app.sock");
        assert!(parse("[[proxy]]\nprefix = \"/a/\"\nupstream = \"unix:app.sock\"").is_err());
    }

    #[test]
    fn quic_section() {
        let base = tmpdir("quic");
        std::fs::write(base.join("c.pem"), "x").unwrap();
        std::fs::write(base.join("k.pem"), "x").unwrap();
        let tls = "[tls]\nlisten=\"127.0.0.1:9443\"\ncert=\"c.pem\"\nkey=\"k.pem\"\n";
        let ok = format!("{tls}[quic]\nlisten=\"127.0.0.1:9443\"\n");
        let s = Settings::from_toml_str(&ok, &base).unwrap();
        let q = s.quic.unwrap();
        assert!(q.retry && q.advertise);
        assert_eq!(
            (q.idle_timeout_secs, q.max_streams, q.listen.port()),
            (30, 100, 9443)
        );

        let custom = format!(
            "{tls}[quic]\nlisten=\"127.0.0.1:9444\"\nretry=false\nadvertise=false\nidle_timeout_secs=5\nmax_streams=7\n"
        );
        let q = Settings::from_toml_str(&custom, &base)
            .unwrap()
            .quic
            .unwrap();
        assert!(!q.retry && !q.advertise);
        assert_eq!((q.idle_timeout_secs, q.max_streams), (5, 7));

        // QUIC needs the TLS identity; limits are validated; unknown keys rejected.
        assert!(Settings::from_toml_str("[quic]\nlisten=\"127.0.0.1:1\"", &base).is_err());
        for bad in [
            "idle_timeout_secs=0",
            "idle_timeout_secs=601",
            "max_streams=0",
            "max_streams=1001",
            "bogus=1",
        ] {
            let t = format!("{tls}[quic]\nlisten=\"127.0.0.1:9443\"\n{bad}\n");
            assert!(Settings::from_toml_str(&t, &base).is_err(), "{bad}");
        }

        // Changing the HTTP/3 listener needs a restart.
        let a = Settings::from_toml_str(&ok, &base).unwrap();
        let b = Settings::from_toml_str(&custom, &base).unwrap();
        assert!(a.restart_required(&b).iter().any(|s| s.contains("quic")));
        assert!(a.restart_required(&a).is_empty());
    }

    #[test]
    fn full_config() {
        let base = tmpdir("full");
        std::fs::create_dir_all(base.join("public")).unwrap();
        std::fs::write(base.join("c.pem"), "x").unwrap();
        std::fs::write(base.join("k.pem"), "x").unwrap();
        let toml = r#"
            [server]
            listen = "127.0.0.1:9000"
            workers = 4
            pin = false
            grace_secs = 9
            [limits]
            max_conns = 100
            read_buf_size = 4096
            ring_entries = 256
            max_body_bytes = 2048
            [tls]
            listen = "127.0.0.1:9443"
            cert = "c.pem"
            key = "k.pem"
            [static]
            root = "public"
            index = "home.html"
            [cache]
            max_entries = 50
            max_bytes = 4096
            max_object_bytes = 1024
            [logging]
            access_log = "logs/access.log"
            [admin]
            listen = "127.0.0.1:9100"
            [[proxy]]
            prefix = "/api/"
            upstreams = ["127.0.0.1:7000", "127.0.0.1:7001", "127.0.0.1:7000"]
            balance = "least_conn"
            strip_prefix = true
            timeout_secs = 5
            max_fails = 2
            fail_timeout_secs = 20
            cache = true
            cache_default_ttl_secs = 60
            [[proxy]]
            prefix = "/other"
            upstream = "127.0.0.1:7002"
        "#;
        let s = Settings::from_toml_str(toml, &base).unwrap();
        assert_eq!((s.listen.port(), s.workers, s.grace_secs), (9000, 4, 9));
        assert_eq!(s.worker.max_body_bytes, 2048);
        assert_eq!(s.tls.as_ref().unwrap().listen.port(), 9443);
        assert_eq!(s.static_files.as_ref().unwrap().index, "home.html");
        assert_eq!(s.cache.max_entries, 50);
        assert_eq!(s.admin.unwrap().port(), 9100);
        assert!(s.access_log.as_ref().unwrap().ends_with("logs/access.log"));
        assert!(std::path::Path::new(s.access_log.as_ref().unwrap()).is_absolute());

        assert_eq!(s.proxies.len(), 2);
        let p = &s.proxies[0];
        assert_eq!(p.upstreams.len(), 2, "duplicates removed");
        assert_eq!(p.balance, Balance::LeastConn);
        assert!(p.strip_prefix && p.cache);
        assert_eq!(
            (
                p.timeout_secs,
                p.max_fails,
                p.fail_timeout_secs,
                p.cache_default_ttl_secs
            ),
            (5, 2, 20, 60)
        );
        let q = &s.proxies[1];
        assert_eq!(
            (q.upstreams.len(), q.balance, q.timeout_secs, q.cache),
            (1, Balance::RoundRobin, 30, false)
        );

        let d = s.dynamic();
        assert_eq!(d.proxies.len(), 2);
        assert!(d.tls.is_some() && d.static_files.is_some());
    }

    #[test]
    fn stdout_access_log_is_kept_verbatim() {
        assert_eq!(
            parse("[logging]\naccess_log = \"-\"")
                .unwrap()
                .access_log
                .as_deref(),
            Some("-")
        );
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(parse("[server]\nlisten_addr = \"x\"").is_err());
        assert!(parse("bogus = 1").is_err());
        assert!(parse("[[proxy]]\nprefix=\"/\"\nupstream=\"127.0.0.1:1\"\nx=1").is_err());
        assert!(parse("[cache]\nmax = 1").is_err());
    }

    #[test]
    fn bad_values_rejected() {
        assert!(parse("[server]\nlisten = \"nope\"").is_err());
        assert!(parse("[server]\ngrace_secs = 0").is_err());
        assert!(parse("[limits]\nmax_conns = 0").is_err());
        assert!(parse("[limits]\nread_buf_size = 10").is_err());
        assert!(parse("[static]\nroot = \"/definitely/not/here\"").is_err());
        assert!(parse("[tls]\nlisten=\"127.0.0.1:1\"\ncert=\"/no\"\nkey=\"/no\"").is_err());
        assert!(parse("[[proxy]]\nprefix=\"api\"\nupstream=\"127.0.0.1:1\"").is_err());
        assert!(
            parse("[[proxy]]\nprefix=\"/a\"\nupstream=\"127.0.0.1:1\"\ntimeout_secs=0").is_err()
        );
        assert!(parse("[[proxy]]\nprefix=\"/a\"\nupstream=\"127.0.0.1:1\"\nmax_fails=0").is_err());
        assert!(
            parse("[[proxy]]\nprefix=\"/a\"\nupstream=\"127.0.0.1:1\"\nbalance=\"random\"")
                .is_err()
        );
        assert!(parse("[[proxy]]\nprefix=\"/a\"").is_err(), "no upstream");
        assert!(parse(
            "[[proxy]]\nprefix=\"/a\"\nupstream=\"127.0.0.1:1\"\nupstreams=[\"127.0.0.1:2\"]"
        )
        .is_err());
        assert!(parse("[[proxy]]\nprefix=\"/a\"\nupstreams=[]").is_err());
        assert!(parse("[cache]\nmax_bytes = 10\nmax_object_bytes = 100").is_err());
    }

    #[test]
    fn restart_required_detection() {
        let a = parse("").unwrap();
        let mut b = a.clone();
        assert!(a.restart_required(&b).is_empty());
        b.listen = "127.0.0.1:1".parse().unwrap();
        b.workers = 8;
        b.worker.max_conns = 5;
        b.admin = Some("127.0.0.1:2".parse().unwrap());
        let r = a.restart_required(&b);
        assert_eq!(r.len(), 4, "{r:?}");
        // Reloadable fields never appear.
        let mut c = a.clone();
        c.proxies.push(ProxySettings::simple(
            "/x",
            "127.0.0.1:1".parse().unwrap(),
            false,
            5,
        ));
        c.access_log = Some("-".into());
        assert!(a.restart_required(&c).is_empty());
    }
    #[test]
    fn log_format_parsed() {
        assert_eq!(
            parse("[logging]\nformat = \"json\"").unwrap().log_format,
            LogFormat::Json
        );
        assert_eq!(
            parse("[logging]\nformat = \"text\"").unwrap().log_format,
            LogFormat::Text
        );
        assert_eq!(parse("").unwrap().log_format, LogFormat::Text);
        assert!(parse("[logging]\nformat = \"xml\"").is_err());
    }
}

// ═══════════════════════════════════════════════════════════════════
// Day 8: security headers
// ═══════════════════════════════════════════════════════════════════

/// The five managed header names, in RFC canonical spelling.
/// Lookup against response headers is case-insensitive.
pub const SECURITY_HEADERS: [&str; 6] = [
    "Strict-Transport-Security",
    "X-Frame-Options",
    "X-Content-Type-Options",
    "Referrer-Policy",
    "Permissions-Policy",
    "Content-Security-Policy",
];

/// WordPress-compatible Content-Security-Policy.
///
/// Permits the inline scripts/styles/eval that unmodified WordPress 6.x
/// and most plugins require; locks down frame-ancestors / base-uri /
/// form-action. See `docs/security-headers.md` for rationale and a
/// stronger-policy migration path.
///
/// Deliberately a string, not a magic `"wordpress"` keyword: a future
/// change to this constant cannot silently alter a running site.
pub const WORDPRESS_CSP: &str = "\
default-src 'self'; \
script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
style-src 'self' 'unsafe-inline'; \
img-src 'self' data: https:; \
font-src 'self' data:; \
connect-src 'self'; \
frame-ancestors 'self'; \
base-uri 'self'; \
form-action 'self'";

/// What to do with one managed header for a given path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action<'a> {
    /// Don't touch — leave whatever (if anything) is already there.
    Skip,
    /// Strip any existing instance.
    Remove,
    /// Strip any existing instance and set this value.
    Set(&'a str),
}

/// Runtime security-header policy (resolved from `[headers]`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderSettings {
    pub hsts: Option<String>,
    pub x_frame_options: Option<String>,
    pub x_content_type_options: Option<String>,
    pub referrer_policy: Option<String>,
    pub permissions_policy: Option<String>,
    pub content_security_policy: Option<String>,
    pub overrides: Vec<HeaderOverride>,
}

#[derive(Default)]
/// Per-path override. `Some("")` means "remove for this path".
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderOverride {
    pub path: String,
    pub hsts: Option<String>,
    pub x_frame_options: Option<String>,
    pub x_content_type_options: Option<String>,
    pub referrer_policy: Option<String>,
    pub permissions_policy: Option<String>,
    pub content_security_policy: Option<String>,
}

impl HeaderSettings {
    /// Longest matching prefix wins.
    fn best_override_for(&self, path: &str) -> Option<&HeaderOverride> {
        let mut best: Option<&HeaderOverride> = None;
        for ov in &self.overrides {
            if path.starts_with(&ov.path) && best.map_or(true, |b| ov.path.len() > b.path.len()) {
                best = Some(ov);
            }
        }
        best
    }

    /// Effective action per managed header at `path`.
    pub fn effective_at(&self, path: &str) -> [Action<'_>; 6] {
        let mut eff = [
            self.hsts.as_deref().map_or(Action::Skip, Action::Set),
            self.x_frame_options
                .as_deref()
                .map_or(Action::Skip, Action::Set),
            self.x_content_type_options
                .as_deref()
                .map_or(Action::Skip, Action::Set),
            self.referrer_policy
                .as_deref()
                .map_or(Action::Skip, Action::Set),
            self.permissions_policy
                .as_deref()
                .map_or(Action::Skip, Action::Set),
            self.content_security_policy
                .as_deref()
                .map_or(Action::Skip, Action::Set),
        ];

        if let Some(ov) = self.best_override_for(path) {
            // Inline: closures cannot be generic over 'a, so `Action<'a>`
            // can't be returned through a helper closure. Five explicit
            // maps keep the borrow checker happy and are trivial to audit.
            #[inline]
            fn act(v: &Option<String>) -> Option<Action<'_>> {
                v.as_deref().map(|s| {
                    if s.is_empty() {
                        Action::Remove
                    } else {
                        Action::Set(s)
                    }
                })
            }
            let over = [
                act(&ov.hsts),
                act(&ov.x_frame_options),
                act(&ov.x_content_type_options),
                act(&ov.referrer_policy),
                act(&ov.permissions_policy),
                act(&ov.content_security_policy),
            ];
            for (i, a) in over.iter().enumerate() {
                if let Some(a) = a {
                    eff[i] = *a;
                }
            }
        }
        eff
    }
}

/// Apply the security-header policy for `path` to `headers`.
///
/// * Managed headers already present are replaced (case-insensitive).
/// * Unmanaged headers are left alone.
/// * `Action::Remove` (from an override with `""`) strips the header entirely.
pub fn apply_security_headers(
    cfg: &HeaderSettings,
    path: &str,
    headers: &mut Vec<(String, String)>,
) {
    let eff = cfg.effective_at(path);
    for (i, name) in SECURITY_HEADERS.iter().enumerate() {
        match eff[i] {
            Action::Skip => {}
            Action::Remove => {
                headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
            }
            Action::Set(v) => {
                headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
                headers.push((name.to_string(), v.to_string()));
            }
        }
    }
}

// ─── Raw TOML shapes ─────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawHeaders {
    hsts: Option<String>,
    x_frame_options: Option<String>,
    x_content_type_options: Option<String>,
    referrer_policy: Option<String>,
    permissions_policy: Option<String>,
    content_security_policy: Option<String>,
    #[serde(default)]
    overrides: Vec<RawHeaderOverride>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeaderOverride {
    path: String,
    hsts: Option<String>,
    x_frame_options: Option<String>,
    x_content_type_options: Option<String>,
    referrer_policy: Option<String>,
    permissions_policy: Option<String>,
    content_security_policy: Option<String>,
}

/// Validate + convert raw headers section. Returns an error string on:
/// * empty top-level value (operator probably meant to omit the key)
/// * CR or LF in any value (header injection)
/// * relative / empty override path
fn build_headers(raw: Option<RawHeaders>) -> Result<HeaderSettings, String> {
    let Some(r) = raw else {
        return Ok(HeaderSettings::default());
    };

    let top = [
        ("hsts", &r.hsts),
        ("x_frame_options", &r.x_frame_options),
        ("x_content_type_options", &r.x_content_type_options),
        ("referrer_policy", &r.referrer_policy),
        ("permissions_policy", &r.permissions_policy),
        ("content_security_policy", &r.content_security_policy),
    ];
    for (field, v) in top {
        if let Some(s) = v {
            if s.is_empty() {
                return Err(format!(
                    "[headers] {field}: empty value; omit the key to skip the header"
                ));
            }
            if s.contains('\n') || s.contains('\r') {
                return Err(format!(
                    "[headers] {field}: value must not contain CR/LF (header injection)"
                ));
            }
        }
    }

    let mut overrides = Vec::with_capacity(r.overrides.len());
    for (i, o) in r.overrides.into_iter().enumerate() {
        if o.path.is_empty() || !o.path.starts_with('/') {
            return Err(format!(
                "[[headers.override]] #{i}: path must be absolute (start with '/')"
            ));
        }
        let fields = [
            ("hsts", &o.hsts),
            ("x_frame_options", &o.x_frame_options),
            ("x_content_type_options", &o.x_content_type_options),
            ("referrer_policy", &o.referrer_policy),
            ("permissions_policy", &o.permissions_policy),
            ("content_security_policy", &o.content_security_policy),
        ];
        for (field, v) in fields {
            if let Some(s) = v {
                if s.contains('\n') || s.contains('\r') {
                    return Err(format!(
                        "[[headers.override]] #{i} {field}: value must not contain CR/LF"
                    ));
                }
            }
        }
        overrides.push(HeaderOverride {
            path: o.path,
            hsts: o.hsts,
            x_frame_options: o.x_frame_options,
            x_content_type_options: o.x_content_type_options,
            referrer_policy: o.referrer_policy,
            permissions_policy: o.permissions_policy,
            content_security_policy: o.content_security_policy,
        });
    }

    Ok(HeaderSettings {
        hsts: r.hsts,
        x_frame_options: r.x_frame_options,
        x_content_type_options: r.x_content_type_options,
        referrer_policy: r.referrer_policy,
        permissions_policy: r.permissions_policy,
        content_security_policy: r.content_security_policy,
        overrides,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Day 23: [backup] config schema
//
// The `[backup]` section describes POLICY only. The executor is
// `vajra backup run` (Day 24+), fired by a systemd timer or cron.
// No scheduler runs inside the worker threads.
// ─────────────────────────────────────────────────────────────────────

/// Which backends to ship a completed archive to.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackupDestination {
    Local,
    S3,
}

/// `[backup]` — auto-backup policy. See `docs/backup.md`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    /// Master switch. When `false`, other fields are ignored.
    #[serde(default)]
    pub enabled: bool,

    /// Cron expression the operator's systemd timer is built from.
    /// Parsed at load time to catch typos early.
    #[serde(default = "BackupConfig::default_schedule")]
    pub schedule: String,

    /// Delete archives older than this many days after each run.
    #[serde(default = "BackupConfig::default_keep_days")]
    pub keep_days: u32,

    /// Where to write archives. Empty + `enabled = true` is a load error.
    #[serde(default)]
    pub destinations: Vec<BackupDestination>,

    /// Local destination directory. Must be absolute.
    #[serde(default = "BackupConfig::default_local_dir")]
    pub local_dir: PathBuf,

    #[serde(default)]
    pub s3_bucket: Option<String>,
    #[serde(default)]
    pub s3_region: Option<String>,
    #[serde(default)]
    pub s3_access_key: Option<String>,
    #[serde(default)]
    pub s3_secret_key: Option<String>,
    /// Custom endpoint for MinIO / Cloudflare R2 / Backblaze B2.
    #[serde(default)]
    pub s3_endpoint: Option<String>,
    #[serde(default = "BackupConfig::default_s3_prefix")]
    pub s3_prefix: String,

    /// Database dump settings (Day 24). `None` skips mysqldump.
    #[serde(default)]
    pub database: Option<BackupDatabase>,

    /// Filesystem paths to archive (Day 24). `None` archives nothing.
    #[serde(default)]
    pub files: Option<BackupFiles>,
}

impl BackupConfig {
    fn default_schedule() -> String {
        // 6-field cron (sec min hour dom month dow) — the cron crate
        // does not accept 5-field expressions. See docs/backup.md
        // §Schedule. 03:00:00 UTC daily.
        "0 0 3 * * *".into()
    }
    fn default_keep_days() -> u32 {
        7
    }
    fn default_local_dir() -> PathBuf {
        PathBuf::from("/var/backups/vajra")
    }
    fn default_s3_prefix() -> String {
        "vajra/".into()
    }

    /// Resolve S3 credentials env-first, then config.
    /// Empty env values are treated as unset.
    pub fn resolved_s3_creds(&self) -> (Option<String>, Option<String>) {
        let ak = std::env::var("VAJRA_S3_ACCESS_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3_access_key.clone().filter(|s| !s.is_empty()));
        let sk = std::env::var("VAJRA_S3_SECRET_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3_secret_key.clone().filter(|s| !s.is_empty()));
        (ak, sk)
    }

    /// Validate this section in isolation. Errors are operator-facing.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.destinations.is_empty() {
            return Err("backup.enabled = true but destinations is empty".into());
        }
        if let Err(e) = self.schedule.parse::<cron::Schedule>() {
            return Err(format!("backup.schedule {:?}: {e}", self.schedule));
        }
        if !(1..=3650).contains(&self.keep_days) {
            return Err(format!(
                "backup.keep_days must be 1..=3650, got {}",
                self.keep_days
            ));
        }
        if self.destinations.contains(&BackupDestination::Local) && !self.local_dir.is_absolute() {
            return Err(format!(
                "backup.local_dir must be absolute: {:?}",
                self.local_dir
            ));
        }
        if self.destinations.contains(&BackupDestination::S3) {
            if self.s3_bucket.as_deref().unwrap_or("").is_empty() {
                return Err(r#"backup.s3_bucket is required when "s3" is a destination"#.into());
            }
            if self.s3_region.as_deref().unwrap_or("").is_empty() {
                return Err(r#"backup.s3_region is required when "s3" is a destination"#.into());
            }
            let (ak, sk) = self.resolved_s3_creds();
            if ak.is_none() || sk.is_none() {
                eprintln!(
                    "vajra: warning: backup s3 destination set but no credentials in \
                     VAJRA_S3_* env or [backup] config; will fall back to instance role \
                     or ~/.aws/credentials"
                );
            }
        }
        Ok(())
    }
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: Self::default_schedule(),
            keep_days: Self::default_keep_days(),
            destinations: Vec::new(),
            local_dir: Self::default_local_dir(),
            s3_bucket: None,
            s3_region: None,
            s3_access_key: None,
            s3_secret_key: None,
            s3_endpoint: None,
            s3_prefix: Self::default_s3_prefix(),
            database: None,
            files: None,
        }
    }
}

impl Settings {
    /// Validate the `[backup]` section. Delegates to `BackupConfig::validate`.
    pub fn validate_backup(&self) -> Result<(), String> {
        self.backup.validate()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Day 24: db + files subsections of [backup]
// ─────────────────────────────────────────────────────────────────────

/// `[backup.database]` — mysqldump settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupDatabase {
    pub host: String,
    #[serde(default = "BackupDatabase::default_port")]
    pub port: u16,
    pub user: String,
    /// Prefer `VAJRA_DB_PASSWORD` (or `MYSQL_PWD`) env over this field.
    #[serde(default)]
    pub password: Option<String>,
    pub name: String,
}

impl BackupDatabase {
    fn default_port() -> u16 {
        3306
    }

    /// Env-first password, mirroring `resolved_s3_creds`.
    pub fn resolved_password(&self) -> Option<String> {
        std::env::var("VAJRA_DB_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.password.clone().filter(|s| !s.is_empty()))
    }
}

/// `[backup.files]` — paths to include in the archive.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupFiles {
    /// Absolute paths. Each is archived under its basename.
    #[serde(default)]
    pub include: Vec<PathBuf>,
}
