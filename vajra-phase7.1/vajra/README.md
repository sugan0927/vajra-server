# Vajra

Thread-per-core, share-nothing web server for Linux, built on io_uring.

Vajra is a from-scratch HTTP server in Rust with no async runtime — the
worker threads drive io_uring directly. It serves static files, acts as a
reverse proxy, hosts PHP-FPM applications (WordPress), and ships with an
operator CLI for setup, validation, WP-CLI, and auto-backup.

> **Status:** experimental. Not independently audited; do not expose to
> the open internet without reviewing `SECURITY.md`. Compiled and tested
> on Debian 12 / Linux 6.1.

## Install

```sh
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server/vajra-phase7.1/vajra
cargo build --release
sudo ./install.sh --bin=target/release/vajra
```

`install.sh` (Debian/Ubuntu) installs `php-fpm` and `wp-cli`, drops
`vajra` at `/usr/local/bin/vajra`, and writes `/etc/vajra/config.toml`
if it does not already exist. It also drops the `vajra-wp` shim.

## Quick start

```sh
# Interactive config wizard
sudo vajra setup

# Deep validation (syntax, PHP-FPM socket, DB, TLS files)
vajra --check --config /etc/vajra/app.toml

# Run the server
sudo vajra --config /etc/vajra/app.toml
```

## Features

**Protocols**
- HTTP/1.1 and HTTP/2 (hand-written parser)
- HTTP/3 (QUIC) via quinn-proto, driven by the same io_uring loop
- TLS via rustls (TLS 1.2+), ALPN; WebSocket tunnels (`ws://`, `wss://`)

**Serving**
- Zero-copy static files (`splice` file -> pipe -> socket on plain HTTP/1.1)
- Reverse proxy: longest-prefix routes, multiple upstreams, `round_robin` /
  `least_conn` / `ip_hash`, passive health (`max_fails`, `fail_timeout_secs`)
- Per-core LRU response cache (`X-Cache: HIT|MISS`)
- On-the-fly compression (gzip, brotli)
- PHP-FPM via TCP or Unix socket; directory index; front controller for
  WordPress pretty permalinks; `deny_exec` for uploads; dotfile protection

**Operations**
- Prometheus metrics (`/metrics` on the admin listener)
- Common Log Format access log (io_uring writes)
- Health endpoints (`/healthz`, `/readyz`) on a side listener
- Hot reload via SIGHUP or `POST /reload`; graceful shutdown
- Rate limiting, access control (IP/CIDR allow/deny, trusted proxies),
  security headers
- FastCGI response cache with canonical flat metrics

**CLI**
- `vajra [flags]` — run the server
- `vajra --check [--config FILE]` — deep config validation
- `vajra setup [--config FILE]` — interactive config wizard
- `vajra wp <args...>` — WP-CLI wrapper (drops to `www-data`)
- `vajra-wp <args...>` — shim around `vajra wp` (installed by `install.sh`)

**Auto-backup**
- Local (mysqldump + tar + zstd) and S3 destinations
- In-process scheduler (cron syntax), keeps `keep_days` archives
- Metrics: `vajra_backup_runs_total`,
  `vajra_backup_last_success_timestamp_seconds`,
  `vajra_backup_archive_bytes`

## Documentation

| Topic | Guide |
|---|---|
| Configuration reference | `docs/USER_GUIDE.md` |
| Architecture | `docs/ARCHITECTURE.md` |
| Admin HTTP API | `docs/API.md` |
| WordPress / PHP-FPM | `docs/WORDPRESS.md` |
| WP-CLI wrapper | `docs/wp-cli.md` |
| Auto-backup | `docs/backup.md` |
| Config validation (`--check`) | `docs/check.md` |
| Config wizard (`setup`) | `docs/setup.md` |
| FastCGI cache | `docs/fastcgi-cache.md` |
| Rate limiting | `docs/rate-limiting.md` |
| Access control | `docs/access-control.md` |
| Security headers | `docs/security-headers.md` |
| Metrics | `docs/metrics.md` |
| Health checks | `docs/health.md` |
| Compression | `docs/compression.md` |
| Benchmarks | `docs/BENCHMARKS.md` |
| Tuning | `docs/TUNING.md` |
| Verify | `docs/verify.md` |

## Configuration

Config is TOML. `vajra setup` generates a starter `/etc/vajra/app.toml`
for a WordPress deployment. Full schema in `docs/USER_GUIDE.md`.

Minimal example:

```toml
[server]
listen     = "0.0.0.0:80"
workers    = 0            # 0 = one per CPU
pin        = true
grace_secs = 5

[admin]
# Metrics + reload endpoint. NO authentication; keep on loopback.
# listen = "127.0.0.1:9200"

[static]
root = "/var/www/html"

[[proxy]]
prefix    = "/"
upstreams = ["unix:/run/php/php8.2-fpm.sock"]
balance   = "round_robin"
```

## Environment variables

| Variable | Purpose |
|---|---|
| `VAJRA_WP_PATH` | `--path=` injected into `vajra wp` when omitted |
| `VAJRA_WP_NO_SUDO` | If set, `vajra wp` never spawns `sudo` |
| `VAJRA_DB_PASSWORD` | Overrides `[backup.database].password` |
| `VAJRA_S3_ACCESS_KEY` | Overrides `[backup].s3_access_key` |
| `VAJRA_S3_SECRET_KEY` | Overrides `[backup].s3_secret_key` |
| `VAJRA_BACKUP_TICK_SECS` | Scheduler wake-up interval (default 5 s) |

## Building

```sh
rustup toolchain install stable    # 1.80 or newer
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

## License

MIT OR Apache-2.0 (per `Cargo.toml`). Note: a top-level `LICENSE` file
is currently missing from the tree.

## Contributing

Personal project; issues and PRs welcome but reviews may be slow.
Please run `cargo test && cargo clippy --all-targets -- -D warnings`
before opening a PR.
