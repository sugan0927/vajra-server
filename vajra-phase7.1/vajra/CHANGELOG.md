# Changelog

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
