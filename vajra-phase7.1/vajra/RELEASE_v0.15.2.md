# Vajra v0.15.2 — Stability Release

Date: 2026-10-18

## Added
- systemd `Restart=on-failure` drop-ins for vajra, php8.4-fpm, mariadb
  (`/etc/systemd/system/<svc>.service.d/override.conf`)
- Python healthcheck timer (`vajra-healthcheck.timer`, every 60s)
  - systemd state
  - TCP listen on :443
  - PHP-FPM socket presence
  - MariaDB SELECT 1
  - Restarts unhealthy service automatically
- Maintenance flag: `/etc/vajra/.maintenance` disables restarts
- Self-healing kill test (`/usr/local/bin/vajra-selftest.py`)

## Verified
- Boot persistence: all 3 services `enabled`
- SIGKILL recovery: each service back within 3-5s
- Reboot: full stack comes back, HTTPS live
- WordPress: HTTP/2 200, TLS valid (LE cert till 2027-01-13)
- Reverse proxy `/api/` still routing to backends

## Not changed
- Rust source (0.15.1 binary)
- TOML schema
