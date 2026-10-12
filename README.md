# Vajra Web Server + WordPress One-Shot Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Debian%2012-red)](https://www.debian.org/)
[![PHP](https://img.shields.io/badge/PHP-8.5%20%7C%208.4-777BB4?logo=php&logoColor=white)](https://www.php.net/)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Security Policy](https://img.shields.io/badge/security-policy-blue)](SECURITY.md)

A complete WordPress stack on a fresh Debian 12 VPS in one command:
**Vajra + Nginx + PHP-FPM + MariaDB + Let's Encrypt + WordPress**

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- wp.example.com you@example.com
```

> Run as **root**. `sudo` is not required on a fresh Debian 12 VPS.

---

## 🚀 Quick Start

On any fresh **Debian 12** VPS (as root):

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- <domain> <email>
```

**Example:**

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash -s -- wp.aidoor.co.in sdsugans018@gmail.com
```

### Prerequisites

| Requirement | Details |
|-------------|---------|
| OS | Debian 12 (Bookworm) |
| Access | root |
| DNS | A record for the domain pointing to the VPS IP (set up beforehand) |
| Firewall | Ports **80** and **443** open (both at VPS and any CDN level) |
| Port 80 | Must be free (no other web server running) |
| glibc | ≥ 2.36 (Debian 12 ships 2.36 — see "GLIBC mismatch" below) |

> ⚠️ **Cloudflare users**: Keep the DNS record as **DNS only** (grey cloud) until the certificate is issued. Afterwards you may enable the proxy (orange cloud), but set **SSL/TLS mode = Full (strict)**.

---

## 📦 What It Installs

| Component | Version / Source |
|-----------|------------------|
| **Vajra** | GitHub Release **tarball** `vajra-<tag>-x86_64-linux.tar.gz` |
| **PHP-FPM** | 8.5 (SURY) — falls back to 8.4 automatically |
| **Nginx** | Debian bookworm default (1.22.x) |
| **MariaDB** | Debian bookworm default (10.11.x) |
| **Certbot** | Let's Encrypt standalone |
| **WordPress** | latest (wordpress.org) |

### PHP Extensions (all bundled)

`cli`, `fpm`, `mysql`, `curl`, `xml`, `mbstring`, `zip`, `gd`, `intl`, `bcmath`, `imagick`

---

## 🏗️ Architecture

```
┌──────────┐   HTTPS    ┌───────────┐   proxy    ┌──────────┐   FastCGI   ┌──────────┐
│  Client  │ ─────────► │   Nginx   │ ─────────► │  Vajra   │ ──────────► │ PHP-FPM  │
│ (browser)│    :443    │  (TLS)    │   :8080    │ (Rust)   │  (socket)   │ (8.4/8.5)│
└──────────┘            └───────────┘            └──────────┘             └──────────┘
                              │                       │                         │
                              │ 80 → 301 https        │ static + PHP            │
                              │                       │ /var/www/wordpress      │
                              └───────────────────────┴─────────────────────────┘
                                                      │
                                              ┌───────┴────────┐
                                              │    MariaDB     │
                                              │  (wordpress)   │
                                              └────────────────┘
```

- **Nginx** — TLS termination, reverse proxy, HTTP → HTTPS redirect
- **Vajra** — Rust web server: static files + FastCGI to PHP-FPM
- **PHP-FPM** — Executes WordPress
- **MariaDB** — WordPress database

---

## 📂 File Layout on VPS

```
/etc/vajra/
├── app.toml              # Vajra config
└── certs/
    ├── fullchain.pem     # Let's Encrypt cert
    └── privkey.pem       # private key

/etc/nginx/sites-available/vajra      # Nginx reverse proxy config
/etc/systemd/system/vajra.service      # systemd unit
/usr/local/bin/vajra                   # Vajra binary
/var/www/wordpress/                    # WordPress root
/var/log/vajra/access.log              # Vajra access log
/etc/letsencrypt/live/<domain>/        # Let's Encrypt certs
/etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh   # certbot → nginx reload
```

---

## 🔧 Post-Install

When the installer finishes, **DB credentials** are printed to the terminal:

```
════════════════════════════════════════════════════════
✅ Ready: https://<domain>/wp-admin/install.php
   DB Name : wordpress
   DB User : wpuser
   DB Pass : <random-strong-password>
════════════════════════════════════════════════════════
```

**Copy these and keep them safe.** Then complete the WordPress setup:

```
https://<domain>/wp-admin/install.php
```

### WordPress Setup Wizard

1. **Site Title** — name of your site
2. **Username** — use something unique instead of `admin` (e.g. `vajra_admin`)
3. **Password** — strong password
4. **Email** — your email
5. **Install WordPress**

### Login

```
https://<domain>/wp-login.php
```

---

## 🛠️ Common Commands

```bash
# Restart all services
sudo systemctl restart nginx vajra php8.4-fpm mariadb

# Logs
sudo journalctl -u vajra -f
sudo tail -f /var/log/vajra/access.log
sudo tail -f /var/log/nginx/error.log

# Certificate renewal (automatic, but manual test)
sudo certbot renew --dry-run

# WordPress DB backup
sudo mysqldump wordpress | gzip > /root/wp-db-$(date +%F).sql.gz

# Files backup
sudo tar czf /root/wp-files-$(date +%F).tar.gz /var/www/wordpress

# View DB password
sudo grep DB_PASSWORD /var/www/wordpress/wp-config.php
```

---

## 🐛 Troubleshooting

| Problem | Cause | Fix |
|---------|-------|-----|
| `Port 80 busy` | Another web server is running | `sudo systemctl stop nginx apache2` |
| `Text file busy` (Vajra) | Vajra already running | installer handles it automatically (auto-stop) |
| Certbot `Timeout during connect` | Firewall blocking | VPS dashboard + `ufw allow 80,443/tcp` |
| `HTTP/2 302` on install.php | WordPress already installed | Open `/wp-admin/` to log in |
| Browser `too many redirects` | Cloudflare SSL mode wrong | SSL/TLS → **Full (strict)** |
| `curl: (3) URL using bad/illegal format` | Invisible character in command | Type the entire command on one line |
| PHP-FPM socket missing | PHP install incomplete | `sudo systemctl status php8.4-fpm` |
| **`GLIBC_2.39 not found`** | CI-built binary from Ubuntu 24.04, target Debian 12 (glibc 2.36) | **Build from source — see below** |
| **Vajra binary 404 on step 3** | Wrong asset name (old installer) | Fixed in the current `install.sh` |

### GLIBC mismatch (most common failure)

The GitHub Actions workflow runs on `ubuntu-latest` (Ubuntu 24.04,
glibc **2.39**). Debian 12 ships glibc **2.36**. The pre-built binary
therefore prints:

```
vajra: /lib/x86_64-linux-gnu/libc.so.6: weak version `GLIBC_2.39' not found
```

**Fix — build Vajra from source on the target machine (~8 min):**

```bash
apt-get install -y build-essential pkg-config git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone https://github.com/sugan0927/vajra-server.git /opt/vajra-src
cd /opt/vajra-src/vajra-phase7.1/vajra
cargo build --release --locked
install -m 0755 target/release/vajra /usr/local/bin/vajra

/usr/local/bin/vajra --help       # must print version, no GLIBC warning
```

Then re-run the installer with `VAJRA_SKIP_BINARY=1` so it uses your
locally-built binary instead of the GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | VAJRA_SKIP_BINARY=1 bash -s -- <domain> <email>
```

**Better long-term fix — musl static binary:**

Switch the CI workflow to build a static musl binary so it runs on any
Linux regardless of glibc. Add to `.github/workflows/release.yml`:

```yaml
- name: Install musl toolchain
  run: |
    sudo apt-get install -y musl-tools
    rustup target add x86_64-unknown-linux-musl

- name: Build (static musl)
  run: cargo build --release --locked --target x86_64-unknown-linux-musl
```

Change the packaging step to copy
`target/x86_64-unknown-linux-musl/release/vajra` instead of the glibc
build. The resulting binary has **no dynamic glibc dependency**.

### Firewall Checklist

```bash
# VPS-level
sudo ufw allow 22/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw reload
```

If a cloud-level firewall group is attached (Vultr, Hetzner, etc.),
allow HTTP/HTTPS there too.

### Verify Installation

```bash
{
  echo "=== Services ==="
  systemctl is-active nginx vajra php8.4-fpm mariadb

  echo "=== Ports ==="
  ss -lntp | grep -E ':(80|443|8080)\b'

  echo "=== Vajra version ==="
  /usr/local/bin/vajra --help 2>&1 | head -1

  echo "=== HTTPS ==="
  curl -skI https://<domain>/ | head -2
} 2>&1
```

Expected: all services `active`, ports listening, no GLIBC warnings,
HTTPS `200`.

---

## 🔄 Re-running the Installer

The installer is **idempotent** and **safe to re-run**:

- PHP/Nginx configs are overwritten
- Let's Encrypt cert is reused via `--keep-until-expiring`
- WordPress files are only downloaded if missing
- Vajra binary is atomically replaced (`.new` → `mv`)
- **`wp-config.php` is preserved** — the existing DB password is read from it and re-used (as of the latest installer)

> ⚠️ Older versions of the installer regenerated `wp-config.php` every
> time, changing the DB password. If you have an older installer, back up first:
>
> ```bash
> sudo cp /var/www/wordpress/wp-config.php /root/wp-config.backup
> ```

---

## 🌐 Environment Variables

| Var | Default | Purpose |
|-----|---------|---------|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repo for binary release |
| `VAJRA_VERSION` | `latest` | Release tag or `latest` |
| `VAJRA_SKIP_BINARY` | `0` | Set to `1` to skip binary download and use `/usr/local/bin/vajra` as-is |

**Example — pin a specific version:**

```bash
VAJRA_VERSION=v0.14.0 curl -fsSL .../install.sh | bash -s -- <domain> <email>
```

**Example — use a locally-built binary:**

```bash
# build first (see "GLIBC mismatch")
curl -fsSL .../install.sh | VAJRA_SKIP_BINARY=1 bash -s -- <domain> <email>
```

---

## 🏗️ Building Vajra from Source

```bash
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server/vajra-phase7.1/vajra

# Debug build (fast, big)
cargo build

# Release build (glibc — matches target machine)
cargo build --release --locked
ls -la target/release/vajra

# Release build (static musl — portable across distros)
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl
ls -la target/x86_64-unknown-linux-musl/release/vajra
```

Install:

```bash
sudo install -m 0755 target/release/vajra /usr/local/bin/vajra
sudo systemctl restart vajra
```

---

## 📜 License

MIT — full text in the [LICENSE](LICENSE) file.

---

## 🤝 Contributing

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/xyz`)
3. Commit your changes (`git commit -am 'Add xyz'`)
4. Push the branch (`git push origin feature/xyz`)
5. Open a Pull Request

---

## 📮 Support

- **Issues**: [GitHub Issues](https://github.com/sugan0927/vajra-server/issues)
- **Security**: See [SECURITY.md](SECURITY.md)
- **Email**: sdsugans018@gmail.com

---

**Made with ❤️ for the self-hosted WordPress community.**