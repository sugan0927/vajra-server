#!/usr/bin/env bash
# Vajra + WordPress One-Shot Installer
# Debian 12 | Nginx | MariaDB | Certbot | PHP 8.5 (or 8.4)
set -euo pipefail

DOMAIN="${1:-}"
EMAIL="${2:-}"
REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"

if [[ -z "$DOMAIN" || -z "$EMAIL" ]]; then
  echo "Usage: $0 <domain> <email>"; exit 1
fi
if [[ $EUID -ne 0 ]]; then
  echo "❌ Run as root"; exit 1
fi

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*"; exit 1; }

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

PHP_VER="8.5"
if ! apt-cache policy "php${PHP_VER}-fpm" 2>/dev/null | grep -q Candidate; then
  warn "PHP 8.5 not on SURY — using 8.4"
  PHP_VER="8.4"
fi
PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"

log "2/9 — PHP ${PHP_VER} + extensions"
apt-get install -y \
  "php${PHP_VER}" "php${PHP_VER}-cli" "php${PHP_VER}-fpm" \
  "php${PHP_VER}-mysql" "php${PHP_VER}-curl" "php${PHP_VER}-xml" \
  "php${PHP_VER}-mbstring" "php${PHP_VER}-zip" "php${PHP_VER}-gd" \
  "php${PHP_VER}-intl" "php${PHP_VER}-bcmath"
apt-get install -y "php${PHP_VER}-imagick" 2>/dev/null || true
update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
php -v | head -1

log "3/9 — Vajra binary"
if [[ "$VAJRA_VERSION" == "latest" ]]; then
  URL="https://github.com/${REPO}/releases/latest/download/vajra-linux-amd64"
else
  URL="https://github.com/${REPO}/releases/download/${VAJRA_VERSION}/vajra-linux-amd64"
fi
curl -fL "$URL" -o /usr/local/bin/vajra.new
chmod +x /usr/local/bin/vajra.new
mv -f /usr/local/bin/vajra.new /usr/local/bin/vajra
vajra --version 2>/dev/null | head -1 || true

log "4/9 — Directories"
mkdir -p /etc/vajra/certs /var/log/vajra /var/www/wordpress
chown -R www-data:www-data /var/log/vajra /var/www/wordpress

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
systemctl is-active vajra || die "Vajra failed to start"

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

log "9/9 — WordPress + wp-config.php"
if [[ ! -f /var/www/wordpress/wp-load.php ]]; then
  curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C /tmp
  cp -a /tmp/wordpress/. /var/www/wordpress/
fi
chown -R www-data:www-data /var/www/wordpress

DB_NAME=wordpress
DB_USER=wpuser
DB_PASS="$(openssl rand -base64 18 | tr -d '/+=')"

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
