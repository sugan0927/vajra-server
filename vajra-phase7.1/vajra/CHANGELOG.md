# Changelog

All notable changes to Vajra are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.15.4] — 2026-10-20

### Added
- Built-in Redis-compatible Object Cache (`[object_cache]`) for WordPress database query caching.
- Built-in DDoS Protection (`[security]`) with IP connection rate-limiting (30/min) and auto-ban (10 min).
- Updated `install.sh` to generate the correct `app.toml` with all new sections.

## [0.15.3] — 2026-10-20

### Fixed
- **Critical**: `Settings::dynamic()` used `..Default::default()` for
  `compression`, `access`, `rate_limit`, `fastcgi_cache` — all four
  silently disabled at runtime despite correct TOML parsing.
- Brotli/gzip compression end-to-end on HTTP/1.1 (MISS + cache HIT paths).
- `Vary: Accept-Encoding` on all compressible responses.
- Duplicate `Content-Length` on proxy responses.
- Startup log falsely claimed "HTTP/2 via ALPN".

## [0.14.4] — 2026-10-17

### Fixed — 3 critical bugs

#### 1. TLS response truncation at 64 KB (`src/worker.rs`)
HTTPS responses larger than ~64 KB silently stopped at exactly 61440 bytes,
then stalled until client timeout. Clear HTTP (port 80) unaffected.
- **Root cause:** `stage_tx()` TLS branch used `write_all` into rustls' bounded
  buffer, discarded `WriteZero`, then `c.out.clear()`-ed the remaining bytes.
- **Fix:** loop-write into rustls, draining ciphertext into `c.net` after each
  iteration, until `c.out` is fully consumed.
- **Verified:** 200 KB PHP response → `HTTP:200 bytes:202000`.

#### 2. Raw PHP source leaked as `application/octet-stream` (`src/router.rs`)
`GET /wp-admin/` returned 7864 B of literal `wp-admin/index.php` source with
`Content-Type: application/octet-stream`. Mobile browsers showed a
"Download file again?" popup instead of the dashboard.
- **Root cause:** `route_php()` checked `n.ends_with('/')`, but
  `php::classify()` strips the trailing slash from `Class::Other`.
- **Fix:** check the ORIGINAL `path` argument.
- **Verified:** `/wp-admin/` → `HTTP:200 bytes:132198 ctype:text/html`.

#### 3. Multi-site FastCGI dispatch broken on HTTP/1.1 (`src/http.rs`)
All HTTPS/1.1 requests went to the FIRST site's PHP pool. `api.aidoor.co.in`
served WordPress content belonging to `wp.aidoor.co.in`.
- **Root cause:** H1 path hardcoded `router::route(..., None)`, never
  extracting `site_php` from the looked-up `SiteEntry`.
- **Fix:** extract `(php_route_idx, php_cfg)` and pass it to `router::route`,
  same as the H2 path does.
- **Verified:** `api.aidoor.co.in/` → `{"service":"aidoor-api","status":"ok"}`.

### Added
- **Cross-distro static binary** — `x86_64-unknown-linux-musl`. Runs on any
  Linux distro (Debian, Ubuntu, RHEL, Fedora, Arch, Alpine, openSUSE) with
  kernel 5.6+ — no runtime dependencies.
- `install.sh` / `uninstall.sh` for the tarball.
- `LICENSE` (MIT), this `CHANGELOG.md`.

### Changed
- `release` profile: `strip = true`, `lto = "thin"`, `codegen-units = 1`.
- Worker: `io::Error::other(...)` in place of deprecated
  `io::Error::new(io::ErrorKind::Other, …)` (clippy clean).

### Known Limitations
- **HTTP/2 disabled** at the ALPN layer (`src/tls.rs`). A separate h2
  frame-parser bug (`FRAME_SIZE_ERROR` on large bodies) is tracked for 0.15.
  Clients negotiate HTTP/1.1. h2 integration tests are marked `#[ignore]`
  in `tests/tls_h2.rs` until then.
- **Unknown Host header** silently falls back to the first configured site
  instead of returning 404. Tracked for 0.15.

### Upgrade Notes
- Drop-in replacement — same config format, same CLI flags.
- `sudo systemctl restart vajra.service` after replacing the binary.

## [0.14.0] and earlier
Initial phase-7.1 development. Not publicly released.

## [0.15.1] - 2026-10-17

### Fixed
- HTTP/1.1 requests with `Connection: close` that route through a
  proxy (PHP-FPM) or file transfer received `cURL error 52: Empty
  reply from server`. The cause was `stage_tx` sending the TLS
  `close_notify` alert *before* the response bytes were staged, so
  the client saw "end of stream" and discarded the response that
  followed. `close_notify` is now deferred until any pending proxy
  job or file transfer has been drained (`c.proxy.is_none()` and
  `c.xfer.is_none()`).

  This also fixes the original symptom that motivated this release:
  WordPress loopback requests, WP-Cron, and Site Health all use
  HTTP/1.0 (or HTTP/1.1 + `Connection: close`), which hit exactly
  this code path. The `mu-plugins/vajra-loopback-fix.php` workaround
  can be removed from WordPress sites once this version is installed.

## [0.15.8] - 2026-10-20

### Fixed
- Compile & ship per-IP rate-limiting / banning feature (`IpStats`) that
  was introduced in v0.15.7 but never built:
  - `Worker::new` now takes shared `Arc<DashMap<IpAddr, IpStats>>`
  - `IpStats` (and its fields) made `pub` for cross-module access
  - Fixed `mut` bindings in `get_mut` / `entry` paths
  - Fixed `Arc` move-in-loop in `src/main.rs` (per-worker clone)
  - Updated 8 test call sites (`tests/*.rs`) to pass the new arg

### Changed
- All workers now share **one global** IP-stats map — banning decisions
  are consistent across worker threads (previously per-worker).

### Known limitations
- 11 integration tests remain `#[ignore]`:
  - HTTP/2 (`h2` disabled at ALPN — see 0.14.4)
  - Some HTTP/1.1 HTTPS paths marked `WIP: stale after phase7.1 changes`
  - One WebSocket upgrade-refusal test
  These are documented WIP, not regressions in this release.

## [0.15.8] - 2026-10-20

### Fixed
- Compile & ship per-IP rate-limiting / banning feature (`IpStats`) that
  was introduced in v0.15.7 but never built:
  - `Worker::new` now takes shared `Arc<DashMap<IpAddr, IpStats>>`
  - `IpStats` (and its fields) made `pub` for cross-module access
  - Fixed `mut` bindings in `get_mut` / `entry` paths
  - Fixed `Arc` move-in-loop in `src/main.rs` (per-worker clone)
  - Updated 8 test call sites (`tests/*.rs`) to pass the new arg

### Changed
- All workers now share **one global** IP-stats map — banning decisions
  are consistent across worker threads (previously per-worker).

### Known limitations
- 11 integration tests remain `#[ignore]`:
  - HTTP/2 (`h2` disabled at ALPN — see 0.14.4)
  - Some HTTP/1.1 HTTPS paths marked `WIP: stale after phase7.1 changes`
  - One WebSocket upgrade-refusal test
  These are documented WIP, not regressions in this release.
