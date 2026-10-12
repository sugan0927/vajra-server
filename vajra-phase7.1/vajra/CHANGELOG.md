# Changelog

All notable changes to Vajra are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/).

## [0.10.0] — 2026-10-12

### Added
- **`vajra wp` — WP-CLI wrapper.** Runs WP-CLI as `www-data`.
  - Resolves `wp` via `$PATH` before dropping privileges, so a missing
    binary gives a readable error + exit 127 (not a sudo trace).
  - `sudo -u www-data -- wp ...` only when: root AND `sudo` on `$PATH`
    AND `VAJRA_WP_NO_SUDO` unset. Otherwise direct exec.
  - `VAJRA_WP_PATH` is injected as `--path=...` when the caller did not
    pass `--path` / `--path=<dir>`.
  - New module: `src/wp.rs`. Dispatch in `src/main.rs` runs before any
    config load / io_uring setup.
- **Canonical flat cache metrics** (Day 21):
  - `vajra_cache_hits_total` (IntCounter)
  - `vajra_cache_misses_total` (IntCounter)
  - `vajra_cache_evictions_total` (IntCounter)
  - `vajra_cache_size_bytes` (IntGauge)
  - These are exported **alongside** the Day 19e `vajra_fastcgi_cache_*`
    family, so existing dashboards keep working. Both sets are bumped
    by the same `record_fastcgi_cache_*` helpers.
- **`install.sh`** — Debian/Ubuntu installer.
  - Installs `php-fpm` + `wp-cli`, drops `vajra` at `/usr/local/bin/vajra`,
    writes `/etc/vajra/config.toml` (never overwrites).
  - Also drops `/usr/local/bin/vajra-wp` — a one-line shim that execs
    `vajra wp "$@"`.
  - Flags: `--wp-path=<dir>`, `--bin=<path>`.
- **`docs/wp-cli.md`** — usage, behaviour table, `--path` precedence,
  env overrides, installer notes, testing notes.
- **Integration test** `tests/php.rs::fastcgi_cache_lifecycle_miss_hit_purge`:
  MISS (fill) → HIT (byte-for-byte replay) → on-disk purge → MISS
  (worker resynced via `PURGE_GEN`).
- **Unit test** `flat_metrics_tests::flat_metrics_are_exported`.
- **Integration tests** `tests/wp.rs` (6 tests): passthrough, `VAJRA_WP_PATH`
  injection, explicit `--path` wins, missing `wp` → 127, `db export`,
  `core update`.

### Changed
- `FastcgiCacheMetrics` struct grew four fields (see above). The old
  fields are untouched — this is backward compatible.
- `record_fastcgi_cache_hit` / `_miss` / `_eviction` / `_stats` now
  also bump the flat metrics.

### Fixed
- `--help` output now reports the version from `CARGO_PKG_VERSION`
  instead of a hardcoded string that had drifted behind `Cargo.toml`.

## [0.9.0] — (previous)
See `git log` for details.
