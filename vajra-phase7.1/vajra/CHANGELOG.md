# Changelog

All notable changes to Vajra are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
