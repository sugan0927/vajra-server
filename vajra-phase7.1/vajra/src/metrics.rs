//! Prometheus metrics registry.
//!
//! A single global registry owns every Vajra metric. It is initialized
//! exactly once in `main` via [`init`]. Admin's `/metrics` handler asks
//! [`render`] for the current text exposition.
//!
//! Kept separate from `observe.rs` (which still renders its own
//! snapshot-based text). Until Day 7 both outputs are concatenated so
//! no existing dashboards break.

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
