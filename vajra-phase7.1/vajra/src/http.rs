//! HTTP/1.1 request handling plus the response renderers shared with HTTP/2.
//!
//! No socket I/O happens here: `process` turns bytes in into response bytes
//! out, plus an optional [`Action`] the worker must carry out (stream a file,
//! run a proxy request, or wait for more body bytes).
//!
//! Request bodies are only accepted on proxy routes (buffered, bounded by
//! `Ctx::max_body`, `Content-Length` framing only). Everywhere else a request
//! that declares a body is answered and then the connection is closed, so the
//! byte stream can never desynchronise.
//!
//! Phase 4: every response produced here is reported to the worker's
//! [`Observer`] (metrics + access log), and proxied `GET`s are answered from the
//! per-core response cache when a fresh entry exists (`X-Cache: HIT`).

use crate::cache::{self, Cache};
use crate::date::DATE_LEN;
use crate::h2::{H2Body, H2Response};
use crate::observe::{Http, Observer};
use crate::proxy::{self, is_hop_by_hop, ProxySpec, ProxyTable, ReqParts};
use crate::router::{self, Reply};
use crate::static_files::{FileCache, OpenFile};
use std::cell::RefCell;
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Max request headers accepted per request (stack-allocated array).
pub const MAX_HEADERS: usize = 64;
/// An incomplete request head larger than this gets a 431.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Per-call context borrowed from the worker.
pub struct Ctx<'a> {
    pub files: Option<&'a mut FileCache>,
    /// Phase 1a: multi-site table. When set, the per-request FileCache
    /// is selected by the request's Host header.
    pub sites: Option<&'a mut crate::site::SiteTable>,
    pub proxies: &'a ProxyTable,
    pub cache: &'a mut Cache,
    pub obs: &'a mut Observer,
    /// Unix seconds (cache freshness).
    pub now: u64,
    pub max_body: usize,
    pub secure: bool,
    pub client_ip: Option<IpAddr>,
    /// Security-header policy from the running config.
    pub sec: Option<&'a crate::config::HeaderSettings>,
    /// Access-control policy from the running config.
    /// `None` disables the check (used by tests).
    pub access: Option<&'a crate::access::AccessSettings>,
    pub rate_limit: Option<&'a mut crate::rate_limit::RateLimiter>,
    /// Compression policy from the running config. `None` disables.
    pub compression: Option<&'a crate::compress::CompressionSettings>,

    /// FastCGI response cache, if enabled on this worker.
    /// `None` on requests that never reach a FastCGI target.
    pub fastcgi_cache: Option<&'a mut crate::cache::fastcgi::FastcgiCache>,
}

/// Case-insensitive lookup of a header value as `&str`.
/// Returns `None` if the header is absent or non-UTF-8.
fn lookup_header<'a>(headers: &'a [httparse::Header<'_>], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .and_then(|h| std::str::from_utf8(h.value).ok())
}

thread_local! {
    /// `Alt-Svc` value advertised on HTTP/1.1 and HTTP/2 responses (empty = none).
    /// Thread-local: each worker sets its own copy, nothing is shared.
    static ALT_SVC: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// ─── Security-header plumbing (thread-local, set once per request) ──

thread_local! {
    /// Resolved `Name: value` pairs for the current request.
    static SEC_HDR: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// Header names this request must drop from upstream/cached responses
    /// (config either replaces or removes them for this path).
    static SEC_DROP: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Compute and cache the security-header actions for `path`.
/// Called once per request as soon as the target is parsed.
pub(crate) fn prepare_sec(cfg: Option<&crate::config::HeaderSettings>, path: &str) {
    SEC_HDR.with(|h| h.borrow_mut().clear());
    SEC_DROP.with(|d| d.borrow_mut().clear());
    let Some(cfg) = cfg else { return };
    let eff = cfg.effective_at(path);
    SEC_HDR.with(|h| {
        SEC_DROP.with(|d| {
            let mut h = h.borrow_mut();
            let mut d = d.borrow_mut();
            for (i, name) in crate::config::SECURITY_HEADERS.iter().enumerate() {
                match eff[i] {
                    crate::config::Action::Skip => {}
                    crate::config::Action::Remove => d.push(name.to_string()),
                    crate::config::Action::Set(v) => {
                        d.push(name.to_string());
                        h.push((name.to_string(), v.to_string()));
                    }
                }
            }
        })
    });
}

/// Append the resolved security headers as complete `Name: value\r\n` lines.
#[inline]
fn push_sec_into(out: &mut Vec<u8>) {
    SEC_HDR.with(|h| {
        for (n, v) in h.borrow().iter() {
            out.extend_from_slice(n.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
    });
}

/// Push the resolved security headers into an `(name, value)` Vec used
/// by H2/H3 response builders. Called once per response.
#[inline]
pub(crate) fn push_sec_h2(v: &mut Vec<(Vec<u8>, Vec<u8>)>) {
    SEC_HDR.with(|h| {
        for (n, val) in h.borrow().iter() {
            v.push((n.as_bytes().to_vec(), val.as_bytes().to_vec()));
        }
    });
}

/// `true` if the config manages `name` (sets or removes it) at the current path.
#[inline]
pub(crate) fn sec_manages(name: &[u8]) -> bool {
    SEC_DROP.with(|d| {
        d.borrow()
            .iter()
            .any(|n| n.as_bytes().eq_ignore_ascii_case(name))
    })
}

/// Advertise HTTP/3 on this thread's responses, e.g. `h3=":443"; ma=86400`.
pub fn set_alt_svc(value: Option<&str>) {
    ALT_SVC.with(|a| *a.borrow_mut() = value.map(|v| v.as_bytes().to_vec()).unwrap_or_default());
}
/// Global HTTP→HTTPS redirect flag (set once at startup).
static REDIRECT_HTTP_TO_HTTPS: AtomicBool = AtomicBool::new(false);

pub fn set_redirect_http_to_https(enabled: bool) {
    REDIRECT_HTTP_TO_HTTPS.store(enabled, Ordering::Relaxed);
}


/// `Alt-Svc: ..\r\n` for an HTTP/1.1 head, or nothing.
fn push_alt_svc_h1(out: &mut Vec<u8>) {
    ALT_SVC.with(|a| {
        let a = a.borrow();
        if !a.is_empty() {
            out.extend_from_slice(b"Alt-Svc: ");
            out.extend_from_slice(&a);
            out.extend_from_slice(b"\r\n");
        }
    });
}

fn push_alt_svc_h2(out: &mut Vec<(Vec<u8>, Vec<u8>)>) {
    ALT_SVC.with(|a| {
        let a = a.borrow();
        if !a.is_empty() {
            out.push((b"alt-svc".to_vec(), a.clone()));
        }
    });
}

#[allow(clippy::large_enum_variant)]
pub enum Action {
    None,
    /// Stream this file as the body of the response whose head is in `out`.
    File(Rc<OpenFile>),
    /// Run this upstream request; the response is written when it completes.
    Proxy(ProxySpec),
    /// A proxy request's head is complete but its body is not: call again with more bytes.
    NeedBody,
}

pub struct Outcome {
    /// Bytes of `input` fully consumed.
    pub consumed: usize,
    /// Close the connection after everything is flushed.
    pub close: bool,
    pub action: Action,
}

fn done(consumed: usize, close: bool) -> Outcome {
    Outcome {
        consumed,
        close,
        action: Action::None,
    }
}

/// Error response + metrics/log entry (the request line may be unparsed).
pub fn fail(
    out: &mut Vec<u8>,
    obs: &mut Observer,
    ip: Option<IpAddr>,
    status: u16,
    date: &[u8; DATE_LEN],
) {
    obs.response(
        ip,
        Http::H1,
        "-",
        "-",
        status,
        error_body(status).len() as u64,
    );
    crate::metrics::request_end("-", status);
    write_error(out, status, date);
}

/// Parse as many complete (possibly pipelined) requests as `input` holds,
/// appending responses to `out`. Stops after a request that needs an action.
pub fn fail_429(
    out: &mut Vec<u8>,
    obs: &mut Observer,
    client_ip: Option<IpAddr>,
    retry_after_secs: u32,
    date: &[u8; DATE_LEN],
) {
    let start = out.len();
    fail(out, obs, client_ip, 429, date);
    if let Some(pos) = out[start..].windows(4).position(|w| w == b"\r\n\r\n") {
        let insert_at = start + pos;
        let mut buf: Vec<u8> = Vec::with_capacity(32);
        buf.extend_from_slice(b"\r\nRetry-After: ");
        push_u64(&mut buf, retry_after_secs as u64);
        out.splice(insert_at..insert_at, buf);
    }
}

pub fn process(input: &[u8], out: &mut Vec<u8>, date: &[u8; DATE_LEN], ctx: &mut Ctx) -> Outcome {
    let out_start = out.len();
    let result = process_inner(input, out, date, ctx);
    crate::metrics::record_bytes_sent((out.len() - out_start) as u64);
    crate::metrics::record_bytes_received(result.consumed as u64);
    result
}

fn process_inner(input: &[u8], out: &mut Vec<u8>, date: &[u8; DATE_LEN], ctx: &mut Ctx) -> Outcome {
    let mut consumed = 0;
    loop {
        crate::metrics::request_begin();
        if consumed == input.len() {
            return done(consumed, false);
        }

        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut req = httparse::Request::new(&mut headers);

        match req.parse(&input[consumed..]) {
            Ok(httparse::Status::Complete(n)) => {
                crate::req_id::set_opt(
                    crate::req_id::extract_from_h1(req.headers)
                        .or_else(|| Some(crate::req_id::RequestId::generate())),
                );
                let method = req.method.unwrap_or("");
                let target = req.path.unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/");
                prepare_sec(ctx.sec, path);

                let mut keep_alive = req.version == Some(1);
                let mut content_length: Option<usize> = None;
                let mut bad_length = false;
                let mut chunked = false;
                let mut if_none_match: Option<&[u8]> = None;
                let mut host: Option<&[u8]> = None;
                let mut accept_encoding: Option<&str> = None;

                for h in req.headers.iter() {
                    if h.name.eq_ignore_ascii_case("connection") {
                        let has = |t: &str| {
                            h.value
                                .split(|&b| b == b',')
                                .any(|x| x.trim_ascii().eq_ignore_ascii_case(t.as_bytes()))
                        };
                        if has("close") {
                            keep_alive = false;
                        } else if has("keep-alive") {
                            keep_alive = true;
                        }
                    } else if h.name.eq_ignore_ascii_case("content-length") {
                        let v = std::str::from_utf8(h.value)
                            .ok()
                            .and_then(|s| s.trim().parse::<usize>().ok());
                        match v {
                            Some(v) if content_length.map_or(true, |p| p == v) => {
                                content_length = Some(v)
                            }
                            _ => bad_length = true,
                        }
                    } else if h.name.eq_ignore_ascii_case("transfer-encoding") {
                        chunked = true;
                    } else if h.name.eq_ignore_ascii_case("if-none-match") {
                        if_none_match = Some(h.value);
                    } else if h.name.eq_ignore_ascii_case("host") {
                        host = Some(h.value);
                    } else if h.name.eq_ignore_ascii_case("accept-encoding") {
                        accept_encoding = std::str::from_utf8(h.value).ok();
                    }
                }

                if bad_length {
                    fail(out, ctx.obs, ctx.client_ip, 400, date);
                    return done(input.len(), true);
                }
                // ─── HTTP → HTTPS redirect ──────────────────────────
                if !ctx.secure
                    && REDIRECT_HTTP_TO_HTTPS.load(Ordering::Relaxed)
                    && !path.starts_with("/.well-known/acme-challenge/")
                {
                    if let Some(h) = host {
                        if let Ok(host_str) = std::str::from_utf8(h) {
                            let host_only = host_str.split(':').next().unwrap_or(host_str);
                            if !host_only.is_empty() {
                                let mut loc: Vec<u8> = Vec::with_capacity(96);
                                loc.extend_from_slice(b"Location: https://");
                                loc.extend_from_slice(host_only.as_bytes());
                                loc.extend_from_slice(target.as_bytes());
                                loc.extend_from_slice(b"\r\n");
                                ctx.obs.response(ctx.client_ip, Http::H1, method, target, 301, 0);
                                crate::metrics::request_end(target, 301);
                                write_head(out, 301, &loc, router::TEXT, 0, false, date);
                                return done(input.len(), true);
                            }
                        }
                    }
                }


                // ─── Effective client IP ────────────────────────────
                // Post trusted-proxy / XFF resolution. Both the access
                // check and the rate limiter use this same value so an
                // IP denied by one is the same IP seen by the other.
                //
                // Absent `ctx.client_ip` (tests), both checks are
                // skipped entirely.
                let xff = lookup_header(req.headers, "x-forwarded-for");
                let xri = lookup_header(req.headers, "x-real-ip");
                let real_client_ip = ctx.client_ip.map(|peer| {
                    if let Some(access) = ctx.access {
                        access.effective_client_ip(peer, xff, xri)
                    } else {
                        if let Some(xff_ip) = xff
                            .and_then(|v| std::str::from_utf8(v.as_ref()).ok())
                            .and_then(|s| s.split(',').next())
                            .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
                        {
                            xff_ip
                        } else if let Some(xri_ip) = xri
                            .and_then(|v| std::str::from_utf8(v.as_ref()).ok())
                            .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
                        {
                            xri_ip
                        } else {
                            peer
                        }
                    }
                });

                if let Some(peer) = ctx.client_ip {
                    let client = real_client_ip.unwrap_or(peer);

                    // ─── IP access control ────────────────────────
                    if let Some(access) = ctx.access {
                        if !access.is_empty()
                            && access.evaluate(client) == crate::access::AccessDecision::Deny
                        {
                            eprintln!(
                                "vajra: access denied client={client} peer={peer} path={path}"
                            );
                            fail(out, ctx.obs, ctx.client_ip, 403, date);
                            return done(input.len(), true);
                        }
                    }

                    // ─── Rate limiting ────────────────────────────
                    let rl_decision = match ctx.rate_limit.as_deref_mut() {
                        Some(rl) if !rl.is_disabled() => {
                            Some(rl.check_with_path(client, std::time::Instant::now(), path))
                        }
                        _ => None,
                    };
                    if let Some(crate::rate_limit::Decision::Deny { retry_after_secs }) =
                        rl_decision
                    {
                        eprintln!(
                            "vajra: rate-limit denied client={client} peer={peer} path={path} retry_after={retry_after_secs}"
                        );
                        fail_429(out, ctx.obs, ctx.client_ip, retry_after_secs, date);
                        return done(input.len(), true);
                    }
                }

                let head_only = method == "HEAD";
                // Phase 1a: per-site FileCache selection by Host header.
                // Phase 1b: also extract the site's PHP route index +
                // PhpConfig so `router::route` dispatches FastCGI to the
                // correct pool. Previously this was hardcoded `None`,
                // which made `route()` fall through to
                // `proxies.php_route()` -> the first synthetic proxy
                // (wp's) -> every host served WP's content.
                let mut site_php: Option<(usize, &crate::php::PhpConfig)> = None;
                let selected_files: Option<&mut FileCache> = match ctx.sites.as_deref_mut() {
                    Some(st) => {
                        let h = host.and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("");
                        match st.lookup_mut(h) {
                            Some(entry) => {
                                let crate::site::SiteEntry {
                                    php,
                                    php_route_idx,
                                    files,
                                    ..
                                } = entry;
                                if let (Some(cfg), Some(idx)) = (php.as_ref(), *php_route_idx) {
                                    site_php = Some((idx, cfg));
                                }
                                Some(files)
                            }
                            None => ctx.files.as_deref_mut(),
                        }
                    }
                    None => ctx.files.as_deref_mut(),
                };
                let reply = router::route(
                    method,
                    path,
                    if_none_match,
                    selected_files,
                    ctx.proxies,
                    site_php,
                );

                if let Some((ri, php_target)) = reply.proxy_parts() {
                    if chunked {
                        fail(out, ctx.obs, ctx.client_ip, 411, date);
                        return done(input.len(), true);
                    }
                    let cl = content_length.unwrap_or(0);
                    let hdrs_all: Vec<(&[u8], &[u8])> = req
                        .headers
                        .iter()
                        .map(|h| (h.name.as_bytes(), h.value))
                        .collect();
                    // A WebSocket handshake is a body-less GET; anything else is plain HTTP.
                    let upgrade = method == "GET"
                        && cl == 0
                        && php_target.is_none()
                        && proxy::is_websocket_upgrade(&hdrs_all);
                    if cl > ctx.max_body {
                        fail(out, ctx.obs, ctx.client_ip, 413, date);
                        return done(input.len(), true);
                    }
                    if input.len() - consumed < n + cl {
                        // Head complete, body still arriving: consume nothing.
                        return Outcome {
                            consumed,
                            close: false,
                            action: Action::NeedBody,
                        };
                    }
                    let body = &input[consumed + n..consumed + n + cl];
                    let hdrs = hdrs_all;

                    let cache_on = !upgrade && ctx.proxies.route(ri).cache.is_some();
                    let mode = match cache::lookup_for_request(
                        ctx.cache,
                        cache_on,
                        method,
                        host.unwrap_or(b""),
                        target,
                        &hdrs,
                        ctx.now,
                    ) {
                        cache::Lookup::Hit(hit) => {
                            let accept_enc_hit: Option<&str> = hdrs
                                .iter()
                                .find(|(n, _)| n.eq_ignore_ascii_case(b"accept-encoding"))
                                .and_then(|(_, v)| std::str::from_utf8(v.as_ref()).ok());
                            let comp_default = crate::compress::CompressionSettings::default();
                            let comp_hit = ctx.compression.unwrap_or(&comp_default);
                            write_proxy_response(
                                out,
                                hit.status,
                                &hit.headers,
                                &hit.body,
                                false,
                                keep_alive,
                                date,
                                Some("HIT"),
                                None,
                                accept_enc_hit,
                                comp_hit,
                            );
                            let bytes = if hit.status == 204 || hit.status == 304 {
                                0
                            } else {
                                hit.body.len() as u64
                            };
                            ctx.obs.response(
                                ctx.client_ip,
                                Http::H1,
                                method,
                                target,
                                hit.status,
                                bytes,
                            );
                            crate::metrics::request_end(method, hit.status);
                            consumed += n + cl;
                            if !keep_alive {
                                return done(consumed, true);
                            }
                            continue;
                        }
                        cache::Lookup::Miss(m) => m,
                    };

                    let mut spec = proxy::make_spec(
                        ctx.proxies,
                        ri,
                        &ReqParts {
                            method,
                            target,
                            host,
                            headers: &hdrs,
                            body,
                            client_ip: real_client_ip,
                            secure: ctx.secure,
                            upgrade,
                            php: php_target,
                        },
                    );
                    spec.cache = mode;

                    // FastCGI cache: gate + arm the write path. On an
                    // eligible request we compute the key and hand it to
                    // `proxy_done`, which stores the response body if it's
                    // cacheable. HIT replay (with headers) lands later;
                    // today every FastCGI request is treated as a MISS.
                    if php_target.is_some() {
                        if let Some(fc) = ctx.fastcgi_cache.as_deref_mut() {
                            let m_ok = method == "GET" || method == "HEAD";
                            let path_only = target.split('?').next().unwrap_or("/");
                            let host_lc = host
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .map(|h| h.to_ascii_lowercase());
                            let host_str = host_lc.as_deref().unwrap_or("");
                            let cookie = hdrs.iter().find_map(|&(n, v)| {
                                if n.eq_ignore_ascii_case(b"cookie") {
                                    std::str::from_utf8(v.as_ref()).ok()
                                } else {
                                    None
                                }
                            });
                            let excluded = crate::cache::fastcgi::any_glob_matches(
                                &fc.settings.exclude_paths,
                                path_only,
                            );
                            let bypassed = crate::cache::fastcgi::cookie_name_matches(
                                cookie,
                                &fc.settings.bypass_cookie,
                            );
                            if m_ok && !excluded && !bypassed {
                                let query = target.split_once('?').map(|(_, q)| q);
                                let key = crate::cache::fastcgi::build_cache_key(
                                    method, host_str, path_only, query,
                                );
                                fc.poll_purge();
                                let accept_enc_hit: Option<&str> = hdrs
                                    .iter()
                                    .find(|(n, _)| n.eq_ignore_ascii_case(b"accept-encoding"))
                                    .and_then(|(_, v)| std::str::from_utf8(v.as_ref()).ok());

                                match fc.get(&key, accept_enc_hit) {
                                    Ok(Some((status, headers, cached))) => {
                                        crate::metrics::record_fastcgi_cache_hit();
                                        let comp_default =
                                            crate::compress::CompressionSettings::default();
                                        let comp_hit = ctx.compression.unwrap_or(&comp_default);
                                        let is_precompressed = headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(b"content-encoding"));
                                        let mut precomp_comp = crate::compress::CompressionSettings::default();
                                        precomp_comp.gzip = false;
                                        precomp_comp.brotli = false;
                                        let comp_to_pass = if is_precompressed { &precomp_comp } else { comp_hit };

                                        write_proxy_response(
                                            out,
                                            status,
                                            &headers,
                                            &cached,
                                            false,
                                            keep_alive,
                                            date,
                                            None,
                                            Some("HIT"),
                                            accept_enc_hit,
                                            comp_to_pass,
                                        );
                                        let bytes = if status == 204 || status == 304 {
                                            0
                                        } else {
                                            cached.len() as u64
                                        };
                                        ctx.obs.response(
                                            ctx.client_ip,
                                            Http::H1,
                                            method,
                                            target,
                                            status,
                                            bytes,
                                        );
                                        crate::metrics::request_end(method, status);
                                        consumed += n + cl;
                                        if !keep_alive {
                                            return done(consumed, true);
                                        }
                                        continue;
                                    }
                                    _ => {
                                        crate::metrics::record_fastcgi_cache_miss();
                                        spec.fcgi_cache_path = Some(path_only.as_bytes().to_vec());
                                        spec.fcgi_cache_key = Some(key);
                                    }
                                }
                            } else {
                                crate::metrics::record_fastcgi_cache_bypass();
                            }
                        }
                    }

                    return Outcome {
                        consumed: consumed + n + cl,
                        // An upgrade never keeps HTTP semantics: the tunnel owns the socket.
                        close: !keep_alive && !upgrade,
                        action: Action::Proxy(spec),
                    };
                }

                consumed += n;
                if chunked || content_length.is_some_and(|v| v > 0) {
                    keep_alive = false; // body is not consumed on these routes
                }
                if matches!(reply, Reply::Mem { status: 405, .. } | Reply::BadRequest) {
                    keep_alive = false;
                }

                let bad = matches!(reply, Reply::BadRequest);
                let (status, bytes) = reply_meta(&reply, head_only);
                ctx.obs
                    .response(ctx.client_ip, Http::H1, method, target, status, bytes);
                crate::metrics::request_end(method, status);
                let file = write_reply(
                    out,
                    &reply,
                    head_only,
                    keep_alive,
                    date,
                    ctx.compression,
                    accept_encoding,
                );
                if bad {
                    return done(input.len(), true);
                }
                if let Some(f) = file {
                    return Outcome {
                        consumed,
                        close: !keep_alive,
                        action: Action::File(f),
                    };
                }
                if !keep_alive {
                    return done(consumed, true);
                }
            }
            Ok(httparse::Status::Partial) => return done(consumed, false),
            Err(httparse::Error::TooManyHeaders) => {
                fail(out, ctx.obs, ctx.client_ip, 431, date);
                return done(input.len(), true);
            }
            Err(_) => {
                fail(out, ctx.obs, ctx.client_ip, 400, date);
                return done(input.len(), true);
            }
        }
    }
}

/// Status and declared body size of a router reply, for logs and metrics.
pub fn reply_meta(reply: &Reply, head_only: bool) -> (u16, u64) {
    match reply {
        Reply::Mem { status, body, .. } => (*status, if head_only { 0 } else { body.len() as u64 }),
        Reply::Acme(body) => (200, if head_only { 0 } else { body.len() as u64 }),
        Reply::File { file, not_modified } => {
            if *not_modified {
                (304, 0)
            } else {
                (200, if head_only { 0 } else { file.size })
            }
        }
        Reply::BadRequest => (400, error_body(400).len() as u64),
        Reply::Proxy(_) | Reply::Php(..) => (0, 0),
    }
}

/// For a Mem reply, decide whether to compress and return the bytes to
/// send alongside any extra headers the caller must append. Adds
/// `Vary: Accept-Encoding` on every compressible type so shared caches
/// key correctly, whether or not this instance compressed.
fn maybe_compress<'a>(
    body: &'a [u8],
    ctype: &str,
    head_only: bool,
    comp: Option<&crate::compress::CompressionSettings>,
    accept: Option<&str>,
) -> (std::borrow::Cow<'a, [u8]>, &'static str) {
    use crate::compress::{brotli_compress, gzip_compress, is_pre_compressed, negotiate, Encoding};
    use std::borrow::Cow;

    let Some(comp) = comp else {
        return (Cow::Borrowed(body), "");
    };
    if !(comp.brotli || comp.gzip) || is_pre_compressed(ctype) {
        return (Cow::Borrowed(body), "");
    }
    let vary = "Vary: Accept-Encoding\r\n";
    if head_only || body.len() < comp.min_size {
        return (Cow::Borrowed(body), vary);
    }
    let enc = negotiate(accept, comp);
    let compressed = match enc {
        Encoding::Brotli => brotli_compress(body, comp.level),
        Encoding::Gzip => gzip_compress(body, comp.gzip_level),
        Encoding::Identity => None,
    };
    match (enc, compressed) {
        (Encoding::Brotli, Some(c)) if c.len() < body.len() => (
            Cow::Owned(c),
            "Vary: Accept-Encoding\r\nContent-Encoding: br\r\n",
        ),
        (Encoding::Gzip, Some(c)) if c.len() < body.len() => (
            Cow::Owned(c),
            "Vary: Accept-Encoding\r\nContent-Encoding: gzip\r\n",
        ),
        _ => (Cow::Borrowed(body), vary),
    }
}

// ───────────────────────── HTTP/1.1 renderers ─────────────────────────

/// Render a router reply. Returns the file to stream when there is a body.
fn write_reply(
    out: &mut Vec<u8>,
    reply: &Reply,
    head_only: bool,
    keep_alive: bool,
    date: &[u8; DATE_LEN],
    compress: Option<&crate::compress::CompressionSettings>,
    accept_encoding: Option<&str>,
) -> Option<Rc<OpenFile>> {
    match reply {
        Reply::Mem {
            status,
            ctype,
            body,
            allow,
        } => {
            let (out_body, comp_hdr) =
                maybe_compress(body, ctype, head_only, compress, accept_encoding);
            let mut hdr_buf: Vec<u8> = Vec::with_capacity(96);
            if *allow {
                hdr_buf.extend_from_slice(b"Allow: GET, HEAD\r\n");
            }
            hdr_buf.extend_from_slice(comp_hdr.as_bytes());
            write_head(
                out,
                *status,
                &hdr_buf,
                ctype,
                out_body.len() as u64,
                keep_alive,
                date,
            );
            if !head_only {
                out.extend_from_slice(&out_body);
            }
            None
        }
        Reply::File { file, not_modified } => {
            if *not_modified {
                write_head(
                    out,
                    304,
                    &file.head_extra,
                    file.ctype,
                    file.size,
                    keep_alive,
                    date,
                );
                None
            } else {
                write_head(
                    out,
                    200,
                    &file.head_extra,
                    file.ctype,
                    file.size,
                    keep_alive,
                    date,
                );
                (!head_only && file.size > 0).then(|| Rc::clone(file))
            }
        }
        Reply::Acme(body) => {
            // ACME HTTP-01 tokens are small, non-user content, and served
            // as text/plain. No compression, no extra headers: what the
            // filesystem has is exactly what the ACME server must see.
            write_head(
                out,
                200,
                b"",
                router::TEXT,
                body.len() as u64,
                keep_alive,
                date,
            );
            if !head_only {
                out.extend_from_slice(body);
            }
            None
        }
        Reply::BadRequest => {
            write_error(out, 400, date);
            None
        }
        Reply::Proxy(_) | Reply::Php(..) => unreachable!("proxy replies are handled by the caller"),
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

pub fn error_body(status: u16) -> &'static [u8] {
    match status {
        400 => b"bad request\n",
        411 => b"length required\n",
        413 => b"payload too large\n",
        431 => b"request header fields too large\n",
        502 => b"bad gateway\n",
        504 => b"gateway timeout\n",
        _ => b"error\n",
    }
}

/// Minimal error response that always closes the connection.
pub fn write_error(out: &mut Vec<u8>, status: u16, date: &[u8; DATE_LEN]) {
    let body = error_body(status);
    write_head(
        out,
        status,
        b"",
        router::TEXT,
        body.len() as u64,
        false,
        date,
    );
    out.extend_from_slice(body);
}

/// Status line + headers + blank line. Hand-rolled (no `write!`) to keep the
/// hot path free of the formatting machinery. `extra` is zero or more
/// complete `Name: value\r\n` lines.
fn write_head(
    out: &mut Vec<u8>,
    status: u16,
    extra: &[u8],
    content_type: &str,
    content_length: u64,
    keep_alive: bool,
    date: &[u8; DATE_LEN],
) {
    out.extend_from_slice(b"HTTP/1.1 ");
    push_u64(out, status as u64);
    out.push(b' ');
    out.extend_from_slice(reason(status).as_bytes());
    out.extend_from_slice(b"\r\nServer: Vajra\r\nDate: ");
    out.extend_from_slice(date);
    out.extend_from_slice(b"\r\n");
    if let Some(id) = crate::req_id::current() {
        out.extend_from_slice(b"X-Request-ID: ");
        out.extend_from_slice(id.as_str().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    push_alt_svc_h1(out);
    push_sec_into(out);
    out.extend_from_slice(b"Content-Type: ");
    out.extend_from_slice(content_type.as_bytes());
    out.extend_from_slice(b"\r\nContent-Length: ");
    push_u64(out, content_length);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(extra);
    out.extend_from_slice(if keep_alive {
        b"Connection: keep-alive\r\n\r\n"
    } else {
        b"Connection: close\r\n\r\n"
    });
}

/// Render a buffered upstream (or cached) response for an HTTP/1.1 client.
/// `xcache` adds an `X-Cache` header (`HIT` / `MISS`).
#[allow(clippy::too_many_arguments)]
pub fn write_proxy_response(
    out: &mut Vec<u8>,
    status: u16,
    headers: &[(Vec<u8>, Vec<u8>)],
    body: &[u8],
    head_req: bool,
    keep_alive: bool,
    date: &[u8; DATE_LEN],
    xcache: Option<&str>,
    fcgi_cache: Option<&str>,
    accept_encoding: Option<&str>,
    comp: &crate::compress::CompressionSettings,
) {
    let mut body = body;
    // Initialised empty to satisfy borrow-checker; overwritten on compress.
    #[allow(unused_assignments)]
    let mut compressed_data = Vec::new();
    let mut is_compressed = false;
    let mut enc_str: &[u8] = b"";

    let mut should_vary = false;
    if status == 200 && !head_req {
        let ct = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(b"content-type"))
            .map(|(_, v)| String::from_utf8_lossy(v).to_string())
            .unwrap_or_default();
        if (comp.brotli || comp.gzip) && !ct.is_empty() && !crate::compress::is_pre_compressed(&ct)
        {
            should_vary = true;
        }
        if crate::compress::decide(comp, accept_encoding, &ct, body.len(), head_req)
            == crate::compress::Plan::Compress
        {
            let enc = crate::compress::negotiate(accept_encoding, comp);
            if enc == crate::compress::Encoding::Brotli {
                if let Some(c) = crate::compress::brotli_compress(body, comp.level) {
                    compressed_data = c;
                    body = &compressed_data;
                    is_compressed = true;
                    enc_str = b"br";
                }
            } else if enc == crate::compress::Encoding::Gzip {
                if let Some(c) = crate::compress::gzip_compress(body, comp.gzip_level) {
                    compressed_data = c;
                    body = &compressed_data;
                    is_compressed = true;
                    enc_str = b"gzip";
                }
            }
        }
    }

    let bodyless = status == 204 || status == 304;
    out.extend_from_slice(b"HTTP/1.1 ");
    push_u64(out, status as u64);
    out.push(b' ');
    out.extend_from_slice(reason(status).as_bytes());
    out.extend_from_slice(b"\r\nServer: Vajra\r\nDate: ");
    out.extend_from_slice(date);
    out.extend_from_slice(b"\r\n");
    if let Some(id) = crate::req_id::current() {
        out.extend_from_slice(b"X-Request-ID: ");
        out.extend_from_slice(id.as_str().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    push_alt_svc_h1(out);
    push_sec_into(out);
    for (n, v) in headers {
        if is_hop_by_hop(n)
            || sec_manages(n)
            || n.eq_ignore_ascii_case(b"date")
            || n.eq_ignore_ascii_case(b"server")
            || n.eq_ignore_ascii_case(b"x-cache")
            || (n.eq_ignore_ascii_case(b"content-length") && !head_req)
            || (is_compressed && n.eq_ignore_ascii_case(b"content-encoding"))
            || (should_vary && n.eq_ignore_ascii_case(b"vary"))
        {
            continue;
        }
        out.extend_from_slice(n);
        out.extend_from_slice(b": ");
        out.extend_from_slice(v);
        out.extend_from_slice(b"\r\n");
    }
    if is_compressed {
        out.extend_from_slice(b"Content-Encoding: ");
        out.extend_from_slice(enc_str);
        out.extend_from_slice(b"\r\n");
    }
    if should_vary {
        out.extend_from_slice(b"Vary: Accept-Encoding\r\n");
    }
    if let Some(x) = xcache {
        out.extend_from_slice(b"X-Cache: ");
        out.extend_from_slice(x.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if let Some(f) = fcgi_cache {
        out.extend_from_slice(b"X-FastCGI-Cache: ");
        out.extend_from_slice(f.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !head_req && !bodyless {
        out.extend_from_slice(b"Content-Length: ");
        push_u64(out, body.len() as u64);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(if keep_alive {
        b"Connection: keep-alive\r\n\r\n"
    } else {
        b"Connection: close\r\n\r\n"
    });
    if !head_req && !bodyless {
        out.extend_from_slice(body);
    }
}

#[inline]
fn push_u64(out: &mut Vec<u8>, mut n: u64) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&tmp[i..]);
}

// ───────────────────────── HTTP/2 renderers ─────────────────────────

fn h2_base(date: &[u8; DATE_LEN], ctype: &str, len: u64) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut v = vec![
        (b"server".to_vec(), b"Vajra".to_vec()),
        (b"date".to_vec(), date.to_vec()),
        (b"content-type".to_vec(), ctype.as_bytes().to_vec()),
        (b"content-length".to_vec(), len.to_string().into_bytes()),
    ];
    if let Some(id) = crate::req_id::current() {
        v.push((b"x-request-id".to_vec(), id.as_str().as_bytes().to_vec()));
    }
    push_alt_svc_h2(&mut v);
    push_sec_h2(&mut v);
    v
}

pub fn h2_response(
    reply: &Reply,
    head_only: bool,
    date: &[u8; DATE_LEN],
    accept_encoding: Option<&str>,
    comp: &crate::compress::CompressionSettings,
) -> H2Response {
    match reply {
        Reply::Mem {
            status,
            ctype,
            body,
            allow,
        } => {
            let mut out_body = body.to_vec();
            let mut is_compressed = false;

            if !head_only && !out_body.is_empty() {
                if crate::compress::decide(comp, accept_encoding, ctype, out_body.len(), false)
                    == crate::compress::Plan::Compress
                {
                    let enc = crate::compress::negotiate(accept_encoding, comp);
                    let compressed = match enc {
                        crate::compress::Encoding::Brotli => crate::compress::brotli_compress(&out_body, comp.level),
                        crate::compress::Encoding::Gzip => crate::compress::gzip_compress(&out_body, comp.gzip_level),
                        crate::compress::Encoding::Identity => None,
                    };
                    if let Some(c) = compressed {
                        if c.len() < out_body.len() {
                            out_body = c;
                            is_compressed = true;
                        }
                    }
                }
            }

            let mut headers = h2_base(date, ctype, out_body.len() as u64);
            if *allow {
                headers.push((b"allow".to_vec(), b"GET, HEAD".to_vec()));
            }

            if !head_only && !body.is_empty() {
                if is_compressed {
                    let enc_str = if crate::compress::negotiate(accept_encoding, comp) == crate::compress::Encoding::Brotli { "br" } else { "gzip" };
                    headers.push((b"content-encoding".to_vec(), enc_str.as_bytes().to_vec()));
                }
                if (comp.brotli || comp.gzip) && !crate::compress::is_pre_compressed(ctype) {
                    headers.push((b"vary".to_vec(), b"Accept-Encoding".to_vec()));
                }
            }

            H2Response {
                status: *status,
                headers,
                body: if head_only {
                    H2Body::Empty
                } else {
                    H2Body::Mem(out_body)
                },
            }
        }
        Reply::File { file, not_modified } => {
            let mut headers = h2_base(date, file.ctype, file.size);
            headers.push((b"etag".to_vec(), file.etag.as_bytes().to_vec()));
            headers.push((b"last-modified".to_vec(), file.last_modified.to_vec()));
            if *not_modified {
                H2Response {
                    status: 304,
                    headers,
                    body: H2Body::Empty,
                }
            } else {
                let body = if head_only || file.size == 0 {
                    H2Body::Empty
                } else {
                    H2Body::File(Rc::clone(file))
                };
                H2Response {
                    status: 200,
                    headers,
                    body,
                }
            }
        }
        Reply::Acme(body) => H2Response {
            status: 200,
            headers: h2_base(date, router::TEXT, body.len() as u64),
            body: if head_only {
                H2Body::Empty
            } else {
                H2Body::Mem(body.clone())
            },
        },
        Reply::BadRequest | Reply::Proxy(_) | Reply::Php(..) => h2_error(400, date),
    }
}

pub fn h2_error(status: u16, date: &[u8; DATE_LEN]) -> H2Response {
    let body = error_body(status);
    H2Response {
        status,
        headers: h2_base(date, router::TEXT, body.len() as u64),
        body: H2Body::Mem(body.to_vec()),
    }
}

pub fn h2_proxy_response(
    status: u16,
    headers: &[(Vec<u8>, Vec<u8>)],
    mut body: Vec<u8>,
    head_req: bool,
    date: &[u8; DATE_LEN],
    xcache: Option<&'static str>,
    fcgi_cache: Option<&'static str>,
    accept_encoding: Option<&str>,
    comp: &crate::compress::CompressionSettings,
) -> H2Response {
    let bodyless = status == 204 || status == 304;
    let mut out = vec![
        (b"server".to_vec(), b"Vajra".to_vec()),
        (b"date".to_vec(), date.to_vec()),
    ];
    push_alt_svc_h2(&mut out);
    push_sec_h2(&mut out);
    for (n, v) in headers {
        if is_hop_by_hop(n)
            || sec_manages(n)
            || n.eq_ignore_ascii_case(b"date")
            || n.eq_ignore_ascii_case(b"server")
            || n.eq_ignore_ascii_case(b"x-cache")
            || (n.eq_ignore_ascii_case(b"content-length") && !head_req)
        {
            continue;
        }
        out.push((n.to_ascii_lowercase(), v.clone()));
    }
    if let Some(x) = xcache {
        out.push((b"x-cache".to_vec(), x.as_bytes().to_vec()));
    }
    if let Some(f) = fcgi_cache {
        out.push((b"x-fastcgi-cache".to_vec(), f.as_bytes().to_vec()));
    }
    if !head_req && !bodyless {
            let ctype = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(b"content-type"))
                .map(|(_, v)| String::from_utf8_lossy(v).to_string())
                .unwrap_or_default();

            if status == 200 && !body.is_empty() {
                if crate::compress::decide(comp, accept_encoding, &ctype, body.len(), false)
                    == crate::compress::Plan::Compress
                {
                    let enc = crate::compress::negotiate(accept_encoding, comp);
                    let compressed = match enc {
                        crate::compress::Encoding::Brotli => crate::compress::brotli_compress(&body, comp.level),
                        crate::compress::Encoding::Gzip => crate::compress::gzip_compress(&body, comp.gzip_level),
                        crate::compress::Encoding::Identity => None,
                    };
                    if let Some(c) = compressed {
                        if c.len() < body.len() {
                            body = c;
                            let enc_str = if enc == crate::compress::Encoding::Brotli { "br" } else { "gzip" };
                            out.push((b"content-encoding".to_vec(), enc_str.as_bytes().to_vec()));
                        }
                    }
                }
                if (comp.brotli || comp.gzip) && !ctype.is_empty() && !crate::compress::is_pre_compressed(&ctype) {
                    out.push((b"vary".to_vec(), b"Accept-Encoding".to_vec()));
                }
            }

            out.push((
                b"content-length".to_vec(),
                body.len().to_string().into_bytes(),
            ));
        }
    let body = if head_req || bodyless || body.is_empty() {
        H2Body::Empty
    } else {
        H2Body::Mem(body)
    };
    H2Response {
        status,
        headers: out,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheMode;
    use crate::cache::CachedResponse;
    use crate::config::{CacheSettings, ProxySettings};

    const DATE: &[u8; DATE_LEN] = b"Thu, 01 Jan 1970 00:00:00 GMT";

    fn table(cached: bool) -> ProxyTable {
        let mut p = ProxySettings::simple("/api/", "127.0.0.1:9000".parse().unwrap(), false, 5);
        p.cache = cached;
        p.cache_default_ttl_secs = 60;
        ProxyTable::new(&[p], 1 << 20)
    }

    struct Env {
        cache: Cache,
        obs: Observer,
    }

    fn env() -> Env {
        Env {
            cache: Cache::new(&CacheSettings::default()),
            obs: Observer::new(),
        }
    }

    fn run_in(input: &str, t: &ProxyTable, e: &mut Env) -> (Outcome, String) {
        let mut out = Vec::new();
        let mut ctx = Ctx {
            sites: None, // FIX4: added by Phase 1a patch
            files: None,
            proxies: t,
            cache: &mut e.cache,
            obs: &mut e.obs,
            now: 0,
            max_body: 1024,
            secure: false,
            client_ip: Some("192.0.2.7".parse().unwrap()),
            sec: None,
            access: None,
            rate_limit: None,
            compression: None,

            fastcgi_cache: None,
        };
        let o = process(input.as_bytes(), &mut out, DATE, &mut ctx);
        (o, String::from_utf8(out).unwrap())
    }

    fn run_with(input: &str, t: &ProxyTable) -> (Outcome, String) {
        run_in(input, t, &mut env())
    }

    fn run(input: &str) -> (Outcome, String) {
        run_with(input, &ProxyTable::empty())
    }

    #[test]
    fn simple_get_keepalive() {
        let req = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let (o, resp) = run(req);
        assert_eq!(o.consumed, req.len());
        assert!(!o.close && matches!(o.action, Action::None));
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp.contains("Connection: keep-alive\r\n"));
        assert!(resp.contains("Content-Length: 27\r\n"));
        assert!(resp.ends_with("Vajra: hello from io_uring\n"));
    }

    #[test]
    fn pipelined_and_partial() {
        let req = "GET / HTTP/1.1\r\nHost: x\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (o, resp) = run(req);
        assert_eq!(o.consumed, req.len());
        assert_eq!(resp.matches("HTTP/1.1 200 OK").count(), 2);

        let (o, resp) = run("GET / HTTP/1.1\r\nHost: x");
        assert_eq!(o.consumed, 0);
        assert!(resp.is_empty());

        let first = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let (o, resp) = run(&format!("{first}GET /hea"));
        assert_eq!(o.consumed, first.len());
        assert_eq!(resp.matches("200 OK").count(), 1);
    }

    #[test]
    fn head_not_found_405() {
        let (_, resp) = run("HEAD / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(resp.contains("Content-Length: 27") && resp.ends_with("\r\n\r\n"));
        let (o, resp) = run("GET /nope?x=1 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(!o.close && resp.starts_with("HTTP/1.1 404 Not Found"));
        let (o, resp) = run("POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nabc");
        assert!(o.close && resp.starts_with("HTTP/1.1 405") && resp.contains("Allow: GET, HEAD"));
    }

    #[test]
    fn connection_semantics() {
        let (o, resp) = run("GET / HTTP/1.0\r\n\r\n");
        assert!(o.close && resp.contains("Connection: close"));
        assert!(
            !run("GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n")
                .0
                .close
        );
        assert!(run("GET / HTTP/1.1\r\nConnection: close\r\n\r\n").0.close);
        assert!(
            run("GET / HTTP/1.1\r\nContent-Length: 4\r\n\r\nabcd")
                .0
                .close
        );
    }

    #[test]
    fn malformed_requests() {
        let (o, resp) = run("\x00\x01\x02 nonsense\r\n\r\n");
        assert!(o.close && resp.starts_with("HTTP/1.1 400"));
        let (o, resp) = run("GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n");
        assert!(o.close && resp.starts_with("HTTP/1.1 400"));

        let mut req = String::from("GET / HTTP/1.1\r\n");
        for i in 0..(MAX_HEADERS + 1) {
            req.push_str(&format!("X-H{i}: v\r\n"));
        }
        req.push_str("\r\n");
        let (o, resp) = run(&req);
        assert!(o.close && resp.starts_with("HTTP/1.1 431"));
    }

    #[test]
    fn responses_are_reported_to_the_observer() {
        let mut e = env();
        e.obs.log_enabled = true;
        e.obs.now = 784_111_777;
        run_in("GET /health HTTP/1.1\r\nHost: x\r\n\r\nGET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n", &ProxyTable::empty(), &mut e);
        run_in("garbage\r\n\r\n", &ProxyTable::empty(), &mut e);
        assert_eq!(e.obs.m.requests_h1, 3);
        assert_eq!((e.obs.m.status[2], e.obs.m.status[4]), (1, 2));
        let log = String::from_utf8(e.obs.log.clone()).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with(
                "192.0.2.7 - - [06/Nov/1994:08:49:37 +0000] \"GET /health HTTP/1.1\" 200 3"
            ),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("\"GET /nope HTTP/1.1\" 404 10"),
            "{}",
            lines[1]
        );
        assert!(lines[2].contains("\"- - HTTP/1.1\" 400 12"), "{}", lines[2]);
    }

    #[test]
    fn websocket_handshake_becomes_an_upgrade_proxy_action() {
        let t = table(true);
        let req = "GET /api/ws HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n\r\nEXTRA";
        let (o, resp) = run_with(req, &t);
        assert!(resp.is_empty());
        assert!(!o.close);
        assert_eq!(
            o.consumed,
            req.len() - "EXTRA".len(),
            "early client bytes stay in the input"
        );
        let Action::Proxy(spec) = o.action else {
            panic!("expected proxy action")
        };
        assert!(spec.upgrade && !spec.idempotent);
        assert_eq!(
            spec.cache,
            CacheMode::None,
            "upgrades never touch the cache"
        );
        let up = String::from_utf8(spec.request).unwrap();
        assert!(up.contains("Sec-WebSocket-Key: abc\r\n") && up.contains("Upgrade: websocket\r\n"));

        // A plain `Upgrade: h2c` request is an ordinary proxied request.
        let (o, _) = run_with(
            "GET /api/x HTTP/1.1\r\nHost: e\r\nUpgrade: h2c\r\nConnection: Upgrade\r\n\r\n",
            &t,
        );
        let Action::Proxy(spec) = o.action else {
            panic!()
        };
        assert!(!spec.upgrade);
    }

    #[test]
    fn proxy_get_builds_upstream_request() {
        let t = table(false);
        let req = "GET /api/users?id=1 HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\n\r\n";
        let (o, resp) = run_with(req, &t);
        assert!(
            resp.is_empty(),
            "nothing is written until the upstream answers"
        );
        assert_eq!(o.consumed, req.len());
        assert!(!o.close);
        let Action::Proxy(spec) = o.action else {
            panic!("expected proxy action")
        };
        let up = String::from_utf8(spec.request.clone()).unwrap();
        assert!(up.starts_with("GET /api/users?id=1 HTTP/1.1\r\nHost: example.com\r\n"));
        assert!(up.contains("X-Forwarded-For: 192.0.2.7\r\n"));
        assert!(spec.idempotent && !spec.head);
        assert_eq!(spec.cache, CacheMode::None, "route has caching disabled");
    }

    #[test]
    fn proxy_post_waits_for_full_body() {
        let t = table(false);
        let head = "POST /api/x HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\n";
        let (o, _) = run_with(&format!("{head}he"), &t);
        assert_eq!(o.consumed, 0);
        assert!(matches!(o.action, Action::NeedBody));

        let full = format!("{head}hello");
        let (o, _) = run_with(&full, &t);
        assert_eq!(o.consumed, full.len());
        let Action::Proxy(spec) = o.action else {
            panic!()
        };
        assert!(!spec.idempotent);
        assert!(String::from_utf8(spec.request)
            .unwrap()
            .ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn proxy_body_limits() {
        let t = table(false);
        let (o, resp) = run_with("POST /api/x HTTP/1.1\r\nContent-Length: 999999\r\n\r\n", &t);
        assert!(o.close && resp.starts_with("HTTP/1.1 413"));
        let (o, resp) = run_with(
            "POST /api/x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            &t,
        );
        assert!(o.close && resp.starts_with("HTTP/1.1 411"));
    }

    #[test]
    fn cache_miss_then_hit_then_invalidate() {
        let t = table(true);
        let mut e = env();
        let get = "GET /api/users?id=1 HTTP/1.1\r\nHost: Example.com\r\n\r\n";

        // Miss: the proxy is asked to store under a normalised key.
        let (o, resp) = run_in(get, &t, &mut e);
        assert!(resp.is_empty());
        let Action::Proxy(spec) = o.action else {
            panic!("miss should proxy")
        };
        assert_eq!(
            spec.cache,
            CacheMode::Store("example.com/api/users?id=1".into())
        );

        // Simulate the worker storing the upstream answer.
        e.cache.put(
            "example.com/api/users?id=1".into(),
            CachedResponse {
                status: 200,
                headers: vec![(b"Content-Type".to_vec(), b"text/plain".to_vec())],
                body: Rc::new(b"cached!".to_vec()),
                stored_at: 0,
                expires_at: 1000,
            },
        );

        // Hit: answered locally, no action, X-Cache header, body intact.
        let (o, resp) = run_in(get, &t, &mut e);
        assert!(matches!(o.action, Action::None) && !o.close);
        assert_eq!(o.consumed, get.len());
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(resp.contains("X-Cache: HIT\r\n") && resp.contains("Content-Length: 7\r\n"));
        assert!(resp.ends_with("\r\n\r\ncached!"));
        assert_eq!(e.cache.stats.hits, 1);
        assert_eq!(e.obs.m.requests_h1, 1, "hits are counted as responses");

        // A hit followed by a pipelined proxied request keeps parsing.
        let both = format!("{get}GET /api/other HTTP/1.1\r\nHost: Example.com\r\n\r\n");
        let (o, resp) = run_in(&both, &t, &mut e);
        assert_eq!(resp.matches("X-Cache: HIT").count(), 1);
        assert!(matches!(o.action, Action::Proxy(_)));
        assert_eq!(o.consumed, both.len());

        // Unsafe methods ask for invalidation.
        let (o, _) = run_in(
            "POST /api/users?id=1 HTTP/1.1\r\nHost: example.com\r\nContent-Length: 0\r\n\r\n",
            &t,
            &mut e,
        );
        let Action::Proxy(spec) = o.action else {
            panic!()
        };
        assert_eq!(
            spec.cache,
            CacheMode::Invalidate("example.com/api/users?id=1".into())
        );

        // Authorization bypasses the cache in both directions.
        let (o, resp) = run_in(
            "GET /api/users?id=1 HTTP/1.1\r\nHost: example.com\r\nAuthorization: Bearer t\r\n\r\n",
            &t,
            &mut e,
        );
        assert!(resp.is_empty());
        let Action::Proxy(spec) = o.action else {
            panic!()
        };
        assert_eq!(spec.cache, CacheMode::None);
    }

    #[test]
    fn expired_cache_entries_are_not_served() {
        let t = table(true);
        let mut e = env();
        e.cache.put(
            "h/api/x".into(),
            CachedResponse {
                status: 200,
                headers: vec![],
                body: Rc::new(b"old".to_vec()),
                stored_at: 0,
                expires_at: 0,
            },
        );
        let (o, resp) = run_in("GET /api/x HTTP/1.1\r\nHost: h\r\n\r\n", &t, &mut e);
        assert!(resp.is_empty() && matches!(o.action, Action::Proxy(_)));
    }

    #[test]
    fn proxy_response_rendering() {
        let hdrs = vec![
            (b"Content-Type".to_vec(), b"text/plain".to_vec()),
            (b"Connection".to_vec(), b"close".to_vec()),
            (b"Content-Length".to_vec(), b"99".to_vec()),
            (b"X-Custom".to_vec(), b"1".to_vec()),
            (b"X-Cache".to_vec(), b"spoofed".to_vec()),
        ];
        let mut out = Vec::new();
        write_proxy_response(
            &mut out,
            201,
            &hdrs,
            b"abc",
            false,
            true,
            DATE,
            Some("MISS"),
            None,
            None,
            &crate::compress::CompressionSettings::default(),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 201 Created\r\n"));
        assert!(s.contains("X-Custom: 1\r\n") && s.contains("Content-Length: 3\r\n"));
        assert!(s.contains("X-Cache: MISS\r\n") && !s.contains("spoofed"));
        assert!(!s.contains("99") && s.contains("Connection: keep-alive"));
        assert!(s.ends_with("\r\n\r\nabc"));

        let mut out = Vec::new();
        write_proxy_response(
            &mut out,
            200,
            &hdrs,
            b"",
            true,
            false,
            DATE,
            None,
            None,
            None,
            &crate::compress::CompressionSettings::default(),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("Content-Length: 99\r\n"),
            "HEAD passes the upstream length through"
        );
        assert!(
            s.ends_with("\r\n\r\n") && s.contains("Connection: close") && !s.contains("X-Cache")
        );
    }

    #[test]
    fn h2_renderers() {
        let r = h2_proxy_response(
            200,
            &[(b"X-A".to_vec(), b"1".to_vec())],
            b"hi".to_vec(),
            false,
            DATE,
            Some("HIT"),
            None,
        );
        assert!(r.headers.iter().any(|(n, v)| n == b"x-a" && v == b"1"));
        assert!(r
            .headers
            .iter()
            .any(|(n, v)| n == b"content-length" && v == b"2"));
        assert!(r
            .headers
            .iter()
            .any(|(n, v)| n == b"x-cache" && v == b"HIT"));
        assert!(matches!(r.body, H2Body::Mem(_)));
        assert_eq!(h2_error(502, DATE).status, 502);
    }
}

#[cfg(test)]
mod compress_hot_path_tests {
    use super::*;
    use crate::compress::CompressionSettings;

    const DATE: &[u8; DATE_LEN] = b"Thu, 01 Jan 1970 00:00:00 GMT";
    static BIG: [u8; 4096] = [b'A'; 4096];
    const SMALL: &[u8] = b"tiny";

    fn settings(on: bool, min_size: usize) -> CompressionSettings {
        CompressionSettings {
            brotli: on,
            gzip: false,
            level: 6,
            gzip_level: 6,
            min_size,
        }
    }

    fn settings_gzip_only(min_size: usize) -> CompressionSettings {
        CompressionSettings {
            brotli: false,
            gzip: true,
            level: 6,
            gzip_level: 6,
            min_size,
        }
    }

    fn settings_both(min_size: usize) -> CompressionSettings {
        CompressionSettings {
            brotli: true,
            gzip: true,
            level: 6,
            gzip_level: 6,
            min_size,
        }
    }

    fn render_mem(
        body: &'static [u8],
        ctype: &'static str,
        head_only: bool,
        comp: Option<&CompressionSettings>,
        accept: Option<&str>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let reply = Reply::Mem {
            status: 200,
            ctype,
            body,
            allow: false,
        };
        let _ = write_reply(&mut out, &reply, head_only, true, DATE, comp, accept);
        out
    }

    /// Header block of an HTTP/1.1 response (up to the first CRLFCRLF).
    fn head_str(resp: &[u8]) -> &str {
        let end = resp
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no CRLFCRLF");
        std::str::from_utf8(&resp[..end]).expect("headers not UTF-8")
    }

    fn header_value<'a>(resp: &'a [u8], name: &str) -> Option<&'a str> {
        for line in head_str(resp).split("\r\n") {
            if let Some((k, v)) = line.split_once(": ") {
                if k.eq_ignore_ascii_case(name) {
                    return Some(v);
                }
            }
        }
        None
    }

    fn content_length(resp: &[u8]) -> u64 {
        header_value(resp, "Content-Length")
            .expect("no Content-Length")
            .parse()
            .expect("Content-Length not numeric")
    }

    #[test]
    fn br_negotiated_compresses_mem_body() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("br"));
        assert_eq!(header_value(&resp, "Content-Encoding"), Some("br"));
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert!(
            content_length(&resp) < BIG.len() as u64,
            "compressed size {} should be < {}",
            content_length(&resp),
            BIG.len()
        );
    }

    #[test]
    fn no_accept_encoding_serves_identity() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), None);
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn gzip_only_serves_identity() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("gzip"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn br_with_q0_serves_identity() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("br;q=0"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn body_below_min_size_serves_identity() {
        let comp = settings(true, 1024);
        let resp = render_mem(SMALL, "text/html", false, Some(&comp), Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert_eq!(content_length(&resp), SMALL.len() as u64);
    }

    #[test]
    fn pre_compressed_content_type_skipped() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "image/png", false, Some(&comp), Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        // is_pre_compressed → skip entirely; no Vary either.
        assert!(header_value(&resp, "Vary").is_none());
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn disabled_settings_serve_identity() {
        let comp = settings(false, 1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert!(header_value(&resp, "Vary").is_none());
        assert_eq!(content_length(&resp), BIG.len() as u64);

        // And with no settings at all:
        let resp = render_mem(&BIG, "text/html", false, None, Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert!(header_value(&resp, "Vary").is_none());
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn head_request_not_compressed() {
        let comp = settings(true, 1024);
        let resp = render_mem(&BIG, "text/html", true, Some(&comp), Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }

    #[test]
    fn gzip_served_when_brotli_disabled() {
        let comp = settings_gzip_only(1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("gzip"));
        assert_eq!(header_value(&resp, "Content-Encoding"), Some("gzip"));
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert!(content_length(&resp) < BIG.len() as u64);
    }

    #[test]
    fn brotli_preferred_when_both_available() {
        let comp = settings_both(1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("gzip, br"));
        assert_eq!(header_value(&resp, "Content-Encoding"), Some("br"));
    }

    #[test]
    fn gzip_fallback_when_br_not_offered() {
        let comp = settings_both(1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("gzip"));
        assert_eq!(header_value(&resp, "Content-Encoding"), Some("gzip"));
        assert!(content_length(&resp) < BIG.len() as u64);
    }

    #[test]
    fn gzip_only_not_used_when_client_asks_br_only() {
        let comp = settings_gzip_only(1024);
        let resp = render_mem(&BIG, "text/html", false, Some(&comp), Some("br"));
        assert!(header_value(&resp, "Content-Encoding").is_none());
        assert_eq!(header_value(&resp, "Vary"), Some("Accept-Encoding"));
        assert_eq!(content_length(&resp), BIG.len() as u64);
    }
}
