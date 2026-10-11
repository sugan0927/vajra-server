# Changelog

## [0.9.0] - 2026-10-11

### Added

**Security response headers** — configurable via `[headers]`:
- `hsts`, `x_frame_options`, `x_content_type_options`, `referrer_policy`,
  `permissions_policy` — each maps to its canonical wire header.
- Per-path override via `[[headers.overrides]]` — longest-prefix match wins;
  an override with `""` removes the header for the matched path; omitted keys
  inherit from the top-level block.
- Applied on **all three protocols**: HTTP/1.1, HTTP/2, and HTTP/3.
- Managed headers are replaced case-insensitively; unmanaged headers are left
  untouched (upstream values pass through).
- CR/LF in values rejected at parse time (header-injection defense).
- Reference: `docs/security-headers.md`. Tests: `tests/security_headers.rs` (15).

**Metrics polish**:
- `vajra_requests_in_flight` gauge (atomic hot path; flushed on scrape and
  every 64th decrement).
- `vajra_config_reloads_total` and `vajra_config_reload_errors_total`
  counters; wired into admin `POST /reload`.
- Prometheus alert `VajraReloadErrors`.
- Grafana dashboard `grafana/dashboard.json` (8 panels: requests/sec by
  status class, PHP-FPM outcomes, latency p50/p95/p99, bandwidth, uptime,
  in-flight, active connections, reload counters).
- `docs/verify.md`: copy-paste curl commands for every metric family.
- Tests: `tests/metrics_render.rs` (4) for Prometheus exposition format.

### Changed

- Prometheus exposition format: histogram families are now correctly
  normalized when checking for `# HELP` / `# TYPE` lines.
- HTTP/2 and HTTP/3 response paths (`h2_base`, `h2_proxy_response`,
  `h2_error`) now share the security-header emission with HTTP/1.1.

### Security

- **GHSA-2gh3-rmm4-6rq5** (protobuf uncontrolled recursion DoS) — resolved
  by disabling the unused `default-features` on `prometheus`; closes
  Dependabot alert #2.
- Security headers managed by the config always replace any same-named
  header coming from an upstream or from the cache (case-insensitive),
  preventing header smuggling via proxied applications.

### Known Issues (WIP)

Unchanged from 0.8.0 — 10 integration tests remain `#[ignore]`d pending a
follow-up release (HTTP/2 flow-control, HTTP/3 stream lifecycle, etc.).
See `tests/` for the exact list.

## [0.8.0] - 2026-10-11

### Added
- **PHP-FPM metrics** (Prometheus, on `/metrics`):
  - `vajra_php_fpm_requests_total{status="ok|error|timeout|parse"}`
  - `vajra_php_fpm_duration_seconds{status="..."}` histogram
  - `vajra_bytes_sent_total`, `vajra_bytes_received_total` (atomic fast-path)
- Prometheus alert rules — `alerts/vajra.yaml` (7 rules)
- Metrics reference — `docs/metrics.md`
- Integration test — `tests/metrics.rs`

### Changed
- `http::process()` — byte accounting (atomic fast-path + ~1 MiB flush)
- `proxy_fail` / `proxy_done` — observe FPM metrics (guarded by `fpm_observed`)

### Fixed
- `vajra_php_fpm_requests_total{status="parse"}` — increment on FastCGI decode error

### Known Issues (WIP)
Ten integration tests are `#[ignore]`d pending follow-up (in `wip:` commit `17e79d2`
scope, not part of this release's feature work):
- `tests/tls_h2.rs`: 5 tests (HTTP/2 flow-control, large-file streaming)
- `tests/http3.rs`: 2 tests (stream lifecycle, oversized body)
- `tests/robustness.rs`: 1 test (FastCGI fuzz)
- `tests/phase4.rs`: 1 test (access log format — request UUID appended)
- `tests/websocket.rs`: 1 test (refused upgrade)

These are tracked for a follow-up release.

### Security
- **GHSA-w7hm-hmxv-pvhf** (CVSS 7.5) — replaced unmaintained `hpack` v0.3.0 with
  `hpack-patched` v0.3.0 via `Cargo.toml` `package = "..."` rename. No source
  changes (same Decoder/Encoder API).
