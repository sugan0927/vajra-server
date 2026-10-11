//! Integration tests for the health listener.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use vajra::config::HealthConfig;
use vajra::health;

fn cfg(mariadb: Option<String>) -> HealthConfig {
    HealthConfig {
        listen: "127.0.0.1:0".into(),
        path: "/health".into(),
        ready_path: "/ready".into(),
        enabled: true,
        mariadb,
    }
}

fn get(addr: SocketAddr, path: &str, method: &str) -> (u16, String) {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text.split_whitespace().nth(1)
        .and_then(|x| x.parse::<u16>().ok()).unwrap_or(0);
    (status, text)
}

#[test]
fn liveness_always_ok() {
    let mut h = health::spawn(cfg(None), None).unwrap();
    let (code, body) = get(h.addr, "/health", "GET");
    assert_eq!(code, 200, "body: {body}");
    assert!(body.contains("\"status\":\"ok\""));
    h.stop();
}

#[test]
fn readiness_ok_without_deps() {
    let mut h = health::spawn(cfg(None), None).unwrap();
    let (code, body) = get(h.addr, "/ready", "GET");
    assert_eq!(code, 200, "body: {body}");
    assert!(body.contains("\"status\":\"ready\""));
    h.stop();
}

#[test]
fn readiness_fails_when_mariadb_down() {
    let mut h = health::spawn(cfg(Some("127.0.0.1:1".into())), None).unwrap();
    let (code, _body) = get(h.addr, "/ready", "GET");
    assert_eq!(code, 503);
    h.stop();
}

#[test]
fn unknown_path_is_404() {
    let mut h = health::spawn(cfg(None), None).unwrap();
    let (code, _) = get(h.addr, "/nope", "GET");
    assert_eq!(code, 404);
    h.stop();
}

#[test]
fn post_is_405() {
    let mut h = health::spawn(cfg(None), None).unwrap();
    let (code, _) = get(h.addr, "/health", "POST");
    assert_eq!(code, 405);
    h.stop();
}

#[test]
fn head_has_content_length_no_body() {
    let mut h = health::spawn(cfg(None), None).unwrap();
    let (_code, text) = get(h.addr, "/health", "HEAD");
    let expected_len = "{\"status\":\"ok\"}\n".len();
    assert!(text.contains(&format!("Content-Length: {expected_len}")), "headers: {text}");
    let after_headers = text.split("\r\n\r\n").nth(1).unwrap_or("");
    assert!(after_headers.is_empty(), "unexpected body: {after_headers:?}");
    h.stop();
}
