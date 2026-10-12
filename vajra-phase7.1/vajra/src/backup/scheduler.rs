//! Background backup scheduler.
//!
//! Spawned by `main` when `[backup].enabled = true`. Wakes every `tick`
//! seconds, checks whether the cron schedule has fired since the last
//! check, and runs the local (+ optional S3) backup once per firing.
//!
//! ## Design
//!
//! * This is a **std::thread**, not a worker core. It touches no
//!   io_uring state and never blocks a worker.
//! * Only one backup runs at a time. If a run overruns the next tick
//!   (unlikely — backups take seconds, ticks are minutes), the missed
//!   tick is skipped, not queued. This is deliberate: stacking
//!   concurrent mysqldumps on a busy VPS is worse than a late backup.
//! * Shutdown is signalled via a shared `AtomicBool`. The loop observes
//!   it within one tick. An in-flight backup completes; only the
//!   *next* firing is suppressed.
//!
//! ## Env vars
//!
//! * `VAJRA_BACKUP_TICK_SECS` — override the wake-up interval
//!   (default 5). Tests use `1`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{BackupConfig, BackupDestination};
use crate::metrics;

/// Default wake-up interval. Fine-grained enough that a `@daily` firing
/// is never missed; coarse enough to be free.
const DEFAULT_TICK_SECS: u64 = 5;

fn tick() -> Duration {
    let secs = std::env::var("VAJRA_BACKUP_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TICK_SECS);
    Duration::from_secs(secs)
}

/// Handle to the scheduler thread. `shutdown()` is idempotent and also
/// runs on drop.
pub struct Scheduler {
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Scheduler {
    /// Signal shutdown and join. Safe to call multiple times.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the scheduler. When `cfg.enabled == false` returns a no-op
/// handle whose `shutdown()` does nothing.
pub fn spawn(cfg: BackupConfig) -> Scheduler {
    let shutdown = Arc::new(AtomicBool::new(false));
    if !cfg.enabled {
        return Scheduler {
            shutdown,
            thread: None,
        };
    }
    let shutdown_thread = shutdown.clone();
    let thread = thread::Builder::new()
        .name("vajra-backup".into())
        .spawn(move || run_loop(cfg, shutdown_thread))
        .expect("spawn backup scheduler");
    Scheduler {
        shutdown,
        thread: Some(thread),
    }
}

// ─────────────────────────────────────────────────────────────────────

fn run_loop(cfg: BackupConfig, shutdown: Arc<AtomicBool>) {
    let schedule: cron::Schedule = match cfg.schedule.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "vajra backup: invalid schedule {:?}: {e} — scheduler disabled",
                cfg.schedule
            );
            return;
        }
    };

    eprintln!(
        "vajra backup: scheduler started (schedule={:?}, next={})",
        cfg.schedule,
        fmt_next(&schedule, SystemTime::now()),
    );

    let mut last_check = SystemTime::now();
    while !shutdown.load(Ordering::Relaxed) {
        thread::sleep(tick());
        let now = SystemTime::now();
        if fires_between(&schedule, last_check, now) {
            run_once(&cfg);
        }
        last_check = now;
    }
    eprintln!("vajra backup: scheduler stopped");
}

/// True iff at least one cron firing falls in `(prev, now]`.
fn fires_between(s: &cron::Schedule, prev: SystemTime, now: SystemTime) -> bool {
    let prev_dt = to_utc(prev);
    for t in s.after(&prev_dt) {
        let t_sys: SystemTime = from_utc(t);
        if t_sys > now {
            return false;
        }
        if t_sys > prev {
            return true;
        }
    }
    false
}

fn run_once(cfg: &BackupConfig) {
    let _ = BackupDestination::Local; // keep import meaningful

    let local_result = super::local::run(cfg);
    match &local_result {
        Ok(r) => {
            let p = r
                .local_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".into());
            eprintln!(
                "vajra backup: local ok: {} ({} bytes, {} pruned)",
                p, r.archive_bytes, r.pruned_local,
            );
            metrics::record_backup_run("local", true, r.archive_bytes);
        }
        Err(e) => {
            eprintln!("vajra backup: local failed: {e}");
            metrics::record_backup_run("local", false, 0);
        }
    }

    if !cfg.destinations.contains(&BackupDestination::S3) {
        return;
    }
    match super::s3::run(cfg) {
        Ok(r) => {
            let key = r.s3_key.as_deref().unwrap_or("-");
            eprintln!(
                "vajra backup: s3 ok: key={} ({} bytes)",
                key, r.archive_bytes,
            );
            metrics::record_backup_run("s3", true, r.archive_bytes);
        }
        Err(e) => {
            eprintln!("vajra backup: s3 failed: {e}");
            metrics::record_backup_run("s3", false, 0);
        }
    }
}

fn fmt_next(s: &cron::Schedule, after: SystemTime) -> String {
    match s.after(&to_utc(after)).next() {
        Some(dt) => dt.to_rfc3339(),
        None => "never".into(),
    }
}

fn to_utc(t: SystemTime) -> chrono::DateTime<chrono::Utc> {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, d.subsec_nanos())
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
}

fn from_utc(dt: chrono::DateTime<chrono::Utc>) -> SystemTime {
    let secs = dt.timestamp();
    if secs < 0 {
        return UNIX_EPOCH;
    }
    UNIX_EPOCH + Duration::new(secs as u64, dt.timestamp_subsec_nanos())
}

// ─────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_between_detects_a_daily_firing() {
        // 2023-11-15 02:59:00 UTC = 1700017140.
        let prev = UNIX_EPOCH + Duration::from_secs(1_700_017_140);
        let now = prev + Duration::from_secs(90); // 03:00:30
        let s: cron::Schedule = "0 0 3 * * *".parse().unwrap();
        assert!(fires_between(&s, prev, now));
    }

    #[test]
    fn fires_between_misses_when_window_before_firing() {
        // 2023-11-15 02:56:40 to 02:57:40 — no 03:00 firing inside.
        let prev = UNIX_EPOCH + Duration::from_secs(1_700_017_000);
        let now = prev + Duration::from_secs(60);
        let s: cron::Schedule = "0 0 3 * * *".parse().unwrap();
        assert!(!fires_between(&s, prev, now));
    }

    #[test]
    fn to_from_utc_round_trip() {
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        let back = from_utc(to_utc(t));
        assert_eq!(t, back);
    }

    #[test]
    fn disabled_scheduler_is_a_noop() {
        // enabled = false → shutdown() must return immediately, no panic.
        let cfg = BackupConfig::default();
        let mut s = spawn(cfg);
        s.shutdown();
    }
}
