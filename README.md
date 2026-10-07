# Vajra Web Server + WordPress One-Shot Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Debian%2012-red)](https://www.debian.org/)
[![PHP](https://img.shields.io/badge/PHP-8.5%20%7C%208.4-777BB4?logo=php&logoColor=white)](https://www.php.net/)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Security Policy](https://img.shields.io/badge/security-policy-blue)](SECURITY.md)

A complete WordPress stack on a fresh Debian 12 VPS in one command:
**Vajra + Nginx + PHP-FPM + MariaDB + Let's Encrypt + WordPress**

```bash
curl -fsSL https://get.aidoor.co.in/vajra | sudo bash -s -- wp.example.com you@example.com
```

---

## 🚀 Quick Start

On any fresh **Debian 12** VPS (as root):

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | sudo bash -s -- <domain> <email>
```

**Example:**

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | sudo bash -s -- wp.aidoor.co.in sdsugans018@gmail.com
```

### Prerequisites

| Requirement | Details |
|-------------|---------|
| OS | Debian 12 (Bookworm) |
| Access | root / sudo |
| DNS | A record for the domain pointing to the VPS IP (set up beforehand) |
| Firewall | Ports **80** and **443** open (both at Vultr and Cloudflare level) |
| Port 80 | Must be free (no other web server running) |

> ⚠️ **Cloudflare users**: Keep the DNS record as **DNS only** (grey cloud) until the certificate is issued. Afterwards you may enable the proxy (orange cloud), but set **SSL/TLS mode = Full (strict)**.

---

## 📦 What It Installs

| Component | Version / Source |
|-----------|------------------|
| **Vajra** | latest GitHub Release binary (`vajra-linux-amd64`) |
| **PHP-FPM** | 8.5 (from SURY repo) — falls back to 8.4 if unavailable |
| **Nginx** | Debian bookworm default (1.22.x) |
| **MariaDB** | Debian bookworm default (10.11.x) |
| **Certbot** | Let's Encrypt standalone |
| **WordPress** | latest (from wordpress.org) |

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
- **Vajra** — Rust-based web server: static files + FastCGI to PHP-FPM
- **PHP-FPM** — Executes WordPress code
- **MariaDB** — WordPress database

---

## 📂 File Layout on VPS

After the installer runs:

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
| Certbot `Timeout during connect` | Firewall blocking | Vultr dashboard + `ufw allow 80,443/tcp` |
| `HTTP/2 302` on install.php | WordPress already installed | Open `/wp-admin/` to log in |
| Browser `too many redirects` | Cloudflare SSL mode wrong | SSL/TLS → **Full (strict)** |
| `curl: (3) URL using bad/illegal format` | Invisible character in command | Type the entire command on one line |
| PHP-FPM socket missing | PHP install incomplete | Check `sudo systemctl status php8.4-fpm` |

### Firewall Checklist

```bash
# VPS-level
sudo ufw allow 22/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw reload

# Vultr Dashboard → Products → Servers → [server] → Settings → Firewall
# (If a Firewall Group is attached, allow HTTP/HTTPS)
```

### Verify Installation

```bash
{
  echo "=== Services ==="
  systemctl is-active nginx vajra php8.4-fpm mariadb

  echo "=== Ports ==="
  ss -lntp | grep -E ':(80|443|8080)\b'

  echo "=== HTTPS ==="
  curl -skI https://<domain>/ | head -2
} 2>&1
```

Expected: all services `active`, ports 80/443/8080 listening, HTTPS `200`.

---

## 🔄 Re-running the Installer

The installer is **idempotent** — it's safe to run it again:

- PHP/Nginx configs are overwritten
- Let's Encrypt cert is reused via `--keep-until-expiring`
- WordPress files are only downloaded if missing
- Vajra binary is atomically replaced (`.new` → `mv`)
- **⚠️ `wp-config.php` is regenerated every run** — the DB password changes

> To avoid breaking an existing WordPress, back up before running the installer:
> ```bash
> sudo cp /var/www/wordpress/wp-config.php /root/wp-config.backup
> ```

---

## 🌐 Environment Variables

| Var | Default | Purpose |
|-----|---------|---------|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repo for binary release |
| `VAJRA_VERSION` | `latest` | Release tag or `latest` |

**Example** — pin a specific version:

```bash
VAJRA_VERSION=v1.0.0 curl -fsSL ... | sudo bash -s -- <domain> <email>
```

---

## 🏗️ Building Vajra from Source

```bash
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server/vajra-phase7.1/vajra

# Debug build
cargo build

# Release build (static musl)
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl

# Output binary
ls -la target/x86_64-unknown-linux-musl/release/vajra
```

Install the binary:

```bash
sudo cp target/x86_64-unknown-linux-musl/release/vajra /usr/local/bin/vajra
sudo chmod +x /usr/local/bin/vajra
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
