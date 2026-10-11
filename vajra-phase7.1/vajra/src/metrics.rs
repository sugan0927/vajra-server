//! Prometheus metrics registry.
//!
//! A single global registry owns every Vajra metric. It is initialized
//! exactly once in `main` via [`init`]. Admin's `/metrics` handler asks
//! [`render`] for the current text exposition.
//!
//! Kept separate from `observe.rs` (which still renders its own
//! snapshot-based text). Until Day 7 both outputs are concatenated so
//! no existing dashboards break.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use prometheus::{Encoder, Gauge, IntGaugeVec, Opts, Registry, TextEncoder};

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();
static UPTIME: OnceLock<Gauge> = OnceLock::new();

/// Initialize the registry. Idempotent: the second call is a no-op.
pub fn init() -> prometheus::Result<()> {
    let _ = START.set(Instant::now());

    if REGISTRY.get().is_some() {
        return Ok(());
    }

    let reg = Registry::new();

    // process_* (cpu, mem, fds, start time) — from /proc/self on Linux.
    let pc = prometheus::process_collector::ProcessCollector::for_self();
    reg.register(Box::new(pc))?;

    // vajra_build_info{version,profile} 1
    let bi = IntGaugeVec::new(
        Opts::new("vajra_build_info", "Vajra build information"),
        &["version", "profile"],
    )?;
    bi.with_label_values(&[env!("CARGO_PKG_VERSION"), "release"])
        .set(1);
    reg.register(Box::new(bi))?;

    // vajra_uptime_seconds — updated on each /metrics scrape.
    let up = Gauge::new("vajra_uptime_seconds", "Seconds since vajra started")?;
    reg.register(Box::new(up.clone()))?;
    let _ = UPTIME.set(up);

    // vajra_start_time_seconds — Unix epoch at process start.
    if let Some(start) = START.get() {
        let elapsed = start.elapsed().as_secs_f64();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let st = Gauge::new("vajra_start_time_seconds", "Unix time at vajra start")?;
        st.set(now - elapsed);
        reg.register(Box::new(st))?;
    }

    init_http(&reg)?;
    init_fpm(&reg)?;
    init_bytes(&reg)?;
    init_in_flight(&reg)?;
    init_reload(&reg)?;
    init_rate_limit(&reg)?;
    init_fastcgi_cache(&reg)?;

    REGISTRY
        .set(reg)
        .map_err(|_| prometheus::Error::Msg("registry already set".into()))?;
    Ok(())
}

/// The live registry, if `init()` has been called.
pub fn registry() -> Option<&'static Registry> {
    REGISTRY.get()
}

/// Render the current registry as Prometheus text format.
/// Returns an empty string if `init()` was never called.
pub fn render() -> String {
    let Some(reg) = REGISTRY.get() else {
        return String::new();
    };
    // Refresh uptime on every scrape.
    if let (Some(u), Some(s)) = (UPTIME.get(), START.get()) {
        u.set(s.elapsed().as_secs_f64());
    }
    flush_bytes();
    flush_in_flight();
    let mut buf = Vec::with_capacity(4096);
    let encoder = TextEncoder::new();
    if encoder.encode(&reg.gather(), &mut buf).is_err() {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
}

// ─── HTTP metrics ─────────────────────────────────────────────────────

pub const METHODS: [&str; 10] = [
    "GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH", "CONNECT", "TRACE", "OTHER",
];
pub const CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];

pub struct HttpMetrics {
    pub requests: Vec<prometheus::IntCounter>,
    pub durations: Vec<prometheus::Histogram>,
    pub active_connections: prometheus::IntGauge,
}

static HTTP: OnceLock<HttpMetrics> = OnceLock::new();

#[inline]
pub fn method_idx(m: &str) -> usize {
    match m {
        "GET" => 0,
        "POST" => 1,
        "PUT" => 2,
        "DELETE" => 3,
        "HEAD" => 4,
        "OPTIONS" => 5,
        "PATCH" => 6,
        "CONNECT" => 7,
        "TRACE" => 8,
        _ => 9,
    }
}

#[inline]
pub fn class_idx(status: u16) -> usize {
    match status / 100 {
        1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        _ => 4,
    }
}

pub fn init_http(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if HTTP.get().is_some() {
        return Ok(());
    }

    let buckets = vec![
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ];

    let mut requests = Vec::with_capacity(METHODS.len() * CLASSES.len());
    let mut durations = Vec::with_capacity(METHODS.len() * CLASSES.len());

    for m in METHODS {
        for c in CLASSES {
            let labels: std::collections::HashMap<String, String> = [
                ("method".into(), m.to_string()),
                ("status_class".into(), c.to_string()),
            ]
            .into_iter()
            .collect();

            let ctr = prometheus::IntCounter::with_opts(
                prometheus::Opts::new("vajra_requests_total", "HTTP requests")
                    .const_labels(labels.clone()),
            )?;
            reg.register(Box::new(ctr.clone()))?;
            requests.push(ctr);

            let hist = prometheus::Histogram::with_opts(
                prometheus::HistogramOpts::new(
                    "vajra_request_duration_seconds",
                    "HTTP request duration in seconds",
                )
                .buckets(buckets.clone())
                .const_labels(labels),
            )?;
            reg.register(Box::new(hist.clone()))?;
            durations.push(hist);
        }
    }

    let active_connections = prometheus::IntGauge::new(
        "vajra_active_connections",
        "Current open client connections",
    )?;
    reg.register(Box::new(active_connections.clone()))?;

    HTTP.set(HttpMetrics {
        requests,
        durations,
        active_connections,
    })
    .map_err(|_| prometheus::Error::Msg("http metrics already set".into()))?;
    Ok(())
}

pub fn http() -> Option<&'static HttpMetrics> {
    HTTP.get()
}

impl HttpMetrics {
    /// Zero-allocation hot path. Called once per response.
    #[inline]
    pub fn observe(&self, method: &str, status: u16, dur: std::time::Duration) {
        let i = method_idx(method) * CLASSES.len() + class_idx(status);
        self.requests[i].inc();
        self.durations[i].observe(dur.as_secs_f64());
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;

    #[test]
    fn method_class_indices() {
        assert_eq!(method_idx("GET"), 0);
        assert_eq!(method_idx("POST"), 1);
        assert_eq!(method_idx("BREW"), 9);
        assert_eq!(class_idx(199), 0);
        assert_eq!(class_idx(200), 1);
        assert_eq!(class_idx(404), 3);
        assert_eq!(class_idx(500), 4);
        assert_eq!(class_idx(599), 4);
    }

    #[test]
    fn observe_updates_counter_and_histogram() {
        init().unwrap();
        let h = http().expect("http metrics initialized");
        let idx = method_idx("GET") * 5 + class_idx(200);
        let before = h.requests[idx].get();
        h.observe("GET", 200, std::time::Duration::from_millis(12));
        let after = h.requests[idx].get();
        assert_eq!(after, before + 1);

        let text = render();
        assert!(text.contains("vajra_requests_total"));
        assert!(text.contains("vajra_request_duration_seconds"));
    }

    #[test]
    fn active_connections_gauge_round_trip() {
        init().unwrap();
        let h = http().unwrap();
        h.active_connections.inc();
        h.active_connections.inc();
        h.active_connections.dec();
        let text = render();
        assert!(text.contains("vajra_active_connections"));
    }
}

// ─── Request-timing helpers (hot path, zero-alloc) ─────────────────────

thread_local! {
    /// Start of the request currently being processed on this worker
    /// thread. Set by `request_begin()`, consumed by `request_end()`.
    /// Worker model is thread-per-core, so there is no cross-thread
    /// contention and no need for atomics.
    static REQUEST_START: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Called once per request, right after the request head is parsed.
#[inline]
pub fn request_begin() {
    REQUEST_START.with(|c| c.set(Some(Instant::now())));
}

/// Called once per response. Consumes the start instant and records
/// the observation. No-op if `request_begin()` was not called.
#[inline]
pub fn request_end(method: &str, status: u16) {
    let start = REQUEST_START.with(|c| c.replace(None));
    let Some(start) = start else { return };
    if let Some(h) = HTTP.get() {
        h.observe(method, status, start.elapsed());
    }
}

/// Clear any pending start time. Used defensively at the top of
/// `process()` so a proxy-path return (which does not observe here)
/// cannot leak its timestamp into a later request.
#[inline]
pub fn request_end_unknown() {
    REQUEST_START.with(|c| c.set(None));
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    #[test]
    fn begin_end_round_trip() {
        init().unwrap();
        let idx = method_idx("GET") * 5 + class_idx(200);
        let before = http().unwrap().requests[idx].get();
        request_begin();
        std::thread::sleep(std::time::Duration::from_millis(1));
        request_end("GET", 200);
        let after = http().unwrap().requests[idx].get();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn end_without_begin_is_noop() {
        init().unwrap();
        request_end_unknown();
        request_end("GET", 200);
    }
}

// ─── PHP-FPM metrics ──────────────────────────────────────────────────

pub const FPM_STATUSES: [&str; 4] = ["ok", "error", "timeout", "parse"];

pub struct FpmMetrics {
    pub requests: Vec<prometheus::IntCounter>,
    pub durations: Vec<prometheus::Histogram>,
}

static FPM: OnceLock<FpmMetrics> = OnceLock::new();

pub fn init_fpm(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if FPM.get().is_some() {
        return Ok(());
    }

    let buckets = vec![
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ];

    let mut requests = Vec::with_capacity(FPM_STATUSES.len());
    let mut durations = Vec::with_capacity(FPM_STATUSES.len());

    for st in FPM_STATUSES {
        let labels: std::collections::HashMap<String, String> =
            [("status".to_string(), st.to_string())]
                .into_iter()
                .collect();

        let ctr = prometheus::IntCounter::with_opts(
            prometheus::Opts::new(
                "vajra_php_fpm_requests_total",
                "PHP-FPM (FastCGI) requests by outcome",
            )
            .const_labels(labels.clone()),
        )?;
        reg.register(Box::new(ctr.clone()))?;
        requests.push(ctr);

        let hist = prometheus::Histogram::with_opts(
            prometheus::HistogramOpts::new(
                "vajra_php_fpm_duration_seconds",
                "PHP-FPM (FastCGI) request duration in seconds",
            )
            .buckets(buckets.clone())
            .const_labels(labels),
        )?;
        reg.register(Box::new(hist.clone()))?;
        durations.push(hist);
    }

    FPM.set(FpmMetrics {
        requests,
        durations,
    })
    .map_err(|_| prometheus::Error::Msg("fpm metrics already set".into()))?;
    Ok(())
}

pub fn fpm() -> Option<&'static FpmMetrics> {
    FPM.get()
}

impl FpmMetrics {
    /// `status_idx` must be an index into `FPM_STATUSES`.
    #[inline]
    pub fn observe(&self, status_idx: usize, dur: std::time::Duration) {
        if let Some(c) = self.requests.get(status_idx) {
            c.inc();
        }
        if let Some(h) = self.durations.get(status_idx) {
            h.observe(dur.as_secs_f64());
        }
    }
}

// ─── Bytes counters ───────────────────────────────────────────────────

static BYTES_SENT: OnceLock<prometheus::IntCounter> = OnceLock::new();
static BYTES_RECEIVED: OnceLock<prometheus::IntCounter> = OnceLock::new();
static BYTES_SENT_FAST: AtomicU64 = AtomicU64::new(0);
static BYTES_RECEIVED_FAST: AtomicU64 = AtomicU64::new(0);

pub fn init_bytes(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if BYTES_SENT.get().is_some() {
        return Ok(());
    }
    let s = prometheus::IntCounter::new(
        "vajra_bytes_sent_total",
        "Bytes written to clients (request+response)",
    )?;
    let r = prometheus::IntCounter::new("vajra_bytes_received_total", "Bytes read from clients")?;
    reg.register(Box::new(s.clone()))?;
    reg.register(Box::new(r.clone()))?;
    let _ = BYTES_SENT.set(s);
    let _ = BYTES_RECEIVED.set(r);
    Ok(())
}

/// Record response bytes. Cheap: single atomic add.
#[inline]
pub fn record_bytes_sent(n: u64) {
    if n == 0 {
        return;
    }
    let prev = BYTES_SENT_FAST.fetch_add(n, Ordering::Relaxed);
    // Flush to the counter every ~1 MiB to avoid atomic-heavy hot path.
    if (prev + n) & 0x000F_FFFF < prev & 0x000F_FFFF {
        flush_bytes();
    }
    let _ = prev;
}

#[inline]
pub fn record_bytes_received(n: u64) {
    if n == 0 {
        return;
    }
    BYTES_RECEIVED_FAST.fetch_add(n, Ordering::Relaxed);
}

/// Push accumulated atomic counters into prometheus IntCounters.
/// Called from the metrics scraper.
pub fn flush_bytes() {
    if let Some(c) = BYTES_SENT.get() {
        let n = BYTES_SENT_FAST.swap(0, Ordering::Relaxed);
        if n > 0 {
            c.inc_by(n);
        }
    }
    if let Some(c) = BYTES_RECEIVED.get() {
        let n = BYTES_RECEIVED_FAST.swap(0, Ordering::Relaxed);
        if n > 0 {
            c.inc_by(n);
        }
    }
}

#[cfg(test)]
mod fpm_tests {
    use super::*;

    #[test]
    fn fpm_observe_round_trip() {
        init().unwrap();
        let m = fpm().expect("fpm metrics");
        let before = m.requests[0].get();
        m.observe(0, std::time::Duration::from_millis(5));
        assert_eq!(m.requests[0].get(), before + 1);
        let text = render();
        assert!(text.contains("vajra_php_fpm_requests_total"));
        assert!(text.contains("vajra_php_fpm_duration_seconds"));
    }

    #[test]
    fn bytes_counters_round_trip() {
        init().unwrap();
        record_bytes_sent(1024);
        record_bytes_received(2048);
        flush_bytes();
        let text = render();
        assert!(text.contains("vajra_bytes_sent_total"));
        assert!(text.contains("vajra_bytes_received_total"));
    }

    #[test]
    fn unknown_status_is_noop() {
        init().unwrap();
        let m = fpm().unwrap();
        m.observe(999, std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init().unwrap();
        init().unwrap();
    }

    #[test]
    fn render_contains_build_info() {
        init().unwrap();
        let s = render();
        assert!(s.contains("vajra_build_info"), "missing build_info: {s}");
        assert!(s.contains(env!("CARGO_PKG_VERSION")), "missing version");
    }

    #[test]
    fn render_contains_process_metrics() {
        init().unwrap();
        let s = render();
        // ProcessCollector provides at least cpu seconds.
        assert!(s.contains("process_cpu_seconds_total"), "missing cpu: {s}");
        assert!(s.contains("process_resident_memory_bytes"), "missing rss");
    }

    #[test]
    fn render_contains_uptime() {
        init().unwrap();
        let s = render();
        assert!(s.contains("vajra_uptime_seconds"), "missing uptime");
    }
}

// ═══════════════════════════════════════════════════════════════════
// Day 7: in-flight gauge + reload counters
// ═══════════════════════════════════════════════════════════════════

// ─── Request-in-flight gauge (atomic hot path) ───────────────────────

static IN_FLIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static IN_FLIGHT_GAUGE: OnceLock<prometheus::IntGauge> = OnceLock::new();

pub fn init_in_flight(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if IN_FLIGHT_GAUGE.get().is_some() {
        return Ok(());
    }
    let g = prometheus::IntGauge::new(
        "vajra_requests_in_flight",
        "HTTP requests currently being processed",
    )?;
    reg.register(Box::new(g.clone()))?;
    let _ = IN_FLIGHT_GAUGE.set(g);
    Ok(())
}

/// Call on request start. Single relaxed atomic increment.
#[inline]
pub fn enter_request() {
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
}

/// Call on every request exit path. Flushes to gauge every 64th decrement.
#[inline]
pub fn leave_request() {
    let prev = IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    if prev & 0x3F == 0 {
        if let Some(g) = IN_FLIGHT_GAUGE.get() {
            g.set(IN_FLIGHT.load(Ordering::Relaxed) as i64);
        }
    }
}

/// Force a flush. Called from `/metrics` scrape so the value is exact.
pub fn flush_in_flight() {
    if let Some(g) = IN_FLIGHT_GAUGE.get() {
        g.set(IN_FLIGHT.load(Ordering::Relaxed) as i64);
    }
}

// ─── Config reload counters ──────────────────────────────────────────

static RELOADS: OnceLock<ReloadMetrics> = OnceLock::new();

pub struct ReloadMetrics {
    pub ok: prometheus::IntCounter,
    pub err: prometheus::IntCounter,
}

pub fn init_reload(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if RELOADS.get().is_some() {
        return Ok(());
    }
    let ok =
        prometheus::IntCounter::new("vajra_config_reloads_total", "Successful config reloads")?;
    let err = prometheus::IntCounter::new(
        "vajra_config_reload_errors_total",
        "Config reload attempts that failed validation",
    )?;
    reg.register(Box::new(ok.clone()))?;
    reg.register(Box::new(err.clone()))?;
    RELOADS
        .set(ReloadMetrics { ok, err })
        .map_err(|_| prometheus::Error::Msg("reload metrics already set".into()))?;
    Ok(())
}

pub fn reload() -> Option<&'static ReloadMetrics> {
    RELOADS.get()
}

#[inline]
pub fn record_reload_ok() {
    if let Some(m) = RELOADS.get() {
        m.ok.inc();
    }
}

#[inline]
pub fn record_reload_err() {
    if let Some(m) = RELOADS.get() {
        m.err.inc();
    }
}

// ─── Rate-limit counters ─────────────────────────────────────────────

static RATE_LIMIT: OnceLock<RateLimitMetrics> = OnceLock::new();

pub struct RateLimitMetrics {
    pub allowed: prometheus::IntCounter,
    pub rejected: prometheus::IntCounter,
    pub cap_reached: prometheus::IntCounter,
    pub tracked_ips: prometheus::IntGauge,
}

pub fn init_rate_limit(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if RATE_LIMIT.get().is_some() {
        return Ok(());
    }
    let allowed = prometheus::IntCounter::new(
        "vajra_rate_limit_allowed_total",
        "Requests allowed by the rate limiter",
    )?;
    let rejected = prometheus::IntCounter::new(
        "vajra_rate_limit_rejected_total",
        "Requests rejected by the rate limiter (includes cap-reached)",
    )?;
    let cap_reached = prometheus::IntCounter::new(
        "vajra_rate_limit_cap_reached_total",
        "Requests rejected because max_tracked_ips was reached",
    )?;
    let tracked_ips = prometheus::IntGauge::new(
        "vajra_rate_limit_tracked_ips",
        "Tracked rate-limit buckets (sampled at cleanup boundary)",
    )?;
    reg.register(Box::new(allowed.clone()))?;
    reg.register(Box::new(rejected.clone()))?;
    reg.register(Box::new(cap_reached.clone()))?;
    reg.register(Box::new(tracked_ips.clone()))?;
    RATE_LIMIT
        .set(RateLimitMetrics {
            allowed,
            rejected,
            cap_reached,
            tracked_ips,
        })
        .map_err(|_| prometheus::Error::Msg("rate-limit metrics already set".into()))?;
    Ok(())
}

#[inline]
pub fn record_rate_limit_allowed() {
    if let Some(m) = RATE_LIMIT.get() {
        m.allowed.inc();
    }
}

#[inline]
pub fn record_rate_limit_rejected() {
    if let Some(m) = RATE_LIMIT.get() {
        m.rejected.inc();
    }
}

#[inline]
pub fn record_rate_limit_cap_reached() {
    if let Some(m) = RATE_LIMIT.get() {
        m.cap_reached.inc();
    }
}

#[inline]
pub fn record_rate_limit_tracked_ips(n: usize) {
    if let Some(m) = RATE_LIMIT.get() {
        m.tracked_ips.set(n as i64);
    }
}

// ─── FastCGI cache counters ──────────────────────────────────────────

/// Index into `FastcgiCacheMetrics::requests`.
pub const FASTCGI_CACHE_RESULTS: [&str; 3] = ["hit", "miss", "bypass"];

pub const FASTCGI_CACHE_HIT: usize = 0;
pub const FASTCGI_CACHE_MISS: usize = 1;
pub const FASTCGI_CACHE_BYPASS: usize = 2;

pub struct FastcgiCacheMetrics {
    /// `vajra_fastcgi_cache_requests_total{result=...}`.
    pub requests: Vec<prometheus::IntCounter>,
    /// `vajra_fastcgi_cache_evictions_total`.
    pub evictions: prometheus::IntCounter,
    /// `vajra_fastcgi_cache_entries`.
    ///
    /// Vajra workers are share-nothing, so this gauge reports the last
    /// cache size observed by any worker. A `sum across workers` would
    /// require per-worker slots; that is deferred. See
    /// `docs/fastcgi-cache.md` for the precise semantics.
    pub entries: prometheus::IntGauge,
    /// `vajra_fastcgi_cache_bytes`. Same caveat as `entries`.
    pub bytes: prometheus::IntGauge,
}

static FASTCGI_CACHE: OnceLock<FastcgiCacheMetrics> = OnceLock::new();

pub fn init_fastcgi_cache(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if FASTCGI_CACHE.get().is_some() {
        return Ok(());
    }

    let mut requests = Vec::with_capacity(FASTCGI_CACHE_RESULTS.len());
    for result in FASTCGI_CACHE_RESULTS {
        let labels: std::collections::HashMap<String, String> =
            [("result".to_string(), result.to_string())]
                .into_iter()
                .collect();
        let c = prometheus::IntCounter::with_opts(
            prometheus::Opts::new(
                "vajra_fastcgi_cache_requests_total",
                "FastCGI response cache lookups by outcome",
            )
            .const_labels(labels),
        )?;
        reg.register(Box::new(c.clone()))?;
        requests.push(c);
    }

    let evictions = prometheus::IntCounter::new(
        "vajra_fastcgi_cache_evictions_total",
        "FastCGI cache entries removed from the index (LRU, expiry, overwrite)",
    )?;
    reg.register(Box::new(evictions.clone()))?;

    let entries = prometheus::IntGauge::new(
        "vajra_fastcgi_cache_entries",
        "FastCGI cache entries (single-worker view; see docs)",
    )?;
    reg.register(Box::new(entries.clone()))?;

    let bytes = prometheus::IntGauge::new(
        "vajra_fastcgi_cache_bytes",
        "FastCGI cache bytes on disk (single-worker view; see docs)",
    )?;
    reg.register(Box::new(bytes.clone()))?;

    FASTCGI_CACHE
        .set(FastcgiCacheMetrics {
            requests,
            evictions,
            entries,
            bytes,
        })
        .map_err(|_| prometheus::Error::Msg("fastcgi cache metrics already set".into()))?;
    Ok(())
}

pub fn fastcgi_cache() -> Option<&'static FastcgiCacheMetrics> {
    FASTCGI_CACHE.get()
}

#[inline]
pub fn record_fastcgi_cache_hit() {
    if let Some(m) = FASTCGI_CACHE.get() {
        if let Some(c) = m.requests.get(FASTCGI_CACHE_HIT) {
            c.inc();
        }
    }
}

#[inline]
pub fn record_fastcgi_cache_miss() {
    if let Some(m) = FASTCGI_CACHE.get() {
        if let Some(c) = m.requests.get(FASTCGI_CACHE_MISS) {
            c.inc();
        }
    }
}

#[inline]
pub fn record_fastcgi_cache_bypass() {
    if let Some(m) = FASTCGI_CACHE.get() {
        if let Some(c) = m.requests.get(FASTCGI_CACHE_BYPASS) {
            c.inc();
        }
    }
}

#[inline]
pub fn record_fastcgi_cache_eviction() {
    if let Some(m) = FASTCGI_CACHE.get() {
        m.evictions.inc();
    }
}

/// Push the current cache shape into the `entries` / `bytes` gauges.
/// Called after every mutation; cheap (two atomic stores).
#[inline]
pub fn record_fastcgi_cache_stats(entries: u64, bytes: u64) {
    if let Some(m) = FASTCGI_CACHE.get() {
        m.entries.set(entries as i64);
        m.bytes.set(bytes as i64);
    }
}

#[cfg(test)]
mod fastcgi_cache_metrics_tests {
    use super::*;

    #[test]
    fn fastcgi_cache_counters_and_gauges() {
        init().unwrap();
        record_fastcgi_cache_hit();
        record_fastcgi_cache_hit();
        record_fastcgi_cache_miss();
        record_fastcgi_cache_bypass();
        record_fastcgi_cache_eviction();
        record_fastcgi_cache_stats(17, 4096);

        let text = render();
        assert!(
            text.contains("vajra_fastcgi_cache_requests_total"),
            "{text}"
        );
        assert!(text.contains("result=\"hit\""), "{text}");
        assert!(text.contains("result=\"miss\""), "{text}");
        assert!(text.contains("result=\"bypass\""), "{text}");
        assert!(
            text.contains("vajra_fastcgi_cache_evictions_total"),
            "{text}"
        );
        assert!(text.contains("vajra_fastcgi_cache_entries"), "{text}");
        assert!(text.contains("vajra_fastcgi_cache_bytes"), "{text}");
    }
}

#[cfg(test)]
mod rate_limit_metrics_tests {
    use super::*;

    #[test]
    fn rate_limit_counters_and_gauge() {
        init().unwrap();
        record_rate_limit_allowed();
        record_rate_limit_rejected();
        record_rate_limit_cap_reached();
        record_rate_limit_tracked_ips(7);
        let text = render();
        assert!(text.contains("vajra_rate_limit_allowed_total"), "{text}");
        assert!(text.contains("vajra_rate_limit_rejected_total"), "{text}");
        assert!(
            text.contains("vajra_rate_limit_cap_reached_total"),
            "{text}"
        );
        assert!(text.contains("vajra_rate_limit_tracked_ips"), "{text}");
    }
}
