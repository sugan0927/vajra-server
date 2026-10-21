## [0.15.11] — 2026-10-21

### Fixed
- TLS crash loop when `/etc/letsencrypt` is missing → fallback cert in `[tls]`
- SNI cert mismatch (api served wp's cert) → per-host ACME + SNI select
- Orphan `vajra` process on `systemctl stop` → `KillMode=mixed`
- ACME rate-limit on stale test domains → config written without test sites

### Added
- `install.sh`: Debian/Ubuntu, RHEL/Rocky/Alma/Fedora, Arch, openSUSE, Alpine
- Auto-detects PHP-FPM socket, MariaDB service, init system
- Non-destructive `wp-config.php`
- Tarball release support (`vajra-v0.15.11.tar.gz`)

### Changed
- `tls_cert` / `tls_key` in `[[site]]` no longer required
- `[tls]` defaults to `/etc/vajra/certs/fallback.{crt,key}`
