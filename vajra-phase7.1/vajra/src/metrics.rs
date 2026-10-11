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
    bi.with_label_values(&[env!("CARGO_PKG_VERSION"), "release"]).set(1);
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
    let mut buf = Vec::with_capacity(4096);
    let encoder = TextEncoder::new();
    if encoder.encode(&reg.gather(), &mut buf).is_err() {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
}


// ─── HTTP metrics ─────────────────────────────────────────────────────

pub const METHODS: [&str; 10] =
    ["GET","POST","PUT","DELETE","HEAD","OPTIONS","PATCH","CONNECT","TRACE","OTHER"];
pub const CLASSES: [&str; 5] = ["1xx","2xx","3xx","4xx","5xx"];

pub struct HttpMetrics {
    pub requests: Vec<prometheus::IntCounter>,
    pub durations: Vec<prometheus::Histogram>,
    pub active_connections: prometheus::IntGauge,
}

static HTTP: OnceLock<HttpMetrics> = OnceLock::new();

#[inline]
pub fn method_idx(m: &str) -> usize {
    match m {
        "GET" => 0, "POST" => 1, "PUT" => 2, "DELETE" => 3,
        "HEAD" => 4, "OPTIONS" => 5, "PATCH" => 6, "CONNECT" => 7,
        "TRACE" => 8, _ => 9,
    }
}

#[inline]
pub fn class_idx(status: u16) -> usize {
    match status / 100 { 1 => 0, 2 => 1, 3 => 2, 4 => 3, _ => 4 }
}

pub fn init_http(reg: &prometheus::Registry) -> prometheus::Result<()> {
    if HTTP.get().is_some() { return Ok(()); }

    let buckets = vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25,
                       0.5, 1.0, 2.5, 5.0, 10.0];

    let mut requests = Vec::with_capacity(METHODS.len() * CLASSES.len());
    let mut durations = Vec::with_capacity(METHODS.len() * CLASSES.len());

    for m in METHODS {
        for c in CLASSES {
            let labels: std::collections::HashMap<String, String> =
                [("method".into(), m.to_string()),
                 ("status_class".into(), c.to_string())]
                .into_iter().collect();

            let ctr = prometheus::IntCounter::with_opts(
                prometheus::Opts::new("vajra_requests_total", "HTTP requests")
                    .const_labels(labels.clone()))?;
            reg.register(Box::new(ctr.clone()))?;
            requests.push(ctr);

            let hist = prometheus::Histogram::with_opts(
                prometheus::HistogramOpts::new(
                    "vajra_request_duration_seconds",
                    "HTTP request duration in seconds")
                    .buckets(buckets.clone())
                    .const_labels(labels))?;
            reg.register(Box::new(hist.clone()))?;
            durations.push(hist);
        }
    }

    let active_connections = prometheus::IntGauge::new(
        "vajra_active_connections",
        "Current open client connections")?;
    reg.register(Box::new(active_connections.clone()))?;

    HTTP.set(HttpMetrics { requests, durations, active_connections })
        .map_err(|_| prometheus::Error::Msg("http metrics already set".into()))?;
    Ok(())
}

pub fn http() -> Option<&'static HttpMetrics> { HTTP.get() }

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
