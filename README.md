# Vajra Web Server + WordPress Multi-Site Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Debian%2012-red)](https://www.debian.org/)
[![PHP](https://img.shields.io/badge/PHP-8.5%20%7C%208.4-777BB4?logo=php&logoColor=white)](https://www.php.net/)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Security Policy](https://img.shields.io/badge/security-policy-blue)](SECURITY.md)

**One VPS. Unlimited WordPress sites.** Install the full stack on
Debian 12 with a single command (Vajra + Nginx + PHP-FPM + MariaDB +
Certbot + WP-CLI), then create as many isolated sites as you want with
`vajra-site add` — each gets its own domain, database, TLS certificate,
and dedicated Vajra process.

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash
```

Then, for each site:

```bash
vajra-site add blog.example.com you@example.com
```

---

## 📑 Table of Contents

- [Quick Start](#-quick-start)
- [Commands](#-commands)
- [Architecture](#-architecture)
- [File Layout](#-file-layout)
- [Managing Sites](#-managing-sites)
- [Backups & Restore](#-backups--restore)
- [Environment Variables](#-environment-variables)
- [Building Vajra from Source](#️-building-vajra-from-source)
- [Troubleshooting](#-troubleshooting)
- [Firewall Checklist](#-firewall-checklist)
- [Verifying Installation](#-verifying-installation)
- [Uninstall](#-uninstall)
- [License](#-license)

---

## 🚀 Quick Start

### Step 1 — Base stack (run once)

On a fresh Debian 12 VPS, as **root**:

```bash
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh \
  | bash
```

In 5–8 minutes this installs:

| Component | Version |
|---|---|
| **Vajra** | latest GitHub Release (or local build) |
| **PHP-FPM** | 8.5 (SURY) — falls back to 8.4 |
| **Nginx** | Debian 12 default (1.22.x) |
| **MariaDB** | Debian 12 default (10.11.x) |
| **Certbot** | Let's Encrypt |
| **WP-CLI** | `wp` → `/usr/local/bin/wp` |
| **`vajra-site`** | CLI to manage sites |

After the base install **no site is created yet** — only the stack is
ready.

### Step 2 — First site

```bash
vajra-site add blog.example.com you@example.com
```

The installer does everything:

1. Downloads WordPress → `/var/www/sites/blog.example.com/`
2. Creates MariaDB database + user
3. Generates `wp-config.php` and runs `wp core install` (non-interactive)
4. Writes Vajra config → `/etc/vajra/sites/blog.example.com.toml`
5. Starts `vajra-site@blog.example.com.service` (auto-allocated port)
6. Adds Nginx HTTP + ACME webroot
7. Obtains a Let's Encrypt certificate (zero downtime)
8. Upgrades Nginx to HTTPS
9. Prints admin credentials

At the end:

```
════════════════════════════════════════════════════════════════
✅ Site ready:  https://blog.example.com/
   Admin:       https://blog.example.com/wp-admin/
   Admin user:  wpadmin
   Admin pass:  X9kM2pQw8vN4rT6s
   DB name:     wp_blog_example_com
   DB user:     u_blog_example_com
   DB pass:     3fL9qRvKp8Hm2dWsY7nB4xT6
   Vajra port:  8080
════════════════════════════════════════════════════════════════
```

**No browser setup wizard.** Open `wp-admin`, log in, done.

### Step 3 — More sites

```bash
vajra-site add shop.example.com me@example.org
vajra-site add myblog.in admin@myblog.in --title "My Blog"
vajra-site add docs.example.com ops@example.com --admin-user docsadmin
```

Every call gets a new port, new database, new certificate, new process.
**No limit.**

### Prerequisites

| Requirement | Details |
|---|---|
| OS | Debian 12 (Bookworm) |
| Access | root |
| DNS | An A record for every domain pointing to the VPS IP, **before install** |
| Firewall | Ports 80 and 443 open (VPS + any cloud firewall) |
| Port 80 | Must be free (installer stops Nginx itself) |
| Disk | ~2 GB (base) + ~500 MB per site |
| glibc | ≥ 2.36 (Debian 12 = 2.36, see [GLIBC mismatch](#glibc-mismatch)) |

> ⚠️ **Cloudflare users**: Keep the DNS record as **DNS only** (grey
> cloud) until the certificate is issued. Afterwards you may enable the
> proxy (orange cloud), but you must set **SSL/TLS mode = Full (strict)**.

---

## 🎯 Commands

### `vajra-site add <domain> <email>`

Creates a new WordPress site.

```bash
vajra-site add example.com you@example.com
```

**Options:**

| Flag | Default | Purpose |
|---|---|---|
| `--title "..."` | `WordPress` | Site title |
| `--admin-user NAME` | `wpadmin` | WP admin username |
| `--admin-pass PASS` | random 16-char | WP admin password |

Example:

```bash
vajra-site add shop.example.com owner@shop.com \
  --title "My Shop" \
  --admin-user shopadmin \
  --admin-pass "StrongPass!234"
```

**What happens automatically:**
- `www.<domain>` is added as an alias
- DB name: `wp_<sanitized-domain>` (max 40 chars)
- DB user: `u_<sanitized-domain>` (max 20 chars)
- Port: auto-allocated 8080, 8081, 8082, ...
- Admin listener: port + 1000
- Access log: `/var/log/vajra/<domain>.access.log`

### `vajra-site list`

Table of all sites:

```bash
$ vajra-site list

DOMAIN                    PORT   DB                    SVC    CREATED
------                    ----   --                    ---    -------
blog.example.com          8080   wp_blog_example_com   ✅     2026-10-12T10:30:00Z
shop.example.com          8081   wp_shop_example_com   ✅     2026-10-12T10:35:00Z
myblog.in                 8082   wp_myblog_in          ✅     2026-10-12T10:40:00Z
```

`SVC` column:
- `✅` — systemd service `vajra-site@<domain>.service` is active
- `❌` — service down (check `systemctl status vajra-site@<domain>.service`)

### `vajra-site info <domain>`

Full details and credentials:

```bash
$ vajra-site info blog.example.com

Domain:       blog.example.com
WWW alias:    www.blog.example.com
Email:        you@example.com
Vajra port:   8080
Site root:    /var/www/sites/blog.example.com
DB name:      wp_blog_example_com
DB user:      u_blog_example_com
DB pass:      3fL9qRvKp8Hm2dWsY7nB4xT6
Admin user:   wpadmin
Admin pass:   X9kM2pQw8vN4rT6s
Created:      2026-10-12T10:30:00Z

Service:      vajra-site@blog.example.com.service  → active
Nginx conf:   /etc/nginx/sites-available/blog.example.com.conf
Vajra conf:   /etc/vajra/sites/blog.example.com.toml
Access log:   /var/log/vajra/blog.example.com.access.log
HTTPS:        https://blog.example.com/
WP admin:     https://blog.example.com/wp-admin/
```

### `vajra-site remove <domain>`

Tears down a site completely:

```bash
vajra-site remove blog.example.com
```

**Options:**
- `--keep-files` — keep `/var/www/sites/<domain>/`
- `--keep-db` — do not drop the database

### `vajra-site help`

Inline usage message.

---

## 🏗️ Architecture

### Multi-site topology

```
                        ┌──────────────────────────────────────────┐
                        │               Nginx :443                 │
                        │   per-site server block, TLS SNI based   │
                        └──┬──────────────┬──────────────┬─────────┘
                           │              │              │
                 blog.ex.  │      shop.ex.│       my.ex.  │
                   :8080   │       :8081  │        :8082 │
                           ▼              ▼              ▼
                     ┌──────────┐   ┌──────────┐   ┌──────────┐
                     │  Vajra   │   │  Vajra   │   │  Vajra   │
                     │ process  │   │ process  │   │ process  │
                     └────┬─────┘   └────┬─────┘   └────┬─────┘
                          │              │              │
                          └──────┬───────┴──────┬───────┘
                                 │              │
                                 ▼              ▼
                          ┌──────────────┐  ┌──────────────┐
                          │ PHP-FPM 8.x  │  │   MariaDB    │
                          │  (one pool)  │  │ (per-site DB)│
                          └──────────────┘  └──────────────┘
```

### Why per-site Vajra process?

Vajra uses a **thread-per-core, share-nothing** architecture. Each
process has its own io_uring ring, cache, and metrics registry. Running
**one Vajra process per site** is therefore the cleanest and fastest
design:

- **Isolation** — one site crashing doesn't affect the others
- **Zero lock contention** — sites never touch each other's caches or locks
- **Per-site metrics** — each Vajra has its own `/metrics` port (8080+1000, 8081+1000, …)
- **Easy shutdown** — `systemctl stop vajra-site@blog.example.com.service`

### Component responsibilities

| Layer | Role |
|---|---|
| **Nginx** | TLS termination, SNI routing, HTTP→HTTPS redirect, ACME webroot |
| **Vajra** | Static files + FastCGI to PHP-FPM, per-site cache and metrics |
| **PHP-FPM** | One shared pool serving all sites |
| **MariaDB** | One database and one user per site |
| **Certbot** | Webroot mode — existing sites are not taken down |
| **WP-CLI** | Non-interactive install, security defaults, rewrite rules |
| **systemd** | `vajra-site@.service` template unit — one unit, one instance per domain |

---

## 📂 File Layout on VPS

```
/usr/local/bin/
├── vajra                        # Vajra binary
├── vajra-site                   # Site management CLI
└── wp                           # WP-CLI

/etc/vajra/
├── vajra-php-version            # "8.5" or "8.4"
└── sites/
    ├── blog.example.com.toml    # Vajra config for this site
    ├── blog.example.com.env     # credentials (chmod 600)
    ├── shop.example.com.toml
    └── shop.example.com.env

/etc/systemd/system/
└── vajra-site@.service          # template unit
    (per-site symlinks in multi-user.target.wants/)

/etc/nginx/
├── sites-available/
│   ├── 000-default              # catch-all → 444
│   ├── blog.example.com.conf
│   └── shop.example.com.conf
└── sites-enabled/               # symlinks

/var/www/
├── sites/
│   ├── blog.example.com/        # WordPress root
│   └── shop.example.com/
└── certbot/
    ├── blog.example.com/.well-known/acme-challenge/
    └── shop.example.com/.well-known/acme-challenge/

/var/log/vajra/
├── blog.example.com.access.log
└── shop.example.com.access.log

/etc/letsencrypt/live/
├── blog.example.com/
└── shop.example.com/
```

---

## 🔧 Managing Sites

### Restart one site

```bash
systemctl restart vajra-site@blog.example.com.service
```

### Restart all sites

```bash
systemctl restart 'vajra-site@*.service'
```

### Live logs

```bash
# Vajra (Rust) — per site
journalctl -u vajra-site@blog.example.com.service -f

# Nginx
tail -f /var/log/nginx/error.log

# WordPress access log
tail -f /var/log/vajra/blog.example.com.access.log

# PHP-FPM
journalctl -u php8.4-fpm -f
```

### WP-CLI on a site

```bash
sudo -u www-data -- wp --path=/var/www/sites/blog.example.com --allow-root \
  plugin list
```

Or set a short alias:

```bash
alias wpblog='sudo -u www-data -- wp --path=/var/www/sites/blog.example.com --allow-root'
wpblog plugin update --all
```

### Database shell for a site

Read the password from the `.env` file:

```bash
source /etc/vajra/sites/blog.example.com.env
mysql -u"$DB_USER" -p"$DB_PASS" "$DB_NAME"
```

### Vajra metrics for one site

```bash
PORT=$(grep '^PORT=' /etc/vajra/sites/blog.example.com.env | cut -d= -f2)
curl -s "http://127.0.0.1:$((PORT + 1000))/metrics" | head
```

### Nginx reload after config change

```bash
nginx -t && systemctl reload nginx
```

### Certificate renewal

Certbot runs from a cron timer automatically. Manual test:

```bash
certbot renew --dry-run
```

A renewal hook is installed at
`/etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh` — Nginx reloads
itself after each successful renewal.

---

## 💾 Backups & Restore

### Per-site manual backup

```bash
D=blog.example.com
source /etc/vajra/sites/$D.env

# Database
mysqldump "$DB_NAME" | gzip > /root/${D}-db-$(date +%F).sql.gz

# Files
tar czf /root/${D}-files-$(date +%F).tar.gz -C /var/www/sites "$D"
```

### Restore

```bash
D=blog.example.com
source /etc/vajra/sites/$D.env

# Database
gunzip -c /root/${D}-db-2026-10-12.sql.gz | mysql "$DB_NAME"

# Files
tar xzf /root/${D}-files-2026-10-12.tar.gz -C /var/www/sites
chown -R www-data:www-data /var/www/sites/$D
```

### Back up all sites (script)

```bash
#!/bin/bash
BACKUP_DIR=/root/backups/$(date +%F)
mkdir -p "$BACKUP_DIR"
for env in /etc/vajra/sites/*.env; do
  D=$(sed -n 's/^DOMAIN=//p' "$env" | head -1)
  source "$env"
  mysqldump "$DB_NAME" | gzip > "$BACKUP_DIR/${D}-db.sql.gz"
  tar czf "$BACKUP_DIR/${D}-files.tar.gz" -C /var/www/sites "$D"
  cp "$env" "$BACKUP_DIR/${D}.env"
done
echo "✅ Backups in $BACKUP_DIR"
```

Add it to cron:

```bash
echo '0 3 * * * /root/backup-all-sites.sh' | crontab -
```

---

## 🌐 Environment Variables

When running `install.sh`:

| Var | Default | Purpose |
|---|---|---|
| `VAJRA_REPO` | `sugan0927/vajra-server` | GitHub repo slug |
| `VAJRA_VERSION` | `latest` | Release tag or `latest` |
| `VAJRA_SKIP_BINARY` | `0` | `1` → use the existing `/usr/local/bin/vajra` |

**Examples:**

```bash
# Pin a specific version
VAJRA_VERSION=v0.14.0 bash install.sh

# Use a locally built binary (fixes glibc mismatch)
VAJRA_SKIP_BINARY=1 bash install.sh
```

Per-site `.env` files (autogenerated) contain:

| Key | Description |
|---|---|
| `DOMAIN` | primary domain |
| `WWW_ALIAS` | `www.<domain>` |
| `EMAIL` | Let's Encrypt email |
| `PORT` | Vajra listen port |
| `DB_NAME`, `DB_USER`, `DB_PASS` | MariaDB credentials |
| `ADMIN_USER`, `ADMIN_PASS` | WordPress admin |
| `SITE_ROOT` | `/var/www/sites/<domain>` |
| `CREATED_AT` | ISO-8601 timestamp |

---

## 🏗️ Building Vajra from Source

If the pre-built binary does not run on your Debian 12 host (GLIBC
mismatch), build from source:

```bash
apt-get install -y build-essential pkg-config git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone https://github.com/sugan0927/vajra-server.git /opt/vajra-src
cd /opt/vajra-src/vajra-phase7.1/vajra

# Save disk space — target/ can grow to 31 GB otherwise
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[profile.release]
debug = 0
strip = "symbols"
EOF

# Release build
cargo build --release --locked
install -m 0755 target/release/vajra /usr/local/bin/vajra

# Verify
vajra --help 2>&1 | head -1
# Expected: "vajra 0.14.0 - thread-per-core io_uring web server"
```

Then run the installer with `VAJRA_SKIP_BINARY=1`.

### Static musl build (best long-term)

Runs on any Linux distro regardless of glibc:

```bash
rustup target add x86_64-unknown-linux-musl
apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl
install -m 0755 target/x86_64-unknown-linux-musl/release/vajra /usr/local/bin/vajra
```

**Also add this to the CI workflow** (`.github/workflows/release.yml`):

```yaml
- name: Install musl toolchain
  run: |
    sudo apt-get install -y musl-tools
    rustup target add x86_64-unknown-linux-musl

- name: Build (static musl)
  run: cargo build --release --locked --target x86_64-unknown-linux-musl

- name: Package
  run: |
    VERSION="${GITHUB_REF_NAME}"
    DIR="vajra-${VERSION}-x86_64-linux"
    mkdir -p "$DIR"
    cp target/x86_64-unknown-linux-musl/release/vajra "$DIR/"
    # ... rest of packaging
```

---

## 🐛 Troubleshooting

| Problem | Cause | Fix |
|---|---|---|
| `Port 80 busy` | Another web server running | `systemctl stop nginx apache2` |
| Certbot `Timeout during connect` | Firewall or DNS wrong | Check both VPS and cloud firewall, verify DNS |
| `too many redirects` in browser | Cloudflare SSL mode wrong | SSL/TLS → **Full (strict)** |
| `GLIBC_2.39 not found` | Ubuntu 24.04 CI binary on Debian 12 | [Build from source](#glibc-mismatch) |
| `vajra-site add` service start fails | Config error or port clash | `journalctl -u vajra-site@<domain>` |
| DB user `Access denied` | Stale password in `.env` | `vajra-site remove <domain>` then `add` |
| Cert renewal fails | ACME webroot permission issue | Check `/var/www/certbot/<domain>/` |
| `wp: command not found` | WP-CLI install failed | `curl -fsSL …/wp-cli.phar -o /usr/local/bin/wp && chmod +x /usr/local/bin/wp` |

### GLIBC mismatch

CI builds on `ubuntu-latest` (Ubuntu 24.04, glibc **2.39**). Debian 12
ships glibc **2.36**. The pre-built binary therefore prints:

```
vajra: /lib/x86_64-linux-gnu/libc.so.6: weak version `GLIBC_2.39' not found
```

**Fix — build from source** (see [Building from Source](#️-building-vajra-from-source)), then:

```bash
VAJRA_SKIP_BINARY=1 bash install.sh
vajra-site add blog.example.com you@example.com
```

**Long-term fix — musl static binary** ([above](#static-musl-build-best-long-term)).

### Service debugging

```bash
# Why did this site's service fail?
systemctl status vajra-site@blog.example.com.service
journalctl -u vajra-site@blog.example.com.service -n 50 --no-pager

# Test Vajra config manually
/usr/local/bin/vajra --check --config /etc/vajra/sites/blog.example.com.toml

# Nginx config check
nginx -t

# PHP-FPM status
systemctl status php8.4-fpm
```

### DNS check — before adding a site

```bash
dig +short blog.example.com @1.1.1.1   # must be your VPS IP
curl -fsSL https://api.ipify.org       # actual VPS IP
```

If they don't match, Certbot will time out at step 7.

---

## 🔥 Firewall Checklist

### VPS-level (installer handles it)

`install.sh` opens ports 80 and 443 in whichever of **ufw, firewalld,
nftables, or iptables** is active. Verify manually if you want:

```bash
# UFW
ufw allow 22/tcp   # SSH — first, or you'll lock yourself out
ufw allow 80/tcp
ufw allow 443/tcp
ufw reload
ufw status

# firewalld
firewall-cmd --permanent --add-port=22/tcp --add-port=80/tcp --add-port=443/tcp
firewall-cmd --reload

# nftables
nft add rule inet filter input tcp dport {22, 80, 443} accept
```

### Cloud-level firewall (manual)

| Provider | Where to open |
|---|---|
| **Vultr** | Products → Servers → [server] → Settings → Firewall → HTTP/HTTPS |
| **Hetzner** | Cloud Console → Firewalls → Rules → tcp/80, tcp/443 |
| **DigitalOcean** | Networking → Firewalls → Inbound Rules → HTTP, HTTPS |
| **AWS EC2** | Security Groups → Inbound → 80, 443 from 0.0.0.0/0 |
| **Cloudflare** | Not a firewall — DNS proxy. Keep grey cloud until certificate is issued |

---

## ✅ Verifying Installation

### Base stack check

```bash
{
  echo "=== Services ==="
  systemctl is-active nginx php8.4-fpm mariadb

  echo
  echo "=== Ports ==="
  ss -lntp | grep -E ':(80|443)\b'

  echo
  echo "=== Vajra binary ==="
  /usr/local/bin/vajra --help 2>&1 | head -1

  echo
  echo "=== WP-CLI ==="
  wp --version --allow-root 2>/dev/null

  echo
  echo "=== vajra-site CLI ==="
  vajra-site help 2>&1 | head -5
} 2>&1
```

Expected:
- `nginx`, `php8.4-fpm`, `mariadb` all `active`
- Ports 80 and 443 listening
- Vajra version line — **no GLIBC warning**
- WP-CLI version
- vajra-site help output

### Per-site check

```bash
D=blog.example.com

{
  echo "=== Vajra service ==="
  systemctl is-active vajra-site@${D}.service

  echo
  echo "=== Ports ==="
  PORT=$(grep '^PORT=' /etc/vajra/sites/${D}.env | cut -d= -f2)
  ss -lntp | grep ":${PORT}\b"

  echo
  echo "=== HTTPS response ==="
  curl -skI "https://${D}/" | head -2

  echo
  echo "=== WordPress install check ==="
  sudo -u www-data -- wp --path=/var/www/sites/${D} --allow-root \
    core is-installed && echo "✅ WP installed"
} 2>&1
```

Expected: service `active`, port listening, `HTTP/2 200` or `HTTP/2 302`,
`✅ WP installed`.

---

## 🔄 Re-running the Installer

`install.sh` is **idempotent** — safe to run again:

- Base packages only install if missing
- Vajra binary is atomically replaced
- `vajra-site` CLI is overwritten with the latest version
- **Existing sites are untouched** — `vajra-site add` never accepts a
  duplicate domain, and existing `.env`/`.toml` files are not overwritten

After a re-run you are ready to add more sites.

---

## 🗑️ Uninstall

```bash
# Remove all sites first
for env in /etc/vajra/sites/*.env; do
  D=$(sed -n 's/^DOMAIN=//p' "$env" | head -1)
  vajra-site remove "$D" || true
done

# Stop services
systemctl disable --now vajra-site@*.service 2>/dev/null || true
systemctl disable --now nginx php8.4-fpm mariadb

# Remove files
rm -rf /etc/vajra /var/www/sites /var/log/vajra
rm -f /usr/local/bin/vajra /usr/local/bin/vajra-site /usr/local/bin/wp
rm -f /etc/systemd/system/vajra-site@.service
rm -f /etc/nginx/sites-available/000-default /etc/nginx/sites-enabled/000-default

systemctl daemon-reload
```

> ⚠️ MariaDB, Nginx, and PHP packages are **not** removed. If you want
> to purge them as well:
> `apt-get purge nginx mariadb-server php8.4-*`

---

## 📜 License

MIT — see [LICENSE](LICENSE).

---

## 🤝 Contributing

1. Fork the repo
2. Create a feature branch: `git checkout -b feature/xyz`
3. Commit your changes: `git commit -am 'Add xyz'`
4. Push the branch: `git push origin feature/xyz`
5. Open a Pull Request

---

## 📮 Support

- **Issues**: [GitHub Issues](https://github.com/sugan0927/vajra-server/issues)
- **Security**: See [SECURITY.md](SECURITY.md)
- **Email**: sdsugans018@gmail.com

---

**Made with ❤️ for the self-hosted WordPress community.**