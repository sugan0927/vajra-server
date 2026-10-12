//! `vajra --check` — validate a config without starting the server.
//!
//! Three severity levels:
//!   * **error**   — will prevent startup or cause runtime failures
//!   * **warning** — suspicious, may bite in production
//!   * **info**    — context (e.g. "local_dir does not exist yet")
//!
//! Exit code is 0 unless there is at least one error.
//!
//! The engine is a list of small, environment-inspecting functions. None
//! mutate anything. Each check bumps `Report::checks_run`.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileTypeExt; // is_socket()
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::{BackupDestination, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub scope: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub checks_run: usize,
}

impl Report {
    fn error(&mut self, scope: &'static str, msg: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Error,
            scope,
            message: msg.into(),
        });
    }
    fn warn(&mut self, scope: &'static str, msg: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Warning,
            scope,
            message: msg.into(),
        });
    }
    fn info(&mut self, scope: &'static str, msg: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Info,
            scope,
            message: msg.into(),
        });
    }

    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count()
    }
    pub fn infos(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Info)
            .count()
    }

    /// Render to a writer in the operator-facing format.
    pub fn render(&self, out: &mut impl io::Write) -> io::Result<()> {
        let bucket = |s: Severity| {
            self.findings
                .iter()
                .filter(move |f| f.severity == s)
                .collect::<Vec<_>>()
        };
        let errors = bucket(Severity::Error);
        let warnings = bucket(Severity::Warning);
        let infos = bucket(Severity::Info);

        if !errors.is_empty() {
            writeln!(out, "ERRORS:")?;
            for f in &errors {
                writeln!(out, "  {} {}", f.scope, f.message)?;
            }
            writeln!(out)?;
        }
        if !warnings.is_empty() {
            writeln!(out, "WARNINGS:")?;
            for f in &warnings {
                writeln!(out, "  {} {}", f.scope, f.message)?;
            }
            writeln!(out)?;
        }
        if !infos.is_empty() {
            writeln!(out, "INFO:")?;
            for f in &infos {
                writeln!(out, "  {} {}", f.scope, f.message)?;
            }
            writeln!(out)?;
        }
        writeln!(
            out,
            "OK: {} check(s), {} warning(s), {} error(s)",
            self.checks_run,
            self.warnings(),
            self.errors()
        )
    }
}

/// Run every check. This is I/O-bound but never mutates state.
pub fn run(settings: &Settings) -> Report {
    let mut r = Report::default();
    check_listen_privileges(settings, &mut r);
    check_worker_count(settings, &mut r);
    check_grace(settings, &mut r);
    check_admin_exposure(settings, &mut r);
    check_tls(settings, &mut r);
    check_php_fpm(settings, &mut r);
    check_backup(settings, &mut r);
    r
}

// ─────────────────────────────────────────────────────────────────────
// Individual checks
// ─────────────────────────────────────────────────────────────────────

fn check_listen_privileges(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    let port = s.listen.port();
    if port < 1024 {
        // SAFETY: geteuid() is async-signal-safe.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            r.warn(
                "[server]",
                format!(
                    "listen {} needs root or CAP_NET_BIND_SERVICE (euid={euid})",
                    s.listen
                ),
            );
        }
    }
}

fn check_worker_count(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    let ncpu = crate::sys::available_cpus();
    if s.workers > ncpu.saturating_mul(2) {
        r.warn(
            "[server]",
            format!(
                "workers = {} is more than 2× CPU count ({})",
                s.workers, ncpu
            ),
        );
    }
}

fn check_grace(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    if s.grace_secs < 2 {
        r.warn(
            "[server]",
            format!(
                "grace_secs = {} is very short; open requests may be dropped",
                s.grace_secs
            ),
        );
    }
}

fn check_admin_exposure(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    let Some(a) = &s.admin else { return };
    if a.ip().is_loopback() {
        r.info("[admin]", format!("admin endpoint on {a}"));
    } else {
        r.warn(
            "[admin]",
            format!(
                "admin endpoint {} is not loopback — it has NO authentication",
                a
            ),
        );
    }
}

fn check_tls(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    let Some(t) = &s.tls else { return };

    for (label, path) in [("cert", &t.cert), ("key", &t.key)] {
        match std::fs::metadata(path) {
            Ok(_) => r.info("[tls]", format!("{label} readable: {}", path.display())),
            Err(e) => r.error("[tls]", format!("{label} {}: {e}", path.display())),
        }
    }

    // Expiry — shell out to `openssl x509 -checkend N` so we do not need
    // to link an X.509 parser. Missing openssl is a no-op (info only).
    if t.cert.exists() {
        match cert_days_remaining(&t.cert) {
            Some(d) if d < 7 => r.error("[tls]", format!("cert expires in ~{d} day(s)")),
            Some(d) if d < 30 => r.warn("[tls]", format!("cert expires in ~{d} day(s)")),
            Some(d) => r.info("[tls]", format!("cert valid for ~{d} more day(s)")),
            None => r.info(
                "[tls]",
                "cert expiry not checked (openssl unavailable, or >90 days)",
            ),
        }
    }
}

/// Return `Some(days)` for certs valid for ≤ 90 days; `None` when openssl
/// is unavailable or the cert is valid for more than 90 days.
fn cert_days_remaining(cert: &Path) -> Option<i64> {
    for (secs, days) in [
        (90 * 86400, 90i64),
        (30 * 86400, 30),
        (7 * 86400, 7),
        (86400, 1),
    ] {
        let out = Command::new("openssl")
            .args(["x509", "-checkend", &secs.to_string(), "-noout", "-in"])
            .arg(cert)
            .output()
            .ok()?;
        if !out.status.success() {
            // Expires within `days`.
            return Some(days - 1);
        }
    }
    None
}

fn check_php_fpm(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    if s.proxies.is_empty() {
        r.info("[php]", "no [[proxy]] routes configured");
        return;
    }
    let mut php_routes = 0usize;
    for (i, p) in s.proxies.iter().enumerate() {
        if p.php.is_none() {
            continue;
        }
        php_routes += 1;
        for up in &p.upstreams {
            match probe_upstream(up) {
                UpstreamProbe::Unix(path) => match std::fs::metadata(&path) {
                    Ok(m) if m.file_type().is_socket() => {
                        let connectable = std::os::unix::net::UnixStream::connect(&path).is_ok();
                        if connectable {
                            r.info(
                                "[php]",
                                format!("proxy[{i}] socket {} connectable", path.display()),
                            );
                        } else {
                            r.error(
                                "[php]",
                                format!("proxy[{i}] socket {} refused connect", path.display()),
                            );
                        }
                    }
                    Ok(_) => r.error(
                        "[php]",
                        format!("proxy[{i}] {} is not a socket", path.display()),
                    ),
                    Err(e) => r.error(
                        "[php]",
                        format!("proxy[{i}] socket {}: {e}", path.display()),
                    ),
                },
                UpstreamProbe::Tcp(host, port) => {
                    if tcp_reachable(&host, port, Duration::from_secs(2)) {
                        r.info(
                            "[php]",
                            format!("proxy[{i}] upstream {host}:{port} reachable"),
                        );
                    } else {
                        r.error(
                            "[php]",
                            format!("proxy[{i}] upstream {host}:{port} not reachable"),
                        );
                    }
                }
                UpstreamProbe::Unknown => {
                    r.warn(
                        "[php]",
                        format!("proxy[{i}] unrecognised upstream; skipped"),
                    );
                }
            }
        }
    }
    if php_routes == 0 {
        r.info(
            "[php]",
            format!("{} proxy route(s) configured, none PHP", s.proxies.len()),
        );
    }
}

/// Shape of a resolved upstream. We parse from the `Debug` repr of
/// `UpAddr` so we do not depend on its exact shape (newtype vs enum).
enum UpstreamProbe {
    Unix(PathBuf),
    Tcp(String, u16),
    Unknown,
}

fn probe_upstream(u: &crate::config::UpAddr) -> UpstreamProbe {
    let s = format!("{u:?}");
    parse_upstream_str(&s)
}

fn parse_upstream_str(s: &str) -> UpstreamProbe {
    let s = s.trim_matches('"');

    // Strip variant wrappers like `Unix("/path")` or `Tcp(1.2.3.4:9000)`.
    if let Some(inner) = s.strip_prefix("Unix(").and_then(|x| x.strip_suffix(')')) {
        let path = inner.trim_matches('"');
        return UpstreamProbe::Unix(PathBuf::from(path));
    }
    if let Some(inner) = s.strip_prefix("Tcp(").and_then(|x| x.strip_suffix(')')) {
        let inner = inner.trim_matches('"');
        if let Some((h, p)) = inner.rsplit_once(':') {
            if let Ok(port) = p.parse::<u16>() {
                return UpstreamProbe::Tcp(h.to_string(), port);
            }
        }
        return UpstreamProbe::Unknown;
    }

    // Bare form (newtype-of-String style): "unix:/path" or "host:port".
    if let Some(p) = s.strip_prefix("unix:") {
        return UpstreamProbe::Unix(PathBuf::from(p));
    }
    if let Some((h, p)) = s.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return UpstreamProbe::Tcp(h.to_string(), port);
        }
    }
    UpstreamProbe::Unknown
}

fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let mut addrs = match (host, port).to_socket_addrs() {
        Ok(it) => it,
        Err(_) => return false,
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

fn check_backup(s: &Settings, r: &mut Report) {
    r.checks_run += 1;
    let b = &s.backup;
    if !b.enabled {
        r.info("[backup]", "disabled");
        return;
    }

    // Local destination.
    if b.destinations.contains(&BackupDestination::Local) {
        if b.local_dir.exists() {
            match std::fs::metadata(&b.local_dir) {
                Ok(m) if m.is_dir() => match tempfile::NamedTempFile::new_in(&b.local_dir) {
                    Ok(_) => r.info(
                        "[backup]",
                        format!("local_dir {} writable", b.local_dir.display()),
                    ),
                    Err(e) => r.error(
                        "[backup]",
                        format!("local_dir {} not writable: {e}", b.local_dir.display()),
                    ),
                },
                Ok(_) => r.error(
                    "[backup]",
                    format!("local_dir {} is not a directory", b.local_dir.display()),
                ),
                Err(e) => r.error(
                    "[backup]",
                    format!("local_dir {}: {e}", b.local_dir.display()),
                ),
            }
        } else {
            let parent = b.local_dir.parent().unwrap_or(Path::new("/"));
            if parent.exists() {
                r.info(
                    "[backup]",
                    format!(
                        "local_dir {} will be created (parent {} exists)",
                        b.local_dir.display(),
                        parent.display()
                    ),
                );
            } else {
                r.error(
                    "[backup]",
                    format!("local_dir parent {} does not exist", parent.display()),
                );
            }
        }
    }

    // S3 credentials.
    if b.destinations.contains(&BackupDestination::S3) {
        let (ak, sk) = b.resolved_s3_creds();
        if ak.is_none() || sk.is_none() {
            r.warn(
                "[backup]",
                "s3 destination set but no credentials in env or config; will fall back to instance role",
            );
        }
    }

    // DB reachability.
    if let Some(db) = &b.database {
        if tcp_reachable(&db.host, db.port, Duration::from_secs(2)) {
            r.info("[backup]", format!("DB {}:{} reachable", db.host, db.port));
        } else {
            r.error(
                "[backup]",
                format!("DB {}:{} not reachable", db.host, db.port),
            );
        }
    } else {
        r.info(
            "[backup]",
            "no [backup.database] section — mysqldump will be skipped",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Unit tests (env-independent pieces only)
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unix_variant() {
        match parse_upstream_str(r#"Unix("/run/php/php8.2-fpm.sock")"#) {
            UpstreamProbe::Unix(p) => assert_eq!(p, PathBuf::from("/run/php/php8.2-fpm.sock")),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn parse_tcp_variant() {
        match parse_upstream_str("Tcp(127.0.0.1:9000)") {
            UpstreamProbe::Tcp(h, p) => {
                assert_eq!(h, "127.0.0.1");
                assert_eq!(p, 9000);
            }
            _ => panic!("expected Tcp"),
        }
    }

    #[test]
    fn parse_bare_unix() {
        match parse_upstream_str("unix:/tmp/fpm.sock") {
            UpstreamProbe::Unix(p) => assert_eq!(p, PathBuf::from("/tmp/fpm.sock")),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn parse_bare_tcp() {
        match parse_upstream_str("10.0.0.1:8080") {
            UpstreamProbe::Tcp(h, p) => {
                assert_eq!(h, "10.0.0.1");
                assert_eq!(p, 8080);
            }
            _ => panic!("expected Tcp"),
        }
    }

    #[test]
    fn garbage_is_unknown() {
        assert!(matches!(
            parse_upstream_str("not-an-upstream"),
            UpstreamProbe::Unknown
        ));
    }
}
