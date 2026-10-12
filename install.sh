#!/usr/bin/env bash
# Vajra + WordPress One-Shot Installer
# Debian 12 | Nginx | MariaDB | Certbot | PHP 8.5 (or 8.4)
set -euo pipefail

DOMAIN="${1:-}"
EMAIL="${2:-}"
REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"
PHP_CANDIDATES=(8.5 8.4)

if [[ -z "$DOMAIN" || -z "$EMAIL" ]]; then
  echo "Usage: $0 <domain> <email>"; exit 1
fi
if [[ $EUID -ne 0 ]]; then
  echo "❌ Run as root"; exit 1
fi

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*"; exit 1; }

# ─── 0. Pre-flight ──────────────────────────────────────────────────────
log "0/9 — Pre-flight checks"

if systemctl is-active --quiet vajra 2>/dev/null; then
  warn "Stopping Vajra to allow binary replace"
  systemctl stop vajra || true
  sleep 1
fi
if systemctl is-active --quiet nginx 2>/dev/null; then
  warn "Stopping Nginx (port 80 needed for standalone TLS)"
  systemctl stop nginx 2>/dev/null || true
fi
if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow 80/tcp  >/dev/null 2>&1 || true
  ufw allow 443/tcp >/dev/null 2>&1 || true
fi
if ss -lntp 2>/dev/null | grep -q ':80\b'; then
  ss -lntp | grep ':80\b' || true
  die "Port 80 busy — stop the process above"
fi
echo "✅ port 80 free"

# ─── 1. Base packages + SURY repo ───────────────────────────────────────
log "1/9 — Base packages + SURY repo"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y curl wget unzip ca-certificates gnupg lsb-release \
                   nginx mariadb-server certbot openssl \
                   apt-transport-https software-properties-common python3

wget -qO /etc/apt/trusted.gpg.d/php.gpg https://packages.sury.org/php/apt.gpg
echo "deb https://packages.sury.org/php/ $(lsb_release -sc) main" \
  > /etc/apt/sources.list.d/php.list
apt-get update -y

# Pick highest PHP that SURY actually has a candidate for
PHP_VER=""
for v in "${PHP_CANDIDATES[@]}"; do
  if apt-cache policy "php${v}-fpm" 2>/dev/null | grep -q 'Candidate: [0-9]'; then
    PHP_VER="$v"
    break
  fi
done
[[ -z "$PHP_VER" ]] && die "No supported PHP (8.5/8.4) available from SURY repo"
PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
echo "✅ Using PHP ${PHP_VER}"

# ─── 2. PHP + extensions ────────────────────────────────────────────────
log "2/9 — PHP ${PHP_VER} + extensions"
apt-get install -y \
  "php${PHP_VER}" "php${PHP_VER}-cli" "php${PHP_VER}-fpm" \
  "php${PHP_VER}-mysql" "php${PHP_VER}-curl" "php${PHP_VER}-xml" \
  "php${PHP_VER}-mbstring" "php${PHP_VER}-zip" "php${PHP_VER}-gd" \
  "php${PHP_VER}-intl" "php${PHP_VER}-bcmath"
apt-get install -y "php${PHP_VER}-imagick" 2>/dev/null || true
update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
php -v | head -1

# ─── 3. Vajra binary (FIXED: use tarball asset, verify it runs) ─────────
log "3/9 — Vajra binary"

if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then
  warn "VAJRA_SKIP_BINARY=1 — keeping /usr/local/bin/vajra unchanged"
  [[ -x /usr/local/bin/vajra ]] || die "/usr/local/bin/vajra missing"
  /usr/local/bin/vajra --help >/dev/null 2>&1 || die "existing vajra does not run"
else
  # Resolve "latest" to a concrete tag via GitHub API
  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
      | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
    [[ -n "$VAJRA_VERSION" ]] || die "Could not determine latest Vajra release tag"
  fi
  echo "   → tag: ${VAJRA_VERSION}"

  TARBALL="vajra-${VAJRA_VERSION}-x86_64-linux.tar.gz"
  URL="https://github.com/${REPO}/releases/download/${VAJRA_VERSION}/${TARBALL}"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT

  echo "   → ${URL}"
  curl -fL --retry 3 --retry-delay 2 "$URL" -o "$TMP/$TARBALL" \
    || die "Vajra download failed (tag ${VAJRA_VERSION}?)"

  tar -xzf "$TMP/$TARBALL" -C "$TMP"
  SRC_BIN="$TMP/vajra-${VAJRA_VERSION}-x86_64-linux/vajra"
  [[ -x "$SRC_BIN" ]] || die "tarball missing vajra binary at expected path"

  # Verify the pre-built binary actually runs on this host.
  # Catches the common "GLIBC_2.39 not found" case when CI builds on
  # Ubuntu 24.04 but the target is Debian 12 (glibc 2.36).
  if "$SRC_BIN" --help >/dev/null 2>&1; then
    install -m 0755 "$SRC_BIN" /usr/local/bin/vajra
    echo "   → $(/usr/local/bin/vajra --help 2>&1 | head -1)"
  else
    warn "Pre-built binary will not run on this host:"
    "$SRC_BIN" --help 2>&1 | head -3 || true
    cat >&2 <<EOF

   This is almost always a glibc mismatch: the GitHub release is built on
   Ubuntu 24.04 (glibc 2.39) while Debian 12 ships glibc 2.36.

   Build Vajra from source on this machine (≈8 min):

     apt-get install -y build-essential pkg-config git
     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
     source "\$HOME/.cargo/env"
     git clone https://github.com/${REPO}.git /opt/vajra-src
     cd /opt/vajra-src/vajra-phase7.1/vajra
     cargo build --release --locked
     install -m 0755 target/release/vajra /usr/local/bin/vajra

   Then re-run this installer with:

     VAJRA_SKIP_BINARY=1 bash <(curl -fsSL .../install.sh) ${DOMAIN} ${EMAIL}

EOF
    die "Set VAJRA_SKIP_BINARY=1 after building from source, then re-run."
  fi
fi

# ─── 4. Directories ─────────────────────────────────────────────────────
log "4/9 — Directories"
mkdir -p /etc/vajra/certs /var/log/vajra /var/www/wordpress
chown -R www-data:www-data /var/log/vajra /var/www/wordpress

# ─── 5. Vajra config ────────────────────────────────────────────────────
log "5/9 — Vajra config"
cat > /etc/vajra/app.toml <<TOML
[server]
listen  = "127.0.0.1:8080"
workers = 1
pin     = false

[static]
root  = "/var/www/wordpress"
index = "index.html"

[php]
upstream         = "unix:${PHP_SOCK}"
index            = ["index.php"]
front_controller = "/index.php"
timeout_secs     = 60

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/access.log"

[admin]
listen = "127.0.0.1:9200"
TOML

# ─── 6. systemd unit ────────────────────────────────────────────────────
log "6/9 — systemd unit"
cat > /etc/systemd/system/vajra.service <<UNIT
[Unit]
Description=Vajra web server (HTTP/1.1 + PHP-FPM backend)
After=network.target php${PHP_VER}-fpm.service mariadb.service
Requires=php${PHP_VER}-fpm.service

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
Restart=on-failure
RestartSec=3
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now vajra
sleep 1
systemctl is-active --quiet vajra \
  || { journalctl -u vajra -n 30 --no-pager; die "Vajra failed to start"; }

# ─── 7. TLS certificate ─────────────────────────────────────────────────
log "7/9 — TLS certificate"
systemctl stop nginx 2>/dev/null || true
sleep 1
if ss -lntp 2>/dev/null | grep -q ':80\b'; then
  ss -lntp | grep ':80\b' || true
  die "Port 80 became busy"
fi

certbot certonly --standalone -d "$DOMAIN" \
    --agree-tos --email "$EMAIL" --non-interactive --keep-until-expiring

cp -L "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem" /etc/vajra/certs/
cp -L "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"   /etc/vajra/certs/
chmod 644 /etc/vajra/certs/fullchain.pem
chmod 600 /etc/vajra/certs/privkey.pem

# Certbot renewal hook — reload nginx after each successful renewal
mkdir -p /etc/letsencrypt/renewal-hooks/deploy
cat > /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh <<'HOOK'
#!/bin/sh
systemctl reload nginx || true
HOOK
chmod 0755 /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh

# ─── 8. Nginx reverse proxy ─────────────────────────────────────────────
log "8/9 — Nginx reverse proxy"
cat > /etc/nginx/sites-available/vajra <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};
    return 301 https://\$host\$request_uri;
}
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name ${DOMAIN};

    ssl_certificate     /etc/vajra/certs/fullchain.pem;
    ssl_certificate_key /etc/vajra/certs/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    client_max_body_size 64m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host              \$host;
        proxy_set_header X-Real-IP         \$remote_addr;
        proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header Connection        "";
        proxy_read_timeout 120s;
        proxy_buffering off;
    }
}
NGINX
rm -f /etc/nginx/sites-enabled/default
ln -sf /etc/nginx/sites-available/vajra /etc/nginx/sites-enabled/vajra
nginx -t
systemctl enable --now nginx
systemctl restart nginx

# ─── 9. WordPress + wp-config.php ───────────────────────────────────────
log "9/9 — WordPress + wp-config.php"
if [[ ! -f /var/www/wordpress/wp-load.php ]]; then
  TMP_WP="$(mktemp -d)"
  curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP_WP"
  cp -a "$TMP_WP/wordpress/." /var/www/wordpress/
  rm -rf "$TMP_WP"
fi
chown -R www-data:www-data /var/www/wordpress

DB_NAME=wordpress
DB_USER=wpuser

# ─── Preserve existing DB password on re-run (FIXED) ────────────────────
WPCONF=/var/www/wordpress/wp-config.php
if [[ -f "$WPCONF" ]]; then
  warn "wp-config.php exists — reusing DB credentials from it"
  DB_PASS="$(sed -n "s/.*define *(\s*'DB_PASSWORD'\s*,\s*'\([^']*\)'.*/\1/p" "$WPCONF" | head -1)"
  DB_NAME="$(sed -n "s/.*define *(\s*'DB_NAME'\s*,\s*'\([^']*\)'.*/\1/p" "$WPCONF" | head -1)"
  DB_USER="$(sed -n "s/.*define *(\s*'DB_USER'\s*,\s*'\([^']*\)'.*/\1/p" "$WPCONF" | head -1)"
  [[ -n "$DB_PASS" && -n "$DB_NAME" && -n "$DB_USER" ]] \
    || { warn "could not parse existing wp-config.php — generating fresh creds"; DB_PASS=""; }
fi

if [[ -z "${DB_PASS:-}" ]]; then
  DB_PASS="$(openssl rand -base64 18 | tr -d '/+=')"
fi

DOMAIN="$DOMAIN" DB_NAME="$DB_NAME" DB_USER="$DB_USER" DB_PASS="$DB_PASS" \
python3 - <<'PY'
import os, re
from pathlib import Path

src_path = Path("/var/www/wordpress/wp-config-sample.php")
if not src_path.exists():
    src_path = Path("/var/www/wordpress/wp-config.php")
src = src_path.read_text()

src = (src
  .replace("database_name_here", os.environ["DB_NAME"])
  .replace("username_here",      os.environ["DB_USER"])
  .replace("password_here",      os.environ["DB_PASS"]))

src = re.sub(r"define\(\s*'WP_HOME'.*?\);\s*", "", src)
src = re.sub(r"define\(\s*'WP_SITEURL'.*?\);\s*", "", src)

tweak = f"""
define('WP_HOME',    'https://{os.environ['DOMAIN']}');
define('WP_SITEURL', 'https://{os.environ['DOMAIN']}');
if (
    (isset($_SERVER['HTTP_X_FORWARDED_PROTO']) &&
     strpos($_SERVER['HTTP_X_FORWARDED_PROTO'], 'https') !== false)
    || (isset($_SERVER['HTTP_X_FORWARDED_SSL']) &&
        $_SERVER['HTTP_X_FORWARDED_SSL'] === 'on')
) {{
    $_SERVER['HTTPS'] = 'on';
    $_SERVER['SERVER_PORT'] = 443;
}}
if (PHP_SAPI !== 'cli') {{
    $_SERVER['HTTPS'] = 'on';
    $_SERVER['SERVER_PORT'] = 443;
}}
"""
src = src.replace("/* That's all, stop editing!",
                  tweak + "\n/* That's all, stop editing!")
Path("/var/www/wordpress/wp-config.php").write_text(src)
print("wp-config.php ready")
PY

chown www-data:www-data /var/www/wordpress/wp-config.php
chmod 640           /var/www/wordpress/wp-config.php

mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4;"
mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
mysql -e "ALTER USER '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
mysql -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"

systemctl restart "php${PHP_VER}-fpm" vajra nginx
sleep 1
systemctl is-active nginx vajra "php${PHP_VER}-fpm" mariadb

echo
echo "════════════════════════════════════════════════════════"
echo "✅ Ready: https://${DOMAIN}/wp-admin/install.php"
echo "   DB Name : ${DB_NAME}"
echo "   DB User : ${DB_USER}"
echo "   DB Pass : ${DB_PASS}"
echo "════════════════════════════════════════════════════════"
echo
echo "   Save these — wp-config.php at /var/www/wordpress/wp-config.php"
echo "   already contains them, so re-running this installer will NOT"
echo "   break the site."