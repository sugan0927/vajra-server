# Vajra Web Server + Multi-Site Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Debian%20%7C%20Ubuntu%20%7C%20RHEL%20%7C%20Fedora%20%7C%20Arch-red)]()
[![PHP](https://img.shields.io/badge/PHP-8.5%20%7C%208.4-777BB4?logo=php&logoColor=white)](https://www.php.net/)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

**One VPS. Unlimited WordPress, PHP, or static HTML sites.** Install the
full stack with a single command, then create isolated sites with
`vajra-site add`.

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
```

Requirements: **Linux kernel 5.1+** (io_uring), **systemd**, **x86_64**.

Create a WordPress site:

```bash
vajra-site add blog.example.com you@example.com
```

Create a PHP-only site:

```bash
vajra-site add app.example.com you@example.com --php
```

Create a static HTML site:

```bash
vajra-site add landing.example.com you@example.com --html
```

---

## Table of contents

- [Supported distributions](#supported-distributions)
- [Installer usage](#installer-usage)
- [Site management — `vajra-site`](#site-management--vajra-site)
- [Runtime management (systemd / logs)](#runtime-management-systemd--logs)
- [Architecture](#architecture)
- [Environment variables](#environment-variables)
- [Building from source](#building-from-source)
- [Troubleshooting](#troubleshooting)
- [License](#license)
- [Support](#support)

---

## Supported distributions

| Family | Distro | Versions | Package manager | PHP source | Nginx layout |
|---|---|---|---|---|---|
| **Debian** | Debian | 11 (Bullseye), 12 (Bookworm), 13 (Trixie) | `apt` | SURY | `sites-available` / `sites-enabled` |
| **Debian** | Ubuntu | 20.04, 22.04, 24.04, 26.04 | `apt` | SURY | `sites-available` / `sites-enabled` |
| **Debian** | Linux Mint | 20, 21, 22 | `apt` | SURY | `sites-available` / `sites-enabled` |
| **Debian** | Pop!_OS | 22.04+ | `apt` | SURY | `sites-available` / `sites-enabled` |
| **Debian** | Kali | rolling | `apt` | SURY | `sites-available` / `sites-enabled` |
| **Debian** | Raspbian | 11, 12 | `apt` | SURY | `sites-available` / `sites-enabled` |
| **RHEL** | RHEL | 9.x | `dnf` | Remi | `/etc/nginx/conf.d` |
| **RHEL** | AlmaLinux | 9.x | `dnf` | Remi | `/etc/nginx/conf.d` |
| **RHEL** | Rocky Linux | 9.x | `dnf` | Remi | `/etc/nginx/conf.d` |
| **RHEL** | Oracle Linux | 9.x | `dnf` | Remi | `/etc/nginx/conf.d` |
| **RHEL** | CentOS Stream | 9, 10 | `dnf` | Remi | `/etc/nginx/conf.d` |
| **RHEL** | Fedora | 38, 39, 40, 41+ | `dnf` | Fedora main | `/etc/nginx/conf.d` |
| **Arch** | Arch Linux | rolling | `pacman` | main repo | `/etc/nginx/conf.d` |
| **Arch** | Manjaro | rolling | `pacman` | main repo | `/etc/nginx/conf.d` |
| **Arch** | EndeavourOS | rolling | `pacman` | main repo | `/etc/nginx/conf.d` |
| **Arch** | Garuda Linux | rolling | `pacman` | main repo | `/etc/nginx/conf.d` |

> ❌ **RHEL / Alma / Rocky 8.x are NOT supported.** Kernel 4.18 ships without
> an io_uring backport. The installer will refuse and print guidance.

The installer detects the OS family from `/etc/os-release`, picks the right
package manager (`apt`, `dnf`, or `pacman`), PHP source (SURY, Remi, or main
repo), and Nginx path layout automatically.

It downloads a statically-linked musl binary from the GitHub release, so no
compilation is needed and glibc version does not matter. If no tarball exists
for the tag, it falls back to building from source on the target machine.

---

## Installer usage

### One-liner (pipe to bash)

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
```

### Download and run with arguments

```bash
bash install.sh                                              # install stack only
bash install.sh example.com you@example.com                  # install + first WordPress site
bash install.sh example.com you@example.com --php            # install + first PHP site
bash install.sh example.com you@example.com --html           # install + first static HTML site
bash install.sh example.com you@example.com --wordpress      # explicit WordPress
bash install.sh example.com you@example.com --wp             # shorthand
```

### Installer environment variables

| Var | Default | Purpose |
|---|---|---|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repo to fetch binary from |
| `VAJRA_VERSION` | `latest` | Release tag, or `latest` to auto-resolve |
| `VAJRA_SKIP_BINARY` | `0` | `1` keeps the existing `/usr/local/bin/vajra` |
| `MARIADB_VERSION` | `mariadb-11.4` | MariaDB LTS series to configure |
| `SITE_MODE` | `wordpress` | Default first-site mode: `wordpress`, `php`, or `html` |

Example:

```bash
VAJRA_VERSION=v0.7.1 MARIADB_VERSION=mariadb-10.11 \
  bash install.sh example.com you@example.com --php
```

### What the installer does (step by step)

1. **Pre-flight** — detect distro, verify kernel ≥ 5.1, free port 80, open firewall.
2. **io_uring check** — compile and run a tiny probe; refuses RHEL 8, Hyper-V, seccomp-locked containers.
3. **Base packages** — nginx, MariaDB 11.4 (via `mariadb_repo_setup`), certbot, curl, openssl, python3.
4. **PHP** — SURY (Debian family) / Remi (RHEL family) / main repo (Arch). Prefers 8.5, falls back to 8.4.
5. **Vajra binary** — downloads musl tarball from GitHub release; builds from source if unavailable.
6. **WP-CLI** — installs `/usr/local/bin/wp` if missing.
7. **Directories + systemd template** — `/etc/vajra/{sites,certs}`, `/var/log/vajra`, `/var/www/sites`, `/var/www/certbot`, `vajra-site@.service`.
8. **Nginx base** — removes default vhosts, installs catch‑all welcome page + default server.
9. **`vajra-site` CLI** — drops `/usr/local/bin/vajra-site`.
10. **Post‑install verification** — checks MariaDB, PHP‑FPM, nginx, ports, config validity.

---

## Site management — `vajra-site`

The installer drops a single CLI, `vajra-site`, at `/usr/local/bin/vajra-site`.
It manages every site end-to-end: DB, WP-CLI, Vajra TOML, systemd unit, Nginx
vhost, and Let's Encrypt cert.

### Synopsis

```
vajra-site <command> [arguments] [options]
```

### Commands

| Command | Description |
|---|---|
| `vajra-site add <domain> <email> [mode] [opts]` | Create a new site (default WordPress) |
| `vajra-site list` (alias `ls`) | Tabular listing of all sites |
| `vajra-site info <domain>` (alias `show`) | Show full details for one site |
| `vajra-site update php <domain> [version]` | Switch PHP version for a site |
| `vajra-site remove <domain> [--keep-files] [--keep-db]` | Tear down a site (alias `rm`) |
| `vajra-site help` (`-h`, `--help`, no args) | Show usage |

### `vajra-site add`

```
vajra-site add <domain> <email> [mode] [options]
```

**Mode flags** (mutually exclusive, default = `--wordpress`):

| Flag | Effect |
|---|---|
| `--wordpress` / `--wp` | Full WordPress install (DB + WP-CLI + cron) |
| `--php` | PHP-only site, no database |
| `--html` | Static HTML, no PHP, no database |

**WordPress options:**

| Option | Description |
|---|---|
| `--title "Site Title"` | WordPress site title (default: `WordPress`) |
| `--admin-user NAME` | WordPress admin username (default: `wpadmin`) |
| `--admin-pass PASS` | WordPress admin password (default: auto-generated) |

**Examples:**

```bash
# WordPress with defaults
vajra-site add blog.example.com admin@example.com

# WordPress with custom title + credentials
vajra-site add blog.example.com admin@example.com \
  --title "My Blog" \
  --admin-user alice \
  --admin-pass 'S3cret!Pass'

# PHP-only site (index.php welcome page generated)
vajra-site add app.example.com admin@example.com --php

# Static HTML site (index.html welcome page generated)
vajra-site add landing.example.com admin@example.com --html
```

### `vajra-site list`

```bash
vajra-site list
```

Output:

```
DOMAIN                         MODE         PORT   DB                     SVC    CREATED
blog.example.com               wordpress    8080   wp_blog_example_com    ✅     2026-10-13T09:12:44Z
app.example.com                php          8081   -                      ✅     2026-10-13T09:18:01Z
landing.example.com            html         8082   -                      ✅     2026-10-13T09:22:19Z
```

### `vajra-site info`

```bash
vajra-site info blog.example.com
```

Shows domain, mode, WWW alias, Vajra port, site root, PHP version/socket
(for `wordpress` and `php` modes), DB name/user/pass, admin user/pass,
service state, and MariaDB version.

### `vajra-site update php`

```bash
vajra-site update php <domain>            # interactive: pick from installed versions
vajra-site update php <domain> 8.5        # explicit target version
```

- Lists every PHP version installed on the host (SURY layout on Debian-family,
  Remi layout on RHEL-family, rolling on Arch).
- Verifies the target FPM socket is present before switching.
- Rewrites `PHP_VER` / `PHP_SOCK` in the site `.env` and the `upstream =`
  line in the site `.toml`, then restarts `vajra-site@<domain>.service`.
- **Rolls back automatically** if Vajra fails to restart.

> Refuses on `--html` mode sites (no PHP involved).

### `vajra-site remove`

```bash
vajra-site remove blog.example.com                       # full teardown
vajra-site remove blog.example.com --keep-files          # keep /var/www/sites/<domain>
vajra-site remove blog.example.com --keep-db             # keep MariaDB database + user
vajra-site remove blog.example.com --keep-files --keep-db
```

Removes (unless flagged otherwise): systemd unit, Nginx vhost, Let's Encrypt
cert, Vajra TOML, WP-Cron entry, MariaDB DB + user, site root, ACME webroot.

---

## Runtime management (systemd / logs)

```bash
# Restart a site's Vajra instance
systemctl restart vajra-site@DOMAIN.service

# Follow Vajra logs for one site
journalctl -u vajra-site@DOMAIN.service -f

# Vajra access log
tail -f /var/log/vajra/DOMAIN.access.log

# Restart PHP-FPM
systemctl restart php8.5-fpm            # Debian family
systemctl restart php-fpm               # RHEL / Arch

# Restart nginx
systemctl restart nginx

# Restart MariaDB
systemctl restart mariadb
```

---

## Architecture

```
Client ──▶ Nginx :443 (TLS) ──▶ Vajra :8080+N ──▶ PHP-FPM ──▶ MariaDB
                                        │
                                        └── static files under /var/www/sites/<domain>/
```

Each site gets its own:

* `/var/www/sites/<DOMAIN>/` — document root (WordPress / PHP / HTML)
* `/etc/vajra/sites/<DOMAIN>.toml` — Vajra config, auto-allocated port
* `/etc/vajra/sites/<DOMAIN>.env` — site metadata (chmod 600)
* `wp_<domain>` — MariaDB database + user (WordPress mode only)
* `vajra-site@<DOMAIN>.service` — systemd instance
* `/etc/nginx/conf.d/<DOMAIN>.conf` (RHEL/Arch) or
  `/etc/nginx/sites-available/<DOMAIN>.conf` (Debian)
* Let's Encrypt cert via `certbot --webroot`
* `/etc/cron.d/vajra-wp-cron-<domain>` — WP-Cron every 5 min (WordPress only)

---

## Environment variables

### Installer

| Var | Default | Purpose |
|---|---|---|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repo |
| `VAJRA_VERSION` | `latest` | Release tag or `latest` |
| `VAJRA_SKIP_BINARY` | `0` | Keep existing `/usr/local/bin/vajra` |
| `MARIADB_VERSION` | `mariadb-11.4` | MariaDB LTS series |
| `SITE_MODE` | `wordpress` | First-site mode: `wordpress`, `php`, `html` |

### Site runtime

None. Per-site config lives in `/etc/vajra/sites/<domain>.env` and
`/etc/vajra/sites/<domain>.toml`.

---

## Building from source

```bash
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server/vajra-phase7.1/vajra
cargo build --release --locked

# Portable musl build
rustup target add x86_64-unknown-linux-musl
apt install musl-tools
cargo build --release --locked --target x86_64-unknown-linux-musl
```

---

## Troubleshooting

### `sudo: command not found` during `vajra-site add`

Minimal installs do not ship `sudo`. The installer runs as root and calls
WP-CLI directly with `--allow-root`, so `sudo` is not needed.

### `ERR_TOO_MANY_REDIRECTS` on `/wp-admin/`

Vajra's FastCGI bridge does not forward `X-Forwarded-*` into PHP `$_SERVER`.
The generated `wp-config.php` forces `$_SERVER['HTTPS'] = 'on'`
unconditionally. If you see a loop on an older site, add this just above the
`/* That's all, stop editing! */` line:

```php
$_SERVER['HTTPS']       = 'on';
$_SERVER['SERVER_PORT'] = 443;
if (isset($_SERVER['HTTP_X_FORWARDED_HOST'])) {
    $_SERVER['HTTP_HOST'] = $_SERVER['HTTP_X_FORWARDED_HOST'];
}
```

### `io_uring_setup() → EPERM / ENOSYS`

The installer refuses to continue and prints a diagnostic box. Common causes:

* **RHEL / Alma / Rocky 8.x** — kernel 4.18, no io_uring backport. Use 9.x.
* **Hyper-V VPS** — host blocks `io_uring_setup` at the hypervisor level. Use
  a KVM-based VPS (Hetzner / DigitalOcean / Linode / Vultr KVM).
* **Docker / LXC** — seccomp filter blocks the syscall. Run on the host, or
  pass `--security-opt seccomp=unconfined`.
* **Kernel ≥ 6.6 with `kernel.io_uring_disabled = 2`** — the installer will try
  to flip it to `0` and persist it in `/etc/sysctl.d/99-enable-io-uring.conf`.
  If the write is rejected, the host is locked and cannot run Vajra.

Debug on the target:

```bash
uname -r
cat /proc/sys/kernel/io_uring_disabled
```

### Site shows `502 Bad Gateway`

Vajra is running but PHP-FPM is not, or the socket path in
`/etc/vajra/sites/<domain>.toml` does not match reality.

```bash
systemctl status php8.5-fpm        # Debian family
systemctl status php-fpm           # RHEL / Arch
grep upstream /etc/vajra/sites/DOMAIN.toml
```

Switch PHP versions cleanly with:

```bash
vajra-site update php DOMAIN 8.4
```

### Nginx config test fails after install

The installer auto-heals default-server conflicts by moving distro defaults
to `*.disabled-by-vajra`. If it still fails:

```bash
nginx -t
ls /etc/nginx/sites-enabled/ /etc/nginx/conf.d/
```

### Certificate issuance fails

`certbot` needs port 80 reachable from the public internet and DNS for the
domain (and `www.` alias) pointing at this server. Check:

```bash
dig +short DOMAIN
curl -I http://DOMAIN/.well-known/acme-challenge/test
```

---

## License

MIT — see [LICENSE](LICENSE).

## Support

* Issues: <https://github.com/sugan0927/vajra-server/issues>
* Security: see [SECURITY.md](SECURITY.md)
