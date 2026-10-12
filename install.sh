#!/usr/bin/env bash
# Vajra Multi-Site WordPress Installer (base stack, run once)
#
# Usage:
#   bash install.sh                     # base install, no site yet
#   bash install.sh example.com you@x   # base install + first site
#
# Env vars:
#   VAJRA_REPO          GitHub repo slug (default: sugan0927/vajra-server)
#   VAJRA_VERSION       release tag or "latest" (default: latest)
#   VAJRA_SKIP_BINARY   "1" to keep existing /usr/local/bin/vajra
#
set -euo pipefail

FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"
PHP_CANDIDATES=(8.5 8.4)

[[ $EUID -ne 0 ]] && { echo "❌ Run as root"; exit 1; }

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }

# ─── Port opener — ufw / firewalld / nftables / iptables ────────────────
open_ports() {
  local ports=(80 443) opened=0

  if command -v ufw >/dev/null 2>&1; then
    if ufw status 2>/dev/null | grep -q "Status: active"; then
      for p in "${ports[@]}"; do ufw allow "${p}/tcp" >/dev/null 2>&1 || true; done
      ufw reload >/dev/null 2>&1 || true
      echo "   → ufw: opened ${ports[*]}/tcp"; opened=1
    fi
  fi

  if command -v firewall-cmd >/dev/null 2>&1; then
    if systemctl is-active --quiet firewalld 2>/dev/null; then
      for p in "${ports[@]}"; do
        firewall-cmd --permanent --add-port="${p}/tcp" >/dev/null 2>&1 || true
      done
      firewall-cmd --reload >/dev/null 2>&1 || true
      echo "   → firewalld: opened ${ports[*]}/tcp"; opened=1
    fi
  fi

  if command -v nft >/dev/null 2>&1; then
    if nft list ruleset 2>/dev/null | grep -qE 'table (inet|ip) filter'; then
      for p in "${ports[@]}"; do
        if ! nft list chain inet filter input 2>/dev/null | grep -q "dport ${p} accept" \
           && ! nft list chain ip filter input 2>/dev/null | grep -q "dport ${p} accept"; then
          nft add rule inet filter input tcp dport "${p}" accept 2>/dev/null \
            || nft add rule ip filter input tcp dport "${p}" accept 2>/dev/null \
            || true
        fi
      done
      echo "   → nftables: ensured ${ports[*]}/tcp"; opened=1
    fi
  fi

  if command -v iptables >/dev/null 2>&1 && [[ "$opened" == 0 ]]; then
    for p in "${ports[@]}"; do
      iptables -C INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null \
        || iptables -I INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null \
        || true
    done
    command -v netfilter-persistent >/dev/null 2>&1 \
      && netfilter-persistent save >/dev/null 2>&1 || true
    echo "   → iptables: opened ${ports[*]}/tcp"; opened=1
  fi

  [[ "$opened" == 0 ]] && echo "   → no host firewall active"
  echo "   ⚠ Cloud firewall (Vultr/Hetzner/DO/AWS SG) must allow 80 & 443"
}

# ─── 0. Pre-flight ──────────────────────────────────────────────────────
log "0/8 — Pre-flight"
open_ports
if ss -lntp 2>/dev/null | grep -q ':80\b'; then
  ss -lntp | grep ':80\b' || true
  die "Port 80 busy — stop the process above"
fi
echo "✅ port 80 free"

# ─── 1. Base packages + SURY ────────────────────────────────────────────
log "1/8 — Base packages + SURY repo"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y curl wget unzip ca-certificates gnupg lsb-release \
                   nginx mariadb-server certbot openssl python3 \
                   apt-transport-https software-properties-common

# Add SURY repo only if not already present
if [[ ! -f /etc/apt/sources.list.d/php.list ]]; then
  wget -qO /etc/apt/trusted.gpg.d/php.gpg https://packages.sury.org/php/apt.gpg
  echo "deb https://packages.sury.org/php/ $(lsb_release -sc) main" \
    > /etc/apt/sources.list.d/php.list
fi
apt-get update -y

# ─── PHP detection — robust, retries, debug output on failure ───────────
PHP_VER=""

# Method 1: try apt-cache show (reliable)
for v in "${PHP_CANDIDATES[@]}"; do
  if apt-cache show "php${v}-fpm" >/dev/null 2>&1; then
    # Confirm apt can actually resolve & install it
    if apt-get install -s -y "php${v}-fpm" >/dev/null 2>&1; then
      PHP_VER="$v"
      break
    fi
  fi
done

# Method 2: if still missing, force one more apt update + retry
if [[ -z "$PHP_VER" ]]; then
  warn "PHP not found on first pass — refreshing apt and retrying"
  apt-get update -y || true
  sleep 2
  for v in "${PHP_CANDIDATES[@]}"; do
    if apt-cache show "php${v}-fpm" >/dev/null 2>&1; then
      PHP_VER="$v"
      break
    fi
  done
fi

# Method 3: final fallback — install directly and see if it works
if [[ -z "$PHP_VER" ]]; then
  warn "apt-cache still empty — trying direct install of php8.4"
  if apt-get install -y php8.4-fpm >/dev/null 2>&1; then
    PHP_VER="8.4"
  elif apt-get install -y php8.5-fpm >/dev/null 2>&1; then
    PHP_VER="8.5"
  fi
fi

# Give up with useful debug
if [[ -z "$PHP_VER" ]]; then
  echo
  echo "Debug info — SURY repo state:" >&2
  echo "  sources.list.d/php.list:" >&2
  cat /etc/apt/sources.list.d/php.list 2>/dev/null | sed 's/^/    /' >&2 || echo "    (missing)" >&2
  echo "  Available php*-fpm packages:" >&2
  apt-cache search '^php[0-9.]+-fpm$' 2>/dev/null | sed 's/^/    /' >&2 || echo "    (none)" >&2
  echo "  apt-cache policy php8.4-fpm:" >&2
  apt-cache policy php8.4-fpm 2>&1 | sed 's/^/    /' >&2
  die "No supported PHP-FPM (8.5 or 8.4) available from SURY"
fi

echo "$PHP_VER" > /etc/vajra-php-version
PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
echo "✅ Using PHP ${PHP_VER} (${PHP_SOCK})"

# ─── 2. PHP + extensions ────────────────────────────────────────────────
log "2/8 — PHP ${PHP_VER} + extensions"
apt-get install -y \
  "php${PHP_VER}" "php${PHP_VER}-cli" "php${PHP_VER}-fpm" \
  "php${PHP_VER}-mysql" "php${PHP_VER}-curl" "php${PHP_VER}-xml" \
  "php${PHP_VER}-mbstring" "php${PHP_VER}-zip" "php${PHP_VER}-gd" \
  "php${PHP_VER}-intl" "php${PHP_VER}-bcmath"
apt-get install -y "php${PHP_VER}-imagick" 2>/dev/null || true
update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
php -v | head -1

# ─── 3. Vajra binary ────────────────────────────────────────────────────
log "3/8 — Vajra binary"
if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then
  [[ -x /usr/local/bin/vajra ]] || die "/usr/local/bin/vajra missing"
  # `vajra --help` exits non-zero even on success. Also pipefail would
  # break `if ! cmd | grep`, so capture output first.
  VAJRA_HELP_OUT="$(/usr/local/bin/vajra --help 2>&1 || true)"
  if ! grep -q 'thread-per-core' <<<"$VAJRA_HELP_OUT"; then
    printf 'vajra --help output was:\n%s\n' "$VAJRA_HELP_OUT" >&2
    die "vajra does not run"
  fi
  warn "VAJRA_SKIP_BINARY=1 — using existing binary"
  echo "   → $(head -1 <<<"$VAJRA_HELP_OUT")"
else
  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
      | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
    # Fallback: follow the /releases/latest redirect
    if [[ -z "$VAJRA_VERSION" ]]; then
      VAJRA_VERSION="$(curl -fsIL "https://github.com/${REPO}/releases/latest" \
        | grep -i '^location:' | head -1 | sed 's|.*/||' | tr -d '\r')"
    fi
    [[ -n "$VAJRA_VERSION" ]] || die "Could not resolve latest tag"
  fi
  echo "   → tag: ${VAJRA_VERSION}"

  TARBALL="vajra-${VAJRA_VERSION}-x86_64-linux.tar.gz"
  URL="https://github.com/${REPO}/releases/download/${VAJRA_VERSION}/${TARBALL}"
  TMP="$(mktemp -d)"
  trap '[[ -n "${TMP:-}" ]] && rm -rf "$TMP"' EXIT

  echo "   → ${URL}"
  curl -fL --retry 3 --retry-delay 2 "$URL" -o "$TMP/$TARBALL" \
    || die "Vajra download failed (tag ${VAJRA_VERSION}?)"

  tar -xzf "$TMP/$TARBALL" -C "$TMP"
  SRC_BIN="$TMP/vajra-${VAJRA_VERSION}-x86_64-linux/vajra"
  [[ -x "$SRC_BIN" ]] || die "tarball missing vajra binary at expected path"

  # Verify the pre-built binary actually runs here (catches GLIBC mismatch)
  if "$SRC_BIN" --help >/dev/null 2>&1 \
     && ! "$SRC_BIN" --help 2>&1 | grep -q 'GLIBC_.*not found'; then
    install -m 0755 "$SRC_BIN" /usr/local/bin/vajra
    echo "   → $(/usr/local/bin/vajra --help 2>&1 | head -1)"
  else
    warn "Pre-built binary will not run on this host:"
    "$SRC_BIN" --help 2>&1 | head -3 || true
    cat >&2 <<EOF

   This is almost always a glibc mismatch: the GitHub release is built on
   Ubuntu 24.04 (glibc 2.39), Debian 12 ships glibc 2.36.

   Build Vajra from source on this machine (~8 min):

     apt-get install -y build-essential pkg-config git
     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
     source "\$HOME/.cargo/env"
     git clone https://github.com/${REPO}.git /opt/vajra-src
     cd /opt/vajra-src/vajra-phase7.1/vajra
     cargo build --release --locked
     install -m 0755 target/release/vajra /usr/local/bin/vajra

   Then re-run this installer with:

     VAJRA_SKIP_BINARY=1 bash install.sh ${FIRST_DOMAIN:-} ${FIRST_EMAIL:-}

EOF
    die "Build from source, then re-run with VAJRA_SKIP_BINARY=1"
  fi
fi

# ─── 4. WP-CLI ──────────────────────────────────────────────────────────
log "4/8 — WP-CLI"
if ! command -v wp >/dev/null 2>&1; then
  curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar \
    -o /usr/local/bin/wp
  chmod 0755 /usr/local/bin/wp
fi
wp --version --allow-root

# ─── 5. Directories + Vajra systemd template ────────────────────────────
log "5/8 — Directories + systemd template"
mkdir -p /etc/vajra/sites /etc/vajra/certs /var/log/vajra \
         /var/www/sites /var/www/certbot
chown -R www-data:www-data /var/log/vajra /var/www/sites

cat > /etc/systemd/system/vajra-site@.service <<UNIT
[Unit]
Description=Vajra web server for %i
After=network.target php${PHP_VER}-fpm.service
Requires=php${PHP_VER}-fpm.service

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/vajra --config /etc/vajra/sites/%i.toml
Restart=on-failure
RestartSec=3
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT

# ─── 6. Base Nginx (no sites yet) ───────────────────────────────────────
log "6/8 — Base Nginx"
mkdir -p /etc/nginx/sites-available /etc/nginx/sites-enabled
rm -f /etc/nginx/sites-enabled/default
cat > /etc/nginx/sites-available/000-default <<'NGINX'
server {
    listen 80 default_server;
    listen [::]:80 default_server;
    server_name _;

    # ACME challenge goes to a shared webroot; per-site configs override
    location /.well-known/acme-challenge/ {
        root /var/www/certbot;
    }
    location / {
        return 444;   # no site configured for this Host
    }
}
NGINX
ln -sf /etc/nginx/sites-available/000-default /etc/nginx/sites-enabled/000-default
nginx -t
systemctl enable --now nginx
systemctl restart nginx

# ─── 7. Install vajra-site CLI ──────────────────────────────────────────
log "7/8 — Installing vajra-site CLI"
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_CLI_EOF'
#!/usr/bin/env bash
# vajra-site — manage multiple WordPress sites on this Vajra install
set -euo pipefail

SITES_DIR=/etc/vajra/sites
NGINX_AVAIL=/etc/nginx/sites-available
NGINX_ENABLED=/etc/nginx/sites-enabled
WWW_ROOT=/var/www/sites
CERTBOT_ROOT=/var/www/certbot
PHP_VER="$(cat /etc/vajra-php-version 2>/dev/null || echo 8.4)"
PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
FIRST_PORT=8080

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
info() { printf "\033[1;36mℹ  %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
err()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; }
die()  { err "$*"; exit 1; }

[[ $EUID -ne 0 ]] && die "Run as root"

usage() {
cat <<'U'
vajra-site — manage WordPress sites (each on its own Vajra + Nginx + cert)

USAGE:
  vajra-site add <domain> <email> [--title "Site Title"] [--admin-user NAME] [--admin-pass PASS]
  vajra-site list
  vajra-site info <domain>
  vajra-site remove <domain> [--keep-files] [--keep-db]
  vajra-site help

EXAMPLES:
  vajra-site add blog.example.com you@example.com
  vajra-site add shop.example.com me@x.io --title "My Shop" --admin-user shopadmin
  vajra-site list
  vajra-site info blog.example.com
  vajra-site remove blog.example.com

NOTES:
  • www.<domain> is added automatically as an alias.
  • Each site gets its own DB, DB user, Vajra instance (auto-allocated port), and cert.
  • Admin credentials are printed at the end of `add` and stored in
      /etc/vajra/sites/<domain>.env
U
}

valid_domain() {
  [[ "$1" =~ ^([a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$ ]]
}
valid_email() {
  [[ "$1" =~ ^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$ ]]
}
sanitize() { tr '[:upper:].-' '[:lower:]__' <<< "$1" | cut -c1-40; }

require_site() {
  local d="$1"
  [[ -f "${SITES_DIR}/${d}.env" ]] || die "Site '$d' not found"
}

next_port() {
  local used
  used="$(grep -h '^PORT=' "${SITES_DIR}"/*.env 2>/dev/null | cut -d= -f2 | sort -n | tail -1 || true)"
  echo $(( ${used:-$((FIRST_PORT-1))} + 1 ))
}

read_env() {
  local env_file="$1" key="$2"
  sed -n "s/^${key}=//p" "$env_file" | head -1
}

# ─── add ────────────────────────────────────────────────────────────────
cmd_add() {
  [[ $# -ge 2 ]] || { err "Usage: vajra-site add <domain> <email> [opts]"; exit 1; }
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local TITLE="WordPress" ADMIN_USER="wpadmin" ADMIN_PASS=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --title) TITLE="$2"; shift 2;;
      --admin-user) ADMIN_USER="$2"; shift 2;;
      --admin-pass) ADMIN_PASS="$2"; shift 2;;
      *) die "Unknown option: $1";;
    esac
  done

  valid_domain "$DOMAIN" || die "Invalid domain: $DOMAIN"
  valid_email  "$EMAIL"  || die "Invalid email: $EMAIL"
  [[ -f "${SITES_DIR}/${DOMAIN}.env" ]] && die "Site '$DOMAIN' already exists (use 'info' or 'remove' first)"

  local WWW_ALIAS="www.${DOMAIN}"
  local DB_NAME="wp_$(sanitize "$DOMAIN")"
  local DB_USER="u_$(sanitize "$DOMAIN")" && DB_USER="${DB_USER:0:20}"
  local DB_PASS
  DB_PASS="$(openssl rand -base64 24 | tr -d '/+=\n' | cut -c1-24)"
  [[ -n "$ADMIN_PASS" ]] || ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"

  local PORT; PORT="$(next_port)"
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"

  log "1/8 — Allocating resources"
  echo "   → domain: $DOMAIN (+ $WWW_ALIAS)"
  echo "   → port:   $PORT"
  echo "   → db:     $DB_NAME / $DB_USER"
  echo "   → root:   $SITE_ROOT"

  log "2/8 — WordPress files"
  mkdir -p "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}/.well-known/acme-challenge"
  if [[ ! -f "$SITE_ROOT/wp-load.php" ]]; then
    local TMP; TMP="$(mktemp -d)"
    curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
    cp -a "$TMP/wordpress/." "$SITE_ROOT/"
    rm -rf "$TMP"
  fi
  chown -R www-data:www-data "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}"

  log "3/8 — MariaDB database + user"
  mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
  mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  mysql -e "ALTER USER '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  mysql -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"

  log "4/8 — wp-config.php + WordPress install (WP-CLI)"
  wp --path="$SITE_ROOT" --allow-root config create \
      --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" \
      --dbhost=localhost --dbcharset=utf8mb4 --skip-check --force

  wp --path="$SITE_ROOT" --allow-root config set WP_HOME    "https://${DOMAIN}" --type=constant
  wp --path="$SITE_ROOT" --allow-root config set WP_SITEURL "https://${DOMAIN}" --type=constant

  # Reverse-proxy HTTPS awareness
  wp --path="$SITE_ROOT" --allow-root config set \
      VAJRA_PROXY_FIX "1" --raw --type=constant 2>/dev/null || true

  # Vajra's FastCGI bridge (v0.14.0) does not forward X-Forwarded-*
  # headers into PHP's $_SERVER, so WordPress cannot detect that TLS
  # was terminated by Nginx. Force HTTPS unconditionally: the site is
  # only ever reached via the Nginx TLS front. If Vajra later learns
  # to propagate X-Forwarded-* into FastCGI, this can become
  # conditional again.
  python3 - "$SITE_ROOT/wp-config.php" <<'PYEOF'
import sys
from pathlib import Path

p = Path(sys.argv[1])
s = p.read_text()
anchor = "/* That's all, stop editing!"
inject = (
    "/** Vajra reverse-proxy: force HTTPS (TLS is terminated by Nginx). */\n"
    "$_SERVER['HTTPS']       = 'on';\n"
    "$_SERVER['SERVER_PORT'] = 443;\n"
    "if (isset($_SERVER['HTTP_X_FORWARDED_HOST'])) {\n"
    "    $_SERVER['HTTP_HOST'] = $_SERVER['HTTP_X_FORWARDED_HOST'];\n"
    "}\n\n"
)
if 'Vajra reverse-proxy: force HTTPS' not in s and anchor in s:
    s = s.replace(anchor, inject + anchor, 1)
    p.write_text(s)
    print("   → wp-config.php: HTTPS force-injected")
else:
    print("   → wp-config.php: no change needed")
PYEOF

  wp --path="$SITE_ROOT" --allow-root core install \
      --url="https://${DOMAIN}" \
      --title="$TITLE" \
      --admin_user="$ADMIN_USER" \
      --admin_password="$ADMIN_PASS" \
      --admin_email="$EMAIL" \
      --skip-email

  # Small security/UX defaults
  wp --path="$SITE_ROOT" --allow-root option update blogdescription "" 2>/dev/null || true
  wp --path="$SITE_ROOT" --allow-root rewrite structure '/%postname%/' --hard 2>/dev/null || true

  log "5/8 — Vajra config + service"
  cat > "${SITES_DIR}/${DOMAIN}.toml" <<TOML
[server]
listen  = "127.0.0.1:${PORT}"
workers = 1
pin     = false

[static]
root  = "${SITE_ROOT}"
index = "index.html"

[php]
upstream         = "unix:${PHP_SOCK}"
index            = ["index.php"]
front_controller = "/index.php"
timeout_secs     = 60

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/${DOMAIN}.access.log"

[admin]
listen = "127.0.0.1:$((PORT + 1000))"
TOML

  cat > "${SITES_DIR}/${DOMAIN}.env" <<ENV
DOMAIN=${DOMAIN}
WWW_ALIAS=${WWW_ALIAS}
EMAIL=${EMAIL}
PORT=${PORT}
DB_NAME=${DB_NAME}
DB_USER=${DB_USER}
DB_PASS=${DB_PASS}
ADMIN_USER=${ADMIN_USER}
ADMIN_PASS=${ADMIN_PASS}
SITE_ROOT=${SITE_ROOT}
CREATED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
ENV
  chmod 600 "${SITES_DIR}/${DOMAIN}.env"

  systemctl daemon-reload
  systemctl enable --now "vajra-site@${DOMAIN}.service"
  sleep 1
  systemctl is-active --quiet "vajra-site@${DOMAIN}.service" \
    || { journalctl -u "vajra-site@${DOMAIN}.service" -n 30 --no-pager; die "Vajra failed for ${DOMAIN}"; }
  echo "   → vajra :${PORT}  ✅"

  log "6/8 — Nginx HTTP (for ACME), then TLS"
  cat > "${NGINX_AVAIL}/${DOMAIN}.conf" <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN} ${WWW_ALIAS};

    location /.well-known/acme-challenge/ {
        root ${CERTBOT_ROOT}/${DOMAIN};
    }
    location / {
        return 301 https://\$host\$request_uri;
    }
}
NGINX
  ln -sf "${NGINX_AVAIL}/${DOMAIN}.conf" "${NGINX_ENABLED}/${DOMAIN}.conf"
  nginx -t && systemctl reload nginx

  log "7/8 — Let's Encrypt certificate"
  certbot certonly --webroot -w "${CERTBOT_ROOT}/${DOMAIN}" \
      -d "${DOMAIN}" -d "${WWW_ALIAS}" \
      --agree-tos --email "$EMAIL" --non-interactive --keep-until-expiring

  log "8/8 — Nginx HTTPS + final reload"
  cat > "${NGINX_AVAIL}/${DOMAIN}.conf" <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN} ${WWW_ALIAS};
    location /.well-known/acme-challenge/ { root ${CERTBOT_ROOT}/${DOMAIN}; }
    location / { return 301 https://\$host\$request_uri; }
}
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name ${DOMAIN} ${WWW_ALIAS};

    ssl_certificate     /etc/letsencrypt/live/${DOMAIN}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/${DOMAIN}/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    client_max_body_size 64m;

    location / {
        proxy_pass http://127.0.0.1:${PORT};
        proxy_http_version 1.1;
        proxy_set_header Host              \$host;
        proxy_set_header X-Real-IP         \$remote_addr;
        proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-Host  $host;
        proxy_set_header Connection        "";
        proxy_read_timeout 120s;
        proxy_buffering off;
    }
}
NGINX
  nginx -t && systemctl reload nginx

  echo
  echo "════════════════════════════════════════════════════════════════"
  echo "✅ Site ready:  https://${DOMAIN}/"
  echo "   Admin:       https://${DOMAIN}/wp-admin/"
  echo "   Admin user:  ${ADMIN_USER}"
  echo "   Admin pass:  ${ADMIN_PASS}"
  echo "   DB name:     ${DB_NAME}"
  echo "   DB user:     ${DB_USER}"
  echo "   DB pass:     ${DB_PASS}"
  echo "   Vajra port:  ${PORT}"
  echo "   Log file:    /var/log/vajra/${DOMAIN}.access.log"
  echo "   Config:      ${SITES_DIR}/${DOMAIN}.toml"
  echo "   Meta:        ${SITES_DIR}/${DOMAIN}.env   (chmod 600)"
  echo "════════════════════════════════════════════════════════════════"
  echo "   ⚠ Save the admin password — WordPress will NOT email it"
  echo "     (we used --skip-email). It's stored in the .env file above."
  echo
}

# ─── list ───────────────────────────────────────────────────────────────
cmd_list() {
  local envs; envs=( "${SITES_DIR}"/*.env )
  if [[ ! -e "${envs[0]}" ]]; then
    info "No sites yet. Add one:"
    echo "  vajra-site add example.com you@example.com"
    return
  fi
  printf '\n%-30s %-6s %-22s %-6s %s\n' "DOMAIN" "PORT" "DB" "SVC" "CREATED"
  printf '%-30s %-6s %-22s %-6s %s\n' "------" "----" "--" "---" "-------"
  for f in "${envs[@]}"; do
    local d p db svc c
    d="$(read_env "$f" DOMAIN)"
    p="$(read_env "$f" PORT)"
    db="$(read_env "$f" DB_NAME)"
    c="$(read_env "$f" CREATED_AT)"
    if systemctl is-active --quiet "vajra-site@${d}.service"; then svc="✅"; else svc="❌"; fi
    printf '%-30s %-6s %-22s %-6s %s\n' "$d" "$p" "$db" "$svc" "$c"
  done
  echo
}

# ─── info ───────────────────────────────────────────────────────────────
cmd_info() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site info <domain>"
  local D="$1"; require_site "$D"
  local f="${SITES_DIR}/${D}.env"
  echo
  echo "Domain:       $(read_env "$f" DOMAIN)"
  echo "WWW alias:    $(read_env "$f" WWW_ALIAS)"
  echo "Email:        $(read_env "$f" EMAIL)"
  echo "Vajra port:   $(read_env "$f" PORT)"
  echo "Site root:    $(read_env "$f" SITE_ROOT)"
  echo "DB name:      $(read_env "$f" DB_NAME)"
  echo "DB user:      $(read_env "$f" DB_USER)"
  echo "DB pass:      $(read_env "$f" DB_PASS)"
  echo "Admin user:   $(read_env "$f" ADMIN_USER)"
  echo "Admin pass:   $(read_env "$f" ADMIN_PASS)"
  echo "Created:      $(read_env "$f" CREATED_AT)"
  echo
  echo "Service:      vajra-site@${D}.service  → $(systemctl is-active "vajra-site@${D}.service" || true)"
  echo "Nginx conf:   ${NGINX_AVAIL}/${D}.conf"
  echo "Vajra conf:   ${SITES_DIR}/${D}.toml"
  echo "Access log:   /var/log/vajra/${D}.access.log"
  echo "HTTPS:        https://${D}/"
  echo "WP admin:     https://${D}/wp-admin/"
  echo
}

# ─── remove ─────────────────────────────────────────────────────────────
cmd_remove() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site remove <domain> [--keep-files] [--keep-db]"
  local D="$1"; shift
  local KEEP_FILES=0 KEEP_DB=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --keep-files) KEEP_FILES=1; shift;;
      --keep-db)    KEEP_DB=1;    shift;;
      *) die "Unknown option: $1";;
    esac
  done
  require_site "$D"
  local f="${SITES_DIR}/${D}.env"
  local PORT DB_NAME DB_USER SITE_ROOT WWW_ALIAS
  PORT="$(read_env "$f" PORT)"; DB_NAME="$(read_env "$f" DB_NAME)"
  DB_USER="$(read_env "$f" DB_USER)"; SITE_ROOT="$(read_env "$f" SITE_ROOT)"
  WWW_ALIAS="$(read_env "$f" WWW_ALIAS)"

  warn "Removing site: $D"
  read -r -p "Continue? [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || { info "Aborted"; return; }

  log "Stopping services"
  systemctl disable --now "vajra-site@${D}.service" 2>/dev/null || true
  rm -f "/etc/systemd/system/vajra-site@${D}.service.d/override.conf" 2>/dev/null || true
  systemctl daemon-reload

  log "Removing Nginx config"
  rm -f "${NGINX_ENABLED}/${D}.conf" "${NGINX_AVAIL}/${D}.conf"
  nginx -t && systemctl reload nginx

  log "Removing cert"
  certbot delete --cert-name "$D" --non-interactive 2>/dev/null || true

  log "Removing Vajra config"
  rm -f "${SITES_DIR}/${D}.toml"

  if [[ "$KEEP_DB" == 0 ]]; then
    log "Dropping database ${DB_NAME}"
    mysql -e "DROP DATABASE IF EXISTS \`${DB_NAME}\`;"
    mysql -e "DROP USER IF EXISTS '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"
  else
    info "Keeping DB: ${DB_NAME}"
  fi

  if [[ "$KEEP_FILES" == 0 ]]; then
    log "Removing site files"
    rm -rf "$SITE_ROOT" "${CERTBOT_ROOT}/${D}"
  else
    info "Keeping files: ${SITE_ROOT}"
  fi

  rm -f "$f"
  echo
  echo "✅ Site '$D' removed"
}

# ─── dispatch ───────────────────────────────────────────────────────────
case "${1:-}" in
  add)         shift; cmd_add "$@";;
  list|ls)     cmd_list;;
  info|show)   shift; cmd_info "$@";;
  remove|rm)   shift; cmd_remove "$@";;
  help|-h|--help|"") usage;;
  *) err "Unknown command: $1"; echo; usage; exit 1;;
esac
VAJRA_SITE_CLI_EOF

chmod 0755 /usr/local/bin/vajra-site

# ─── 8. Optional: create first site now ─────────────────────────────────
log "8/8 — Base install complete"
echo
echo "✅ Vajra stack installed."
echo
echo "   Manage sites with:"
echo "     vajra-site add <domain> <email>     # create a new WordPress site"
echo "     vajra-site list                     # list sites"
echo "     vajra-site info <domain>            # credentials + paths"
echo "     vajra-site remove <domain>          # tear down a site"
echo

if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
  log "Creating first site: ${FIRST_DOMAIN}"
  /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL"
fi