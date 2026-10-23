# Vajra Web Server: Complete Documentation

**Developed by Sugan Dodrai**
Repository: <https://github.com/sugan0927/vajra-server>
License: MIT

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Key Features](#2-key-features)
3. [Requirements](#3-requirements)
4. [Supported Distributions](#4-supported-distributions)
5. [Installation](#5-installation)
6. [What the Installer Does](#6-what-the-installer-does)
7. [Architecture](#7-architecture)
8. [Directory and File Layout](#8-directory-and-file-layout)
9. [Site Management with `vajra-site`](#9-site-management-with-vajra-site)
10. [Core CLI: `vajra`](#10-core-cli-vajra)
11. [Configuration Reference (`app.toml`)](#11-configuration-reference-apptoml)
12. [Runtime Management](#12-runtime-management)
13. [WordPress Notes](#13-wordpress-notes)
14. [PHP Version Management](#14-php-version-management)
15. [TLS, ACME and HTTP/3](#15-tls-acme-and-http3)
16. [Security Best Practices](#16-security-best-practices)
17. [Performance Tuning](#17-performance-tuning)
18. [Backup and Restore](#18-backup-and-restore)
19. [Upgrading](#19-upgrading)
20. [Uninstalling](#20-uninstalling)
21. [Building from Source](#21-building-from-source)
22. [Environment Variables](#22-environment-variables)
23. [Troubleshooting](#23-troubleshooting)
24. [FAQ](#24-faq)
25. [Contributing, License and Credits](#25-contributing-license-and-credits)

---

## 1. Introduction

**Vajra** is a fast, memory-safe, single-binary web server written in Rust. It replaces the traditional Nginx + Certbot + ModSecurity stack with one cohesive binary that terminates TLS, obtains and renews Let's Encrypt certificates, serves HTTP/1.1, HTTP/2 and HTTP/3, compresses responses, adds security headers, serves static files, and talks FastCGI to PHP-FPM.

The **Vajra Multi-Site Installer** turns one VPS into a multi-site host. One command installs the stack (Vajra, PHP-FPM, MariaDB, WP-CLI), and one command per site creates a WordPress, PHP, or static HTML website.

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
vajra-site add blog.example.com you@example.com
```

The project has two parts:

| Component | Purpose |
|---|---|
| `vajra` | The Rust web server binary at `/usr/local/bin/vajra`. It listens on ports 80 and 443 directly. |
| `vajra-site` | The site manager CLI at `/usr/local/bin/vajra-site`. It automates database, WP-CLI, PHP-FPM pool, and Vajra site configuration. |

---

## 2. Key Features

* **Single binary:** no Nginx, no Certbot, no Apache.
* **Native ACME:** automatically provisions and renews Let's Encrypt certificates.
* **HTTP/3 (QUIC):** next-generation transport enabled alongside HTTP/2 and HTTP/1.1.
* **Native Brotli and Gzip:** on-the-fly compression for HTTP/1.1, HTTP/2 and HTTP/3.
* **io_uring powered:** uses Linux asynchronous I/O for high throughput and zero-copy static file serving.
* **PHP-FPM support:** native FastCGI integration with a built-in FastCGI cache.
* **Security headers:** HSTS, CSP, X-Frame-Options, X-Content-Type-Options, and more, configured natively.
* **Multi-site:** host many domains from a single Vajra instance, each as a `[[site]]` block.
* **One-command setup:** WordPress, PHP-only, and static HTML sites via `vajra-site add`.
* **Safe PHP switching:** change a site's PHP version with automatic rollback on failure.
* **Portable binary:** statically linked musl build, independent of the host glibc version, with a source-build fallback.

---

## 3. Requirements

| Requirement | Details |
|---|---|
| Operating system | Linux (see [supported distributions](#4-supported-distributions)) |
| Kernel | **5.1 or newer** (required for `io_uring`) |
| Init system | **systemd** |
| CPU architecture | **x86_64** (the installer targets x86_64) |
| Privileges | Root |
| Network | Ports **80/tcp** (ACME HTTP-01 and redirects), **443/tcp** (HTTPS) and **443/udp** (HTTP/3) reachable from the internet |
| DNS | A/AAAA records for each domain pointing at the server |
| Virtualization | KVM-based VPS recommended (Hetzner, DigitalOcean, Linode, Vultr KVM) |

**Not supported:**

* RHEL, AlmaLinux and Rocky Linux **8.x** (kernel 4.18, no `io_uring` backport).
* Hyper-V based VPS hosts that block `io_uring_setup` at the hypervisor.
* Containers whose seccomp profile blocks `io_uring`. Docker must run with `--security-opt seccomp=unconfined`.

Because Vajra binds ports 80 and 443 itself, no other web server (Nginx, Apache, Caddy) may be listening on them.

---

## 4. Supported Distributions

| Family | Distro | Versions | Package manager | PHP source |
|---|---|---|---|---|
| Debian | Debian | 11, 12, 13 | `apt` | SURY |
| Debian | Ubuntu | 20.04, 22.04, 24.04, 26.04 | `apt` | SURY |
| Debian | Linux Mint | 20, 21, 22 | `apt` | SURY |
| Debian | Pop!_OS | 22.04+ | `apt` | SURY |
| Debian | Kali | rolling | `apt` | SURY |
| Debian | Raspbian | 11, 12 | `apt` | SURY |
| RHEL | RHEL | 9.x | `dnf` | Remi |
| RHEL | AlmaLinux | 9.x | `dnf` | Remi |
| RHEL | Rocky Linux | 9.x | `dnf` | Remi |
| RHEL | Oracle Linux | 9.x | `dnf` | Remi |
| RHEL | CentOS Stream | 9, 10 | `dnf` | Remi |
| RHEL | Fedora | 38, 39, 40, 41+ | `dnf` | Fedora main |
| Arch | Arch Linux | rolling | `pacman` | main repo |
| Arch | Manjaro | rolling | `pacman` | main repo |
| Arch | EndeavourOS | rolling | `pacman` | main repo |
| Arch | Garuda Linux | rolling | `pacman` | main repo |

The installer reads `/etc/os-release`, detects the family, and selects the package manager and PHP source automatically.

---

## 5. Installation

### 5.1 One-liner (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
```

This installs the stack only. No site is created.

### 5.2 One-liner with a first site

```bash
# WordPress (default)
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- blog.example.com admin@example.com --wordpress

# PHP-only
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- api.example.com admin@example.com --php

# Static HTML
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- landing.example.com admin@example.com --html
```

### 5.3 Download and run

```bash
curl -fsSLO https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh

bash install.sh                                        # stack only
bash install.sh example.com you@example.com            # + first WordPress site
bash install.sh example.com you@example.com --php      # + first PHP site
bash install.sh example.com you@example.com --html     # + first static HTML site
bash install.sh example.com you@example.com --wordpress
bash install.sh example.com you@example.com --wp       # shorthand
```

### 5.4 Pinning versions

```bash
VAJRA_VERSION=v0.7.1 MARIADB_VERSION=mariadb-10.11 \
  bash install.sh example.com you@example.com --php
```

### 5.5 Pre-install checklist

1. DNS A/AAAA records for the domain (and `www.` alias) point to the server.
2. Ports 80/tcp, 443/tcp and 443/udp are open in the cloud firewall and on the host.
3. Nothing else is listening on ports 80 or 443.
4. You are root.
5. Kernel is 5.1 or newer (`uname -r`).
6. The server is KVM-based, not Hyper-V, and not a restricted container.

---

## 6. What the Installer Does

1. **Pre-flight:** detects the distro, verifies kernel 5.1 or newer, frees ports 80 and 443, and opens the firewall.
2. **io_uring check:** compiles and runs a tiny probe. It refuses to continue on RHEL 8, Hyper-V, and seccomp-locked containers, and prints a diagnostic.
3. **Base packages:** installs MariaDB 11.4 (through `mariadb_repo_setup`), curl, openssl, and python3.
4. **PHP:** installs PHP-FPM from SURY (Debian family), Remi (RHEL family) or the main repo (Arch). It prefers PHP 8.5 and falls back to 8.4.
5. **Vajra binary:** downloads the musl tarball from the GitHub release. If none exists for the tag, it builds from source.
6. **WP-CLI:** installs `/usr/local/bin/wp` if missing.
7. **Directories and service:** creates `/etc/vajra`, `/etc/vajra/certs`, `/var/log/vajra`, `/var/www/sites`, and the `vajra.service` systemd unit.
8. **Fallback certificate:** generates a self-signed fallback certificate so Vajra can start before ACME issues real certificates.
9. **`vajra-site` CLI:** installs `/usr/local/bin/vajra-site`.
10. **Post-install verification:** checks MariaDB, PHP-FPM, Vajra, listening ports, and configuration validity (`vajra --check`).

If you pass a domain and email, the installer then runs `vajra-site add` for you.

---

## 7. Architecture

Vajra does not sit behind Nginx. It binds directly to ports 80 and 443.

```text
Client Request
      │
      ▼
[ Vajra :443 (Rust) ]
      ├── HTTP/3 (QUIC), HTTP/2 or HTTP/1.1
      ├── TLS termination (rustls)
      ├── Native ACME challenge (HTTP-01 on :80)
      ├── Brotli / Gzip compression
      ├── Security header injection
      │
      ├──> Static files (zero-copy I/O)
      │
      └──> FastCGI proxy
             │
             ▼
         [ PHP-FPM pool ] ──> [ MariaDB ]
```

* **One service, many sites:** a single `vajra.service` serves every domain defined by a `[[site]]` block in `/etc/vajra/app.toml`.
* **Static content** (HTML, CSS, JS, images) is served directly by Vajra.
* **Dynamic content** (PHP, WordPress) is passed to PHP-FPM over a Unix socket using FastCGI, optionally through the FastCGI cache.
* **MariaDB** holds WordPress data. Each WordPress site has its own database and user.
* **Certificates** are obtained and renewed by Vajra itself. A fallback certificate keeps the server online if issuance or renewal fails.

---

## 8. Directory and File Layout

| Path | Purpose |
|---|---|
| `/usr/local/bin/vajra` | The Vajra server binary |
| `/usr/local/bin/vajra-site` | The site manager CLI |
| `/usr/local/bin/wp` | WP-CLI |
| `/etc/vajra/app.toml` | Main configuration (server, TLS, ACME, headers, compression, cache, sites) |
| `/etc/vajra/certs/fallback.crt`, `fallback.key` | Fallback TLS certificate. **Never delete.** |
| `/var/www/sites/<domain>/` | Site document root |
| `/var/log/vajra/access.log` | HTTP access log |
| `/var/cache/vajra/fastcgi/` | FastCGI cache directory |
| `/etc/php/<ver>/fpm/pool.d/` | PHP-FPM pools, including per-site pools (Debian family) |
| `/run/php/` | PHP-FPM sockets, for example `vajra-example_com.sock` |
| `/etc/systemd/system/vajra.service` | systemd unit for Vajra |
| `/etc/cron.d/vajra-wp-cron-<domain>` | WP-Cron job for a WordPress site |

> Paths for PHP-FPM differ by distro family (SURY on Debian, Remi on RHEL, rolling on Arch). `vajra-site` accounts for this automatically.

---

## 9. Site Management with `vajra-site`

`vajra-site` is a wrapper script at `/usr/local/bin/vajra-site` that automates site setup: document root, WordPress, PHP-FPM pool, database, cron, and the matching `[[site]]` block in Vajra's configuration.
sudo systemctl restart vajra
sleep 2
sudo systemctl is-active vajra

### 9.1 Synopsis

```
vajra-site <command> [arguments] [options]
```

### 9.2 Commands

| Command | Description |
|---|---|
| `vajra-site add <domain> <email> [mode] [opts]` | Create a new site (default: WordPress) |
| `vajra-site list` (alias `ls`) | List all sites |
| `vajra-site info <domain>` (alias `show`) | Show full details for one site |
| `vajra-site update php <domain> [version]` | Switch the PHP version for a site |
| `vajra-site remove <domain> [--keep-files] [--keep-db]` (alias `rm`) | Tear down a site |
| `vajra-site help` (`-h`, `--help`, or no arguments) | Show usage |

### 9.3 `vajra-site add`

```
vajra-site add <domain> <email> [mode] [options]
```

**Mode flags** (mutually exclusive; default is `--wordpress`):

| Flag | Effect |
|---|---|
| `--wordpress` / `--wp` | Installs WordPress, sets up the database, and configures WP-Cron |
| `--php` | Sets up a PHP-FPM pool and an `index.php`; no database |
| `--html` | Sets up a static directory and an `index.html`; no PHP, no database |

**WordPress options:**

| Option | Description |
|---|---|
| `--title "Site Title"` | WordPress site title (default: `WordPress`) |
| `--admin-user NAME` | Admin username (default: `wpadmin`) |
| `--admin-pass PASS` | Admin password (default: auto-generated) |

**Examples:**

```bash
# WordPress with defaults
vajra-site add blog.example.com admin@example.com

# WordPress with custom title and credentials
vajra-site add blog.example.com admin@example.com \
  --title "My Blog" --admin-user alice --admin-pass 'S3cret!Pass'

# PHP-only site
vajra-site add app.example.com admin@example.com --php

# Static HTML site
vajra-site add landing.example.com admin@example.com --html
```

Once the site is added and DNS points at the server, Vajra's native ACME obtains the certificate automatically using the email you supplied.

### 9.4 `vajra-site list`

```bash
vajra-site list
```

Prints each site with its mode, database, and state.

### 9.5 `vajra-site info`

```bash
vajra-site info blog.example.com
```

Shows the domain, mode, document root, PHP version and socket (WordPress and PHP modes), database name and user, admin user, and service state.

> The output may contain credentials. Do not paste it into public places.

### 9.6 `vajra-site update php`

```bash
vajra-site update php <domain>          # choose from installed versions
vajra-site update php <domain> 8.5      # explicit version
```

* Lists installed PHP versions.
* Verifies the target FPM socket exists before switching.
* Updates the site's `php_sock` and restarts Vajra to apply the change.
* Rolls back automatically if Vajra fails to restart.
* Refuses on `--html` sites.

### 9.7 `vajra-site remove`

```bash
vajra-site remove blog.example.com                         # full teardown
vajra-site remove blog.example.com --keep-files            # keep /var/www/sites/<domain>
vajra-site remove blog.example.com --keep-db               # keep database and user
vajra-site remove blog.example.com --keep-files --keep-db
```

Unless flagged otherwise, removal deletes the site's configuration block, PHP-FPM pool, WP-Cron entry, database and user, and document root. Back up first (see [section 18](#18-backup-and-restore)).

---

## 10. Core CLI: `vajra`

```bash
vajra --version                              # print version
vajra --check --config /etc/vajra/app.toml   # validate configuration without starting
vajra --config /etc/vajra/app.toml           # run in the foreground (normally systemd does this)
```

Always run `--check` before restarting after a manual edit. A syntax error in the TOML will stop the server from starting.

---

## 11. Configuration Reference (`app.toml`)

All server configuration lives in one TOML file: `/etc/vajra/app.toml`. After any change:

```bash
vajra --check --config /etc/vajra/app.toml && systemctl restart vajra
```

> Option names are documented for the Vajra releases this project targets. Run `vajra --version` and consult the repository for the release you installed if something is rejected.

### 11.1 Server and logging

```toml
[server]
listen     = "0.0.0.0:443"
workers    = 1
grace_secs = 5

[logging]
access_log = "/var/log/vajra/access.log"
format     = "text"       # or "json"
```

| Key | Meaning |
|---|---|
| `listen` | Address and port for the main listener |
| `workers` | Number of worker threads |
| `grace_secs` | Seconds allowed for in-flight requests during shutdown or restart |
| `access_log` | Path to the access log |
| `format` | `text` or `json` log lines |

### 11.2 TLS and QUIC (HTTP/3)

Vajra needs a fallback certificate to start safely before ACME issues the real one.

```toml
[tls]
listen = "0.0.0.0:443"
cert   = "/etc/vajra/certs/fallback.crt"
key    = "/etc/vajra/certs/fallback.key"

[quic]
listen = "0.0.0.0:443"    # HTTP/3 over UDP
```

Open **443/udp** in your firewall for HTTP/3 to work.

### 11.3 Native ACME (Let's Encrypt)

```toml
[acme]
enabled = true
email   = "admin@example.com"
staging = false
```

Vajra requests certificates through HTTP-01 challenges on port 80. Set `staging = true` while testing to avoid Let's Encrypt rate limits.

### 11.4 Compression

```toml
[compression]
enabled      = true
gzip         = true
gzip_level   = 6
brotli       = true
brotli_level = 5
min_size     = 1024
```

Responses smaller than `min_size` bytes are not compressed. Brotli is preferred for clients that support it.

### 11.5 Security headers

```toml
[headers]
hsts                    = "max-age=31536000; includeSubDomains; preload"
x_frame_options         = "SAMEORIGIN"
x_content_type_options  = "nosniff"
content_security_policy = "upgrade-insecure-requests; default-src 'self' https: data: 'unsafe-inline' 'unsafe-eval';"
```

Define `[headers]` only once. A duplicate block makes the TOML invalid.

### 11.6 Multi-site configuration

Each site needs a `[[site]]` block. `vajra-site` writes these for you.

```toml
[[site]]
host     = "example.com"
root     = "/var/www/sites/example.com"
index    = "index.php"
mode     = "php"                                  # "wordpress", "php" or "html"
php_sock = "unix:/run/php/vajra-example_com.sock"
```

| Key | Meaning |
|---|---|
| `host` | Domain served by this block |
| `root` | Document root |
| `index` | Default index file |
| `mode` | `wordpress`, `php` or `html` |
| `php_sock` | PHP-FPM socket (omit for `html` sites) |

### 11.7 FastCGI cache

Speeds up PHP and WordPress by caching dynamic responses.

```toml
[fastcgi_cache]
enabled          = true
path             = "/var/cache/vajra/fastcgi"
max_size_mb      = 512
default_ttl_secs = 3600
exclude_paths    = ["/wp-admin/*", "/wp-login.php", "/wp-json/*"]
bypass_cookie    = ["woocommerce_items_in_cart", "wp_woocommerce_session_", "comment_author_"]
```

Admin, login and REST paths are never cached, and requests carrying the listed cookies (for example, shoppers with a cart) bypass the cache.

### 11.8 Full example

```toml
[server]
listen     = "0.0.0.0:443"
workers    = 1
grace_secs = 5

[logging]
access_log = "/var/log/vajra/access.log"
format     = "text"

[tls]
listen = "0.0.0.0:443"
cert   = "/etc/vajra/certs/fallback.crt"
key    = "/etc/vajra/certs/fallback.key"

[quic]
listen = "0.0.0.0:443"

[acme]
enabled = true
email   = "admin@example.com"
staging = false

[compression]
enabled      = true
gzip         = true
gzip_level   = 6
brotli       = true
brotli_level = 5
min_size     = 1024

[headers]
hsts                    = "max-age=31536000; includeSubDomains; preload"
x_frame_options         = "SAMEORIGIN"
x_content_type_options  = "nosniff"
content_security_policy = "upgrade-insecure-requests; default-src 'self' https: data: 'unsafe-inline' 'unsafe-eval';"

[fastcgi_cache]
enabled          = true
path             = "/var/cache/vajra/fastcgi"
max_size_mb      = 512
default_ttl_secs = 3600
exclude_paths    = ["/wp-admin/*", "/wp-login.php", "/wp-json/*"]
bypass_cookie    = ["woocommerce_items_in_cart", "wp_woocommerce_session_", "comment_author_"]

[[site]]
host     = "blog.example.com"
root     = "/var/www/sites/blog.example.com"
index    = "index.php"
mode     = "wordpress"
php_sock = "unix:/run/php/vajra-blog_example_com.sock"

[[site]]
host  = "landing.example.com"
root  = "/var/www/sites/landing.example.com"
index = "index.html"
mode  = "html"
```

---

## 12. Runtime Management

Vajra runs as one service, `vajra.service`, handling every domain in `app.toml`.

```bash
# Apply configuration changes
systemctl restart vajra

# Status, start, stop
systemctl status vajra
systemctl start vajra
systemctl stop vajra

# Start on boot
systemctl enable vajra

# Application logs (ACME, errors, system events)
journalctl -u vajra -f

# HTTP access log
tail -f /var/log/vajra/access.log

# PHP-FPM
systemctl restart php8.5-fpm        # Debian family
systemctl restart php-fpm           # RHEL / Arch

# MariaDB
systemctl restart mariadb
```

### Useful checks

```bash
# Validate configuration
vajra --check --config /etc/vajra/app.toml

# Confirm Vajra owns ports 80 and 443
ss -ltnp | grep -E ':(80|443)\b'
ss -lunp | grep ':443'              # HTTP/3 (UDP)

# Health of the stack
systemctl is-active vajra mariadb php8.5-fpm
```

---

## 13. WordPress Notes

### 13.1 HTTPS detection

Vajra's FastCGI bridge does not forward `X-Forwarded-*` into PHP `$_SERVER`, so WordPress could otherwise think it is running on plain HTTP and redirect endlessly. The generated `wp-config.php` forces HTTPS unconditionally.

### 13.2 WP-Cron

For WordPress sites, `/etc/cron.d/vajra-wp-cron-<domain>` triggers WP-Cron from system cron, which is more reliable than page-load-triggered cron.

### 13.3 Using WP-CLI

Run as root with `--allow-root` and point at the site path:

```bash
wp --allow-root --path=/var/www/sites/blog.example.com plugin list
wp --allow-root --path=/var/www/sites/blog.example.com user list
wp --allow-root --path=/var/www/sites/blog.example.com core update
```

### 13.4 Fixing mixed content

If WordPress loads `http://` assets:

```bash
wp --allow-root --path=/var/www/sites/DOMAIN \
  search-replace 'http://DOMAIN' 'https://DOMAIN' --all-tables
```

Also make sure `content_security_policy` with `upgrade-insecure-requests` is active in `app.toml`.

### 13.5 Cache and WooCommerce

The default `exclude_paths` and `bypass_cookie` values in `[fastcgi_cache]` are tuned for WordPress and WooCommerce. If you use another membership or e-commerce plugin, add its session cookie names to `bypass_cookie`.

---

## 14. PHP Version Management

* The installer installs PHP 8.5 when available and falls back to 8.4.
* Install additional versions from your distro's PHP repository, then switch a site with `vajra-site update php`.
* Each site's PHP version is independent, because each `[[site]]` has its own `php_sock`.
* If the target FPM socket is missing, the switch is refused. If Vajra fails to restart afterwards, the change is rolled back.

```bash
vajra-site update php blog.example.com          # interactive
vajra-site update php blog.example.com 8.4      # explicit
vajra-site info blog.example.com                # confirm result
```

---

## 15. TLS, ACME and HTTP/3

### 15.1 How certificates are issued

1. A site is added and DNS points at the server.
2. Vajra requests a certificate from Let's Encrypt using the HTTP-01 challenge on port 80.
3. The issued certificate is used for that host. Renewal is automatic.
4. If issuance or renewal fails, the fallback certificate keeps the server online (with a browser warning) so nothing crashes.

### 15.2 Requirements

* DNS for the domain points to this server's public IP.
* Port 80 reachable from the internet (`ufw allow 80/tcp`).
* `[acme] enabled = true` with a valid email.

### 15.3 Checking status

```bash
journalctl -u vajra -f | grep -i acme
dig +short yourdomain.com
```

### 15.4 HTTP/3

Enable the `[quic]` listener and open **443/udp**:

```bash
ufw allow 443/udp
```

Browsers discover HTTP/3 automatically after a first HTTP/2 or HTTP/1.1 visit.

---

## 16. Security Best Practices

1. **HSTS:** set it with `preload`, and submit your domain to the HSTS preload list once every subdomain supports HTTPS.
2. **CSP:** Vajra sets `Content-Security-Policy` natively, which helps against XSS and mixed content. The permissive example in this document is a starting point; tighten it per site.
3. **Fallback certificates:** never delete `/etc/vajra/certs/fallback.crt`. If ACME renewal fails, it keeps the server online.
4. **Firewall:** allow only 22, 80, 443/tcp and 443/udp publicly. MariaDB should listen on localhost only.
5. **Updates:** keep the OS, PHP, WordPress core, themes and plugins updated; remove unused plugins.
6. **WordPress admin:** set `--admin-user` and a strong `--admin-pass` at creation time.
7. **SSH:** use keys and disable password login.
8. **Backups:** schedule them and test restores.
9. **Logs:** review `/var/log/vajra/access.log` and `journalctl -u vajra` regularly.

---

## 17. Performance Tuning

* **Workers:** raise `[server] workers` to match the number of CPU cores on busy servers.
* **Compression:** Brotli level 5 and Gzip level 6 balance CPU and size. Lower levels for CPU-bound hosts, raise them for bandwidth-bound hosts.
* **FastCGI cache:** increase `max_size_mb` and `default_ttl_secs` for mostly-static content. Keep admin and cart paths excluded.
* **PHP-FPM:** tune `pm.max_children` and memory limits to your RAM.
* **MariaDB:** size `innodb_buffer_pool_size` to roughly 50 to 70 percent of RAM on a dedicated database host, and less on shared VPS setups.
* **Kernel:** a recent kernel gives `io_uring` the best performance.

---

## 18. Backup and Restore

### Back up one WordPress site

```bash
DOMAIN=blog.example.com
STAMP=$(date +%F)
mkdir -p /root/backups

tar -czf /root/backups/${DOMAIN}-files-${STAMP}.tar.gz -C /var/www/sites ${DOMAIN}

wp --allow-root --path=/var/www/sites/${DOMAIN} db export \
  /root/backups/${DOMAIN}-db-${STAMP}.sql

cp /etc/vajra/app.toml /root/backups/app.toml.${STAMP}
```

### Restore

```bash
tar -xzf /root/backups/${DOMAIN}-files-${STAMP}.tar.gz -C /var/www/sites
wp --allow-root --path=/var/www/sites/${DOMAIN} db import \
  /root/backups/${DOMAIN}-db-${STAMP}.sql
systemctl restart vajra
```

### Scheduling

Add a daily job under `/etc/cron.d/` and copy archives off the server.

---

## 19. Upgrading

Re-run the installer to replace the binary; sites and configuration stay in place:

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
# or a specific release
VAJRA_VERSION=v0.7.1 bash install.sh
```

Then validate and restart:

```bash
vajra --check --config /etc/vajra/app.toml && systemctl restart vajra
```

Keep the current binary while re-running the installer:

```bash
VAJRA_SKIP_BINARY=1 bash install.sh
```

---

## 20. Uninstalling

```bash
vajra-site list
vajra-site remove DOMAIN              # repeat for each site

systemctl disable --now vajra
rm -f /usr/local/bin/vajra /usr/local/bin/vajra-site
rm -f /etc/systemd/system/vajra.service && systemctl daemon-reload
rm -rf /etc/vajra /var/log/vajra /var/cache/vajra
```

Remove MariaDB and PHP with your package manager only if nothing else uses them.

---

## 21. Building from Source

```bash
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server/vajra-phase7.1/vajra
cargo build --release --locked
```

Portable musl build:

```bash
rustup target add x86_64-unknown-linux-musl
apt install musl-tools
cargo build --release --locked --target x86_64-unknown-linux-musl
```

Requires Rust 1.75 or newer.

---

## 22. Environment Variables

### Installer

| Variable | Default | Purpose |
|---|---|---|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repository to fetch the binary from |
| `VAJRA_VERSION` | `latest` | Release tag, or `latest` to auto-resolve |
| `VAJRA_SKIP_BINARY` | `0` | `1` keeps the existing `/usr/local/bin/vajra` |
| `MARIADB_VERSION` | `mariadb-11.4` | MariaDB LTS series to configure |
| `SITE_MODE` | `wordpress` | Default first-site mode: `wordpress`, `php`, or `html` |

### Runtime

None. Vajra is configured entirely through `/etc/vajra/app.toml`.

---

## 23. Troubleshooting

### `io_uring_setup() failed` (EPERM / ENOSYS)

Vajra requires modern kernel I/O. Common causes:

* **Old kernel:** upgrade to 5.1 or newer.
* **RHEL, Alma, Rocky 8.x:** unsupported (kernel 4.18). Use 9.x.
* **Hyper-V VPS:** the host blocks the syscall. Use a KVM-based VPS.
* **Docker or LXC:** run with `--security-opt seccomp=unconfined`, or run on the host.
* **Kernel 6.6 or newer with `kernel.io_uring_disabled = 2`:** the installer tries to set it to `0` and persists that in `/etc/sysctl.d/99-enable-io-uring.conf`. If the write is rejected, the host is locked.

```bash
uname -r
cat /proc/sys/kernel/io_uring_disabled
```

### `502 Bad Gateway`

Vajra cannot reach PHP-FPM.

```bash
systemctl status php8.5-fpm
grep php_sock /etc/vajra/app.toml
ls /run/php/
ls /etc/php/8.5/fpm/pool.d/
```

The `php_sock` path in `app.toml` must match the socket defined in the pool file. Switch versions cleanly with `vajra-site update php DOMAIN 8.4`.

### `TOML parse error` or Vajra will not start

```bash
vajra --check --config /etc/vajra/app.toml
```

Look for duplicate `[headers]` blocks, missing quotes, or unescaped characters.

### Port 80 or 443 already in use

Another web server is running (Nginx, Apache, Caddy).

```bash
ss -ltnp | grep -E ':(80|443)\b'
systemctl disable --now nginx apache2 httpd 2>/dev/null
systemctl restart vajra
```

### ACME certificate not issuing

* **DNS:** `dig +short yourdomain.com` must return this server's IP.
* **Port 80:** must be open (`ufw allow 80/tcp`) and reachable from the internet.
* **Logs:** `journalctl -u vajra -f | grep -i acme`
* **Rate limits:** use `staging = true` while testing, then switch back.

### HTTP/3 not working

* Confirm `[quic]` is configured and **443/udp** is open on the host and cloud firewall.
* Check with `curl --http3 -I https://yourdomain.com` (needs an HTTP/3-capable curl).

### `ERR_TOO_MANY_REDIRECTS` on `/wp-admin/`

The generated `wp-config.php` forces HTTPS. On an older site, add this just above `/* That's all, stop editing! */`:

```php
$_SERVER['HTTPS']       = 'on';
$_SERVER['SERVER_PORT'] = 443;
if (isset($_SERVER['HTTP_X_FORWARDED_HOST'])) {
    $_SERVER['HTTP_HOST'] = $_SERVER['HTTP_X_FORWARDED_HOST'];
}
```

### WordPress mixed content

```bash
wp --allow-root --path=/var/www/sites/DOMAIN \
  search-replace 'http://DOMAIN' 'https://DOMAIN' --all-tables
```

Also confirm `content_security_policy` is active in `app.toml`.

### `sudo: command not found`

Minimal installs do not ship `sudo`. The installer runs as root and calls WP-CLI with `--allow-root`, so `sudo` is not needed.

### WordPress cannot connect to the database

```bash
systemctl status mariadb
vajra-site info DOMAIN
```

Compare the database name, user and password with `wp-config.php`.

---

## 24. FAQ

**Do I need Nginx or Certbot?**
No. Vajra handles TLS, ACME certificates, HTTP/3 and proxying to PHP-FPM itself.

**Can I host more than one site?**
Yes. Run `vajra-site add` once per domain; each adds a `[[site]]` block.

**Can I mix WordPress, PHP and static sites?**
Yes. The mode is chosen per site.

**Do I need to compile anything?**
No. The installer downloads a static musl binary and builds from source only if no release exists for the tag.

**Does it work on ARM?**
The installer currently targets x86_64.

**Which PHP version is used?**
8.5 when available, otherwise 8.4, changeable per site with `vajra-site update php`.

**What happens if a certificate renewal fails?**
Vajra keeps serving using the fallback certificate until renewal succeeds.

---

## 25. Contributing, License and Credits

* **Issues:** <https://github.com/sugan0927/vajra-server/issues>
* **Security reports:** see `SECURITY.md` in the repository.
* **Contributions:** pull requests are welcome. Describe the problem, test on at least one distro per supported family, and keep changes focused.

**License:** MIT. See `LICENSE`.

**Developed by Sugan Dodrai.**
