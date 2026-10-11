//! Health-check side listener.
//!
//! A blocking `TcpListener` on its own thread answers liveness and
//! readiness probes. It runs outside the io_uring worker pool so probes
//! succeed even when the main listener is saturated.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::config::HealthConfig;

const MAX_REQ: usize = 4096;
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct HealthStats {
    pub liveness_ok: AtomicU64,
    pub readiness_ok: AtomicU64,
    pub readiness_fail: AtomicU64,
    pub not_found: AtomicU64,
    pub bad_method: AtomicU64,
}

pub struct HealthHandle {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    stats: Arc<HealthStats>,
    pub addr: SocketAddr,
}

impl HealthHandle {
    pub fn stats(&self) -> Arc<HealthStats> {
        self.stats.clone()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for HealthHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn(cfg: HealthConfig, php_upstream: Option<String>) -> io::Result<HealthHandle> {
    let addr: SocketAddr = cfg.listen.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("health.listen {:?}: {e}", cfg.listen),
        )
    })?;
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(HealthStats::default());

    let stop_bg = stop.clone();
    let stats_bg = stats.clone();
    let cfg_bg = cfg.clone();
    let php_bg = php_upstream;

    let thread = thread::Builder::new()
        .name("vajra-health".into())
        .spawn(move || accept_loop(listener, stop_bg, stats_bg, cfg_bg, php_bg))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("spawn health: {e}")))?;

    Ok(HealthHandle {
        stop,
        thread: Some(thread),
        stats,
        addr: local,
    })
}

fn accept_loop(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    stats: Arc<HealthStats>,
    cfg: HealthConfig,
    php: Option<String>,
) {
    let mut buf = [0u8; MAX_REQ];
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let _ = handle_conn(stream, &cfg, php.as_deref(), &stats, &mut buf);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn handle_conn(
    mut stream: TcpStream,
    cfg: &HealthConfig,
    php: Option<&str>,
    stats: &HealthStats,
    buf: &mut [u8],
) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_nodelay(true);

    let n = read_request_line(&mut stream, buf)?;
    let (method, target) = parse_request_line(&buf[..n]);

    let (status, body): (u16, &'static str) = match (method, target) {
        ("GET", Some(p)) if p == cfg.path => {
            stats.liveness_ok.fetch_add(1, Ordering::Relaxed);
            (200, "{\"status\":\"ok\"}\n")
        }
        ("HEAD", Some(p)) if p == cfg.path => {
            stats.liveness_ok.fetch_add(1, Ordering::Relaxed);
            (200, "")
        }
        ("GET", Some(p)) if p == cfg.ready_path => match readiness(cfg, php) {
            Ok(()) => {
                stats.readiness_ok.fetch_add(1, Ordering::Relaxed);
                (200, "{\"status\":\"ready\"}\n")
            }
            Err(_) => {
                stats.readiness_fail.fetch_add(1, Ordering::Relaxed);
                (503, "{\"status\":\"not_ready\"}\n")
            }
        },
        ("HEAD", Some(p)) if p == cfg.ready_path => match readiness(cfg, php) {
            Ok(()) => {
                stats.readiness_ok.fetch_add(1, Ordering::Relaxed);
                (200, "")
            }
            Err(_) => {
                stats.readiness_fail.fetch_add(1, Ordering::Relaxed);
                (503, "")
            }
        },
        ("GET", Some(_)) | ("HEAD", Some(_)) => {
            stats.not_found.fetch_add(1, Ordering::Relaxed);
            (404, "{\"error\":\"not_found\"}\n")
        }
        _ => {
            stats.bad_method.fetch_add(1, Ordering::Relaxed);
            (405, "{\"error\":\"method_not_allowed\"}\n")
        }
    };

    write_response(&mut stream, status, method == "HEAD", body)
}

fn read_request_line(stream: &mut TcpStream, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0usize;
    loop {
        if total >= buf.len() {
            return Ok(total);
        }
        match stream.read(&mut buf[total..]) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                total += n;
                if buf[..total].windows(2).any(|w| w == b"\r\n") {
                    return Ok(total);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(total),
            Err(e) => return Err(e),
        }
    }
}

fn parse_request_line(buf: &[u8]) -> (&str, Option<&str>) {
    let end = buf
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(buf.len());
    let line = std::str::from_utf8(&buf[..end]).unwrap_or("");
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("");
    let target = it.next();
    (method, target)
}

fn readiness(cfg: &HealthConfig, php: Option<&str>) -> Result<(), &'static str> {
    if let Some(upstream) = php {
        if let Some(path) = upstream.strip_prefix("unix:") {
            if !path.is_empty() {
                let s = UnixStream::connect(std::path::Path::new(path))
                    .map_err(|_| "php_fpm_unreachable")?;
                drop(s);
            }
        }
    }
    if let Some(addr) = cfg.mariadb.as_deref() {
        let mut any = false;
        match addr.to_socket_addrs() {
            Ok(addrs) => {
                for sa in addrs {
                    any = true;
                    if let Ok(s) = TcpStream::connect_timeout(&sa, PROBE_TIMEOUT) {
                        drop(s);
                        return Ok(());
                    }
                }
            }
            Err(_) => return Err("mariadb_bad_address"),
        }
        if any {
            return Err("mariadb_unreachable");
        }
        return Err("mariadb_bad_address");
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    head_only: bool,
    body: &str,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        len = body.len(),
    );
    stream.write_all(head.as_bytes())?;
    if !head_only && !body.is_empty() {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()?;
    Ok(())
}
