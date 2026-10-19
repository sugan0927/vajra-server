# Vajra v0.15.2 — Production Stability Release

Date: 2026-10-19

## Fixed
- Boot delay: vajra was blocked by `network-online.target` (2+ min delay
  caused by `systemd-networkd-wait-online.service` on this host).
  - Drop-in now uses `After=` (empty first) to clear inherited ordering,
    then `After=network.target php8.4-fpm.service mariadb.service`.
  - `systemd-networkd-wait-online.service` masked; `ifupdown-wait-online`
    is the active network readiness signal (3s).
- Healthcheck `is_active()` was too permissive — treats transitional
  states as active. Now requires state ∈ {active, reloading}.
- Healthcheck `restart()` was failing during start-limit lockout. Now
  calls `systemctl reset-failed` first.

## Added
- systemd drop-ins with `Restart=on-failure` + rate limits for
  vajra, php8.4-fpm, mariadb.
- Python healthcheck timer (60s) with:
  - strict systemd state check
  - TCP :443 reachability
  - PHP-FPM socket presence
  - MariaDB `SELECT 1`
  - reset-failed-before-restart to survive lockouts
- Maintenance flag: `/etc/vajra/.maintenance` disables auto-heals.
- Self-healing kill test (`/usr/local/bin/vajra-selftest.py`).

## Verified (post-reboot)
- All services active within seconds of boot (no 2min stall)
- SIGKILL recovery: 3-5s per service
- Start-limit lockout recovery via healthcheck: PASS
- WordPress HTTPS: 200, TLS valid (LE cert till 2027-01-13)
- Reverse proxy `/api/` routes to backends

## Not changed
- Rust source (0.15.1 binary, unchanged)
- TOML schema
