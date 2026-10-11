//! End-to-end wire tests for security response headers.
//!
//! Unlike `tests/security_headers.rs` (pure function + config parsing),
//! these spin up a real `Worker` over plain TCP and check that headers
//! actually appear in the HTTP/1.1 wire response — and that managed
//! headers from an upstream are replaced, not duplicated.
//!
//! Each test skips (passes) when the environment forbids io_uring.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use vajra::admin::Manager;
use vajra::config::{Dynamic, HeaderOverride, HeaderSettings, ProxySettings, Settings};
use vajra::control;
use vajra::sys;
use vajra::worker::{Config, Listener, Worker};

// ───────────────────────── server harness ─────────────────────────

struct Server {
    addr: SocketAddr,
    mgr: Arc<Manager>,
    done: mpsc::Receiver<std::io::Result<()>>,
}

fn start(dynamic: Dynamic) -> Option<Server> {
    let sock = sys::listener("127.0.0.1:0".parse().unwrap(), false, 128).unwrap();
    let addr = sock.local_addr().unwrap().as_socket().unwrap();
    let (handle, inbox) = control::channel().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let l = [Listener {
            fd: sock.as_raw_fd(),
            tls: false,
        }];
        match Worker::new(&l, Config::default(), &dynamic, None) {
            Ok(mut w) => {
                w.attach_control(0, inbox, Duration::from_secs(3));
                ready_tx.send(true).unwrap();
                let r = w.run();
                drop(w);
                drop(sock);
                let _ = done_tx.send(r);
            }
            Err(e) => {
                eprintln!("io_uring unavailable ({e}); skipping");
                ready_tx.send(false).unwrap();
            }
        }
    });

    if !ready_rx.recv().unwrap() {
        return None;
    }
    let mgr = Arc::new(Manager::new(vec![handle], None, Settings::default()));
    Some(Server {
        addr,
        mgr,
        done: done_rx,
    })
}

// ───────────────────────── helpers ────────────────────────────────

fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf
}

/// Parse headers from an HTTP/1.1 response into (lowercase_name, value).
fn parse_headers(resp: &str) -> Vec<(String, String)> {
    let head = resp.split("\r\n\r\n").next().unwrap_or("");
    head.lines()
        .skip(1) // status line
        .filter_map(|l| {
            let (n, v) = l.split_once(':')?;
            Some((n.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect()
}

fn find<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn count(h: &[(String, String)], name: &str) -> usize {
    h.iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .count()
}

fn all_headers_set() -> HeaderSettings {
    HeaderSettings {
        hsts: Some("max-age=31536000; includeSubDomains".into()),
        x_frame_options: Some("DENY".into()),
        x_content_type_options: Some("nosniff".into()),
        referrer_policy: Some("no-referrer".into()),
        permissions_policy: Some("geolocation=()".into()),
        overrides: vec![],
    }
}

fn empty_override() -> HeaderOverride {
    HeaderOverride {
        path: String::new(),
        hsts: None,
        x_frame_options: None,
        x_content_type_options: None,
        referrer_policy: None,
        permissions_policy: None,
    }
}

// ───────────────────────── upstream that "manages" headers ─────────

/// Spawn a mock upstream that returns its own `X-Frame-Options` and
/// `Strict-Transport-Security`; a misbehaving upstream that has been
/// given no business managing them. Config must override both.
fn spawn_upstream_with_managed_headers() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { break };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let body = b"upstream-body";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/plain\r\n\
                     Content-Length: {}\r\n\
                     X-Frame-Options: SAMEORIGIN\r\n\
                     Strict-Transport-Security: max-age=1\r\n\
                     X-Upstream-Only: keep-me\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(body);
            });
        }
    });
    addr
}

// ───────────────────────── tests ──────────────────────────────────

#[test]
fn h1_top_level_headers_land_on_the_wire() {
    let dynamic = Dynamic {
        headers: all_headers_set(),
        ..Dynamic::default()
    };
    let Some(s) = start(dynamic) else { return };

    let r = http_get(s.addr, "/any/path");
    let h = parse_headers(&r);

    assert_eq!(
        find(&h, "strict-transport-security"),
        Some("max-age=31536000; includeSubDomains"),
        "{r}"
    );
    assert_eq!(find(&h, "x-frame-options"), Some("DENY"), "{r}");
    assert_eq!(find(&h, "x-content-type-options"), Some("nosniff"), "{r}");
    assert_eq!(find(&h, "referrer-policy"), Some("no-referrer"), "{r}");
    assert_eq!(
        find(&h, "permissions-policy"),
        Some("geolocation=()"),
        "{r}"
    );

    s.mgr.shutdown();
    let _ = s.done.recv_timeout(Duration::from_secs(5));
}

#[test]
fn h1_override_and_remove_take_effect_on_the_wire() {
    let dynamic = Dynamic {
        headers: HeaderSettings {
            hsts: Some("max-age=100".into()),
            x_frame_options: Some("DENY".into()),
            overrides: vec![
                HeaderOverride {
                    path: "/api/".into(),
                    hsts: Some("max-age=60".into()),
                    ..empty_override()
                },
                HeaderOverride {
                    path: "/embed/".into(),
                    x_frame_options: Some(String::new()), // remove
                    ..empty_override()
                },
            ],
            ..HeaderSettings::default()
        },
        ..Dynamic::default()
    };
    let Some(s) = start(dynamic) else { return };

    // Default path → base values
    let h = parse_headers(&http_get(s.addr, "/elsewhere"));
    assert_eq!(find(&h, "strict-transport-security"), Some("max-age=100"));
    assert_eq!(find(&h, "x-frame-options"), Some("DENY"));

    // /api/xyz → HSTS overridden, others inherited
    let h = parse_headers(&http_get(s.addr, "/api/xyz"));
    assert_eq!(find(&h, "strict-transport-security"), Some("max-age=60"));
    assert_eq!(find(&h, "x-frame-options"), Some("DENY"));

    // /embed/page → X-Frame-Options removed, others inherited
    let h = parse_headers(&http_get(s.addr, "/embed/page"));
    assert_eq!(find(&h, "x-frame-options"), None, "removed header leaked");
    assert_eq!(find(&h, "strict-transport-security"), Some("max-age=100"));

    s.mgr.shutdown();
    let _ = s.done.recv_timeout(Duration::from_secs(5));
}

#[test]
fn h1_managed_header_replaces_upstream_value() {
    let upstream = spawn_upstream_with_managed_headers();
    let dynamic = Dynamic {
        headers: HeaderSettings {
            hsts: Some("max-age=999999".into()),
            x_frame_options: Some("DENY".into()),
            ..HeaderSettings::default()
        },
        proxies: vec![ProxySettings::simple("/api/", upstream, false, 5)],
        ..Dynamic::default()
    };
    let Some(s) = start(dynamic) else { return };

    let r = http_get(s.addr, "/api/anything");
    let h = parse_headers(&r);

    // Exactly one instance each, and it's the config's value.
    assert_eq!(count(&h, "x-frame-options"), 1, "{r}");
    assert_eq!(find(&h, "x-frame-options"), Some("DENY"), "{r}");
    assert_eq!(count(&h, "strict-transport-security"), 1, "{r}");
    assert_eq!(
        find(&h, "strict-transport-security"),
        Some("max-age=999999"),
        "{r}"
    );

    // Unmanaged upstream headers still flow through.
    assert_eq!(find(&h, "x-upstream-only"), Some("keep-me"), "{r}");

    s.mgr.shutdown();
    let _ = s.done.recv_timeout(Duration::from_secs(5));
}
