# Changelog

All notable changes to Vajra are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/).

## [0.12.0] — 2026-10-12

### Added
- **In-process backup scheduler** (`src/backup/scheduler.rs`). A
  dedicated `std::thread` ("vajra-backup") wakes every
  `VAJRA_BACKUP_TICK_SECS` (default 5 s), checks whether the cron
  schedule fired in `(last_check, now]`, and runs the local (+ optional
  S3) backup once per firing. Shutdown is signal-based; the loop
  observes it within one tick. Reverses the Day 23 decision to rely on
  a systemd timer.
- **S3 backup destination** (`src/backup/s3.rs`). Uses `rust-s3`'s
  blocking client (`tokio-free`). Region::Custom + path-style for
  MinIO / R2 / B2. Credentials env-first. Multipart above the crate
  threshold.
- **Backup Prometheus metrics**: `vajra_backup_runs_total`,
  `vajra_backup_last_success_timestamp_seconds`,
  `vajra_backup_archive_bytes`.
- **`Raw` config plumbing** so `[backup]` in a real TOML file parses
  end-to-end.

## [0.11.0] — 2026-10-12

### Added
- **[backup] config schema** (`src/config.rs`): `BackupConfig`,
  `BackupDestination` (Local | S3), `BackupDatabase`, `BackupFiles`.
  Validation covers schedule cron grammar, `keep_days` bounds,
  `local_dir` absolute, S3 bucket/region required. Credentials are
  env-first (`VAJRA_S3_ACCESS_KEY`, `VAJRA_S3_SECRET_KEY`,
  `VAJRA_DB_PASSWORD`).
- **Local backup** (`src/backup/local.rs`): `mysqldump`
  (`--single-transaction --quick --skip-lock-tables --hex-blob`,
  password via `MYSQL_PWD`), `tar` + `zstd`, atomic `.partial` ->
  `rename(2)`, retention via `keep_days`. Never leaves a torn archive.
- `docs/backup.md`.

### Fixed
- Day 24 / Day 25 landed with a broken build; fixes included
  `rust-s3 --features blocking,tokio-rustls-tls` (not `rustls-tls`),
  removing a stray `From<crate::config::BackupError>` impl, and
  rewriting `s3.rs` to drop a prune path that relied on an async
  `bucket.list()`.

## [0.13.0] — 2026-10-12

### Added
- **`vajra --check` — deep config validation** (`src/check.rs`).
  Prints a structured report with error / warning / info severities;
  exit code 1 on any error. Checks include: `[server]` bind privilege,
  worker count vs CPU count, grace; `[admin]` non-loopback exposure;
  `[tls]` cert/key readability and expiry (`openssl x509 -checkend`);
  `[php]` PHP-FPM unix socket reachable, TCP upstream reachable;
  `[backup]` local_dir writable, DB host:port reachable, S3 creds
  presence. Never mutates; bounded I/O (2 s connect_timeout).
- **`vajra setup` — interactive config wizard** (`src/setup.rs`).
  Asks domain / email / PHP version / redis / backups / rate limit,
  writes `/etc/vajra/app.toml` (refuses to overwrite without
  confirmation), and leaves validation to `vajra --check`. Wizard is
  driven by a `Prompter` trait so it is testable without a TTY.
- `docs/check.md`, `docs/setup.md`.

### Fixed
- `BackupConfig::default().schedule` used a 5-field cron expression
  (`"0 3 * * *"`) that the `cron` crate rejects; corrected to
  `"0 0 3 * * *"`. Would have failed load for any minimal `[backup]`
  config with `enabled = true`.
- `setup::render()` now emits the full `[rate_limit]` block:
  `requests_per_second`, `burst`, and `max_tracked_ips` — the last two
  are required by the schema when `enabled = true`.

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
