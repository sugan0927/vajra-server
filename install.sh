#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer (v13 — Ultimate Universal Fix)
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
SITE_MODE="${SITE_MODE:-wordpress}"

if [[ "${3:-}" == "--php"  ]]; then SITE_MODE="php";  fi
if [[ "${3:-}" == "--html" ]]; then SITE_MODE="html"; fi

VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }

# ─── Distro Detection ───────────────────────────────────────────────────
DISTRO_ID=""; DISTRO_LIKE=""; DISTRO_VER=""; DISTRO_NAME=""; DISTRO_FAMILY=""
detect_distro() {
  [[ -f /etc/os-release ]] || die "/etc/os-release missing"
  . /etc/os-release
  DISTRO_ID="${ID:-unknown}"
  DISTRO_LIKE="${ID_LIKE:-}"
  DISTRO_VER="${VERSION_ID:-}"
  DISTRO_NAME="${PRETTY_NAME:-$DISTRO_ID $DISTRO_VER}"
  case "$DISTRO_ID" in
    debian|ubuntu|linuxmint|pop|kali|raspbian) DISTRO_FAMILY="debian" ;;
    rhel|centos|rocky|almalinux|ol|fedora)     DISTRO_FAMILY="rhel"   ;;
    arch|manjaro|endeavouros|garuda)           DISTRO_FAMILY="arch"   ;;
    *)
      case "$DISTRO_LIKE" in
        *debian*|*ubuntu*) DISTRO_FAMILY="debian" ;;
        *rhel*|*fedora*)   DISTRO_FAMILY="rhel"   ;;
        *arch*)            DISTRO_FAMILY="arch"   ;;
        *) die "Unsupported distro: $DISTRO_NAME" ;;
      esac ;;
  esac
  command -v systemctl >/dev/null 2>&1 || die "systemd required"
  [[ "$(uname -m)" == "x86_64" ]] || die "Only x86_64 supported"
  echo "   → distro: $DISTRO_NAME"
  echo "   → family: $DISTRO_FAMILY"
}

www_user() {
  case "$DISTRO_FAMILY" in
    debian) echo "www-data" ;;
    rhel)   echo "apache"   ;;
    arch)   echo "http"     ;;
  esac
}
WWW_USER=""

# ─── Package Manager ────────────────────────────────────────────────────
pkg_update() {
  case "$DISTRO_FAMILY" in
    debian) DEBIAN_FRONTEND=noninteractive apt-get update -y ;;
    rhel)   dnf -y makecache >/dev/null 2>&1 || true ;;
    arch)   pacman -Sy --noconfirm ;;
  esac
}
pkg_install() {
  case "$DISTRO_FAMILY" in
    debian) DEBIAN_FRONTEND=noninteractive apt-get install -y "$@" ;;
    rhel)   dnf install -y "$@" ;;
    arch)   pacman -S --noconfirm --needed "$@" ;;
  esac
}

# ─── PHP Setup ──────────────────────────────────────────────────────────
PHP_VER=""; PHP_SOCK=""; PHP_FPM_SVC=""
setup_php() {
  case "$DISTRO_FAMILY" in
    debian)
      pkg_install curl ca-certificates gnupg lsb-release
      if [[ ! -f /etc/apt/sources.list.d/php.list ]]; then
        curl -fsSL https://packages.sury.org/php/apt.gpg -o /etc/apt/trusted.gpg.d/php.gpg
        echo "deb https://packages.sury.org/php/ $(lsb_release -sc) main" > /etc/apt/sources.list.d/php.list
      fi
      pkg_update
      local v
      for v in 8.5 8.4; do apt-cache show "php${v}-fpm" >/dev/null 2>&1 && { PHP_VER="$v"; break; }; done
      [[ -n "$PHP_VER" ]] || die "PHP 8.5/8.4 not in SURY repo"
      pkg_install "php${PHP_VER}" "php${PHP_VER}-cli" "php${PHP_VER}-fpm" \
                  "php${PHP_VER}-mysql" "php${PHP_VER}-curl" "php${PHP_VER}-xml" \
                  "php${PHP_VER}-mbstring" "php${PHP_VER}-zip" "php${PHP_VER}-gd" \
                  "php${PHP_VER}-intl" "php${PHP_VER}-bcmath" \
                  "php${PHP_VER}-imagick" "php${PHP_VER}-fileinfo" "php${PHP_VER}-exif"
      PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
      PHP_FPM_SVC="php${PHP_VER}-fpm"
      ;;
    rhel)
      if [[ "$DISTRO_ID" == "fedora" ]]; then
        pkg_install dnf-plugins-core || true
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml php-mbstring php-zip php-gd || true
        pkg_install php-intl php-bcmath php-pecl-imagick php-process || true
      else
        dnf install -y epel-release || true
        dnf config-manager --set-enabled crb 2>/dev/null || true
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml php-mbstring php-zip php-gd || true
        pkg_install php-intl php-bcmath php-imagick php-fileinfo php-exif || true
      fi
      if ! command -v php >/dev/null 2>&1; then die "PHP installation failed on Fedora/RHEL."; fi
      PHP_VER="$(php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || echo "8.3")"
      PHP_SOCK="/run/php-fpm/www.sock"
      PHP_FPM_SVC="php-fpm"
      ;;
    arch)
      pkg_install curl ca-certificates php php-fpm php-gd php-imagick php-intl
      PHP_VER="$(pacman -Si php 2>/dev/null | awk '/^Version/{print $3}' | cut -d. -f1,2)"
      [[ -n "$PHP_VER" ]] || PHP_VER="8"
      PHP_SOCK="/run/php-fpm/php-fpm.sock"
      PHP_FPM_SVC="php-fpm"
      ;;
  esac
  echo "   → PHP $PHP_VER (socket: $PHP_SOCK)"
}

# ─── Vajra Binary Downloader ────────────────────────────────────────────
install_vajra_binary() {
  if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then return; fi
  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${VAJRA_REPO}/releases/latest" \
      | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
  fi
  echo "   → tag: $VAJRA_VERSION"
  local BASE="https://github.com/${VAJRA_REPO}/releases/download/${VAJRA_VERSION}"
  local VER_NOV="${VAJRA_VERSION#v}"
  local TMP; TMP="$(mktemp -d)"
  local -a CANDIDATES=(
    "vajra-${VAJRA_VERSION}-linux-x86_64.tar.gz"
    "vajra-${VER_NOV}-linux-x86_64.tar.gz"
    "vajra-${VAJRA_VERSION}-x86_64-linux.tar.gz"
    "vajra-${VER_NOV}-x86_64-linux.tar.gz"
    "vajra-${VAJRA_VERSION}-x86_64-unknown-linux-musl.tar.gz"
    "vajra-${VER_NOV}-x86_64-unknown-linux-musl.tar.gz"
  )
  local api_url="https://api.github.com/repos/${VAJRA_REPO}/releases/tags/${VAJRA_VERSION}"
  while IFS= read -r a; do
    [[ -z "$a" ]] && continue
    [[ "$a" == *.sha256 ]] && continue
    CANDIDATES+=("$a")
  done < <(curl -fsSL "$api_url" 2>/dev/null \
    | grep -oE '"browser_download_url"\s*:\s*"[^"]+\.tar\.gz"' \
    | sed -E 's|.*/||; s|"$||' | grep -E 'x86_64|linux' || true)
  local asset ok=0
  for asset in "${CANDIDATES[@]}"; do
    echo "   → trying: $asset"
    if curl -fL --retry 2 --connect-timeout 10 --max-time 180 -sS \
         "$BASE/$asset" -o "$TMP/$asset" 2>/dev/null; then
      if curl -fL -sS "$BASE/${asset}.sha256" -o "$TMP/${asset}.sha256" 2>/dev/null; then
        if ( cd "$TMP" && sha256sum -c "${asset}.sha256" >/dev/null 2>&1 ); then
          echo "   → checksum OK"
        else
          echo "   → checksum FAILED, skipping"
          continue
        fi
      fi
      if tar -xzf "$TMP/$asset" -C "$TMP" 2>/dev/null; then
        local SRC
        SRC=$(find "$TMP" -type f -name vajra -not -name '*.tar.gz' 2>/dev/null | head -1)
        if [[ -n "$SRC" ]]; then
          install -m 0755 "$SRC" /usr/local/bin/vajra
          echo "   → installed vajra from $asset"
          ok=1; break
        fi
      fi
    fi
  done
  rm -rf "$TMP"
  if [[ "$ok" != "1" ]]; then
    die "Could not download Vajra binary from ${BASE}.
  Tried: ${CANDIDATES[*]}
  Check https://github.com/${VAJRA_REPO}/releases/tag/${VAJRA_VERSION}"
  fi
}

# ─── WP-CLI Installer ───────────────────────────────────────────────────
install_wp_cli() {
  if ! command -v wp >/dev/null 2>&1; then
    curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o /usr/local/bin/wp
    chmod 0755 /usr/local/bin/wp
  fi
}

# ─── The `vajra-site` CLI Tool (Vajra-Only, robust) ─────────────────────
create_vajra_site_cli() {
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
# ── Dynamic web-user detection (Debian: www-data, RHEL: apache, Arch: http) ──
if id -u www-data >/dev/null 2>&1; then WWW_USER="www-data"
elif id -u apache >/dev/null 2>&1; then WWW_USER="apache"
elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"
elif id -u http >/dev/null 2>&1; then WWW_USER="http"
else WWW_USER="nobody"; fi
PHP_SOCK="__PHP_SOCK__"
APP_TOML="/etc/vajra/app.toml"
WWW_ROOT="/var/www/sites"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }
[[ $EUID -ne 0 ]] && die "Run as root"

cmd_add() {
  if [[ $# -lt 2 ]]; then
    echo "Usage: vajra-site add <domain> <email> [--admin-user <user>] [--admin-pass <pass>]"
    exit 1
  fi
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local ADMIN_USER="wpadmin"
  local ADMIN_PASS=""
  local SITE_MODE="__DEFAULT_SITE_MODE__"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --admin-user) ADMIN_USER="$2"; shift 2 ;;
      --admin-pass) ADMIN_PASS="$2"; shift 2 ;;
      --php)  SITE_MODE="php";  shift ;;
      --html) SITE_MODE="html"; shift ;;
      *) shift ;;
    esac
  done

  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"
  
  # अगर साइट पहले से इंस्टॉल है, तो दोबारा कुछ न करें
  if [[ -f "$SITE_ROOT/wp-load.php" ]]; then
      echo "✅ Site ${DOMAIN} is already installed at ${SITE_ROOT}."
      echo "   To reinstall, remove it first: rm -rf ${SITE_ROOT}"
      exit 0
  fi

  mkdir -p "$SITE_ROOT"

  log "Installing WordPress..."
  if [[ ! -f "$SITE_ROOT/wp-load.php" ]]; then
    local TMP; TMP="$(mktemp -d)"
    curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
    cp -a "$TMP/wordpress/." "$SITE_ROOT/"
    rm -rf "$TMP"
  fi
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"

  log "Getting SSL Certificate..."
  systemctl stop vajra.service 2>/dev/null || true
  certbot certonly --standalone -d "$DOMAIN" -d "www.$DOMAIN" --agree-tos --email "$EMAIL" --non-interactive || true
  systemctl start vajra.service || true

  log "Updating Vajra config (append mode)..."
  if [[ ! -f "$APP_TOML" ]]; then
    cat > "$APP_TOML" <<TOML_EOF
[server]
listen  = "0.0.0.0:80"
workers = 1
[tls]
listen  = "0.0.0.0:443"
cert    = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
key     = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
TOML_EOF
  fi
  if ! grep -q '^\[tls\]' "$APP_TOML"; then
    cat >> "$APP_TOML" <<TOML_EOF

[tls]
listen  = "0.0.0.0:443"
cert    = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
key     = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
TOML_EOF
  fi
  if ! grep -q "^host *= *\"${DOMAIN}\"" "$APP_TOML"; then
    cat >> "$APP_TOML" <<TOML_EOF

[[site]]
host     = "${DOMAIN}"
root     = "${SITE_ROOT}"
index    = "index.php"
mode     = "${SITE_MODE}"
php_sock = "unix:${PHP_SOCK}"
tls_cert = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
tls_key  = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
TOML_EOF
  else
    echo "   → site ${DOMAIN} already present — skipping"
  fi
  systemctl restart vajra.service

  # WP-CLI Install
  cd "$SITE_ROOT"
  local DB_NAME="wp_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-30)"
  local DB_USER="u_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-20)"
  local DB_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
  if [[ -z "$ADMIN_PASS" ]]; then
    ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
  fi

  db_cli() { if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi; }
  db_exec() { "$(db_cli)" "$@"; }

  db_exec -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`;"
  db_exec -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  db_exec -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';"
  db_exec -e "FLUSH PRIVILEGES;"

  wp --allow-root config create --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" --dbhost=127.0.0.1 --skip-check --force
  
  # wp-config.php को सही मालिक और अनुमति दें
  chown "$WWW_USER:$WWW_USER" wp-config.php
  chmod 644 wp-config.php
  
  # कॉन्फ़िगरेशन सेट करें (--raw सिर्फ true/false के लिए)
  wp --allow-root config set WP_HTTP_BLOCK_EXTERNAL false --raw
  wp --allow-root config set WP_ACCESSIBLE_HOSTS "*.wordpress.org,*.wp.com,${DOMAIN},127.0.0.1,localhost"
  
  # WordPress Install
  wp --allow-root core install --url="https://${DOMAIN}" --title="WordPress" --admin_user="$ADMIN_USER" --admin_password="$ADMIN_PASS" --admin_email="$EMAIL" --skip-email
  
  # सुरक्षा के लिए वापस 640 करें
  chmod 640 wp-config.php

  # Site Health Fixes
  wp --allow-root plugin delete akismet hello 2>/dev/null || true
  wp --allow-root plugin install wp-super-cache --activate 2>/dev/null || true
  
  # mu-plugin for loopback
  mkdir -p "$SITE_ROOT/wp-content/mu-plugins"
  cat > "$SITE_ROOT/wp-content/mu-plugins/vajra-loopback-fix.php" <<'MUEOF'
<?php
add_filter('https_ssl_verify', '__return_false');
add_filter('https_local_ssl_verify', '__return_false');
MUEOF
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT/wp-content/mu-plugins"

  echo ""
  echo "════════════════════════════════════════════════════════════════"
  echo "✅ Site created successfully!"
  echo "   Website:     https://${DOMAIN}"
  echo "   Admin URL:   https://${DOMAIN}/wp-admin/"
  echo "   Admin User:  ${ADMIN_USER}"
  echo "   Admin Pass:  ${ADMIN_PASS}"
  echo "   DB Name:     ${DB_NAME}"
  echo "   DB User:     ${DB_USER}"
  echo "   DB Pass:     ${DB_PASS}"
  echo "════════════════════════════════════════════════════════════════"
  echo ""

  # क्रेडेंशियल्स को एक सुरक्षित फाइल में सेव करें
  local CRED_FILE="/root/vajra-site-${DOMAIN}.txt"
  cat > "$CRED_FILE" <<CREDS
Site:       https://${DOMAIN}
Admin URL:  https://${DOMAIN}/wp-admin/
Admin User: ${ADMIN_USER}
Admin Pass: ${ADMIN_PASS}
DB Name:    ${DB_NAME}
DB User:    ${DB_USER}
DB Pass:    ${DB_PASS}
CREDS
  chmod 600 "$CRED_FILE"
  echo "🔐 Credentials saved to: ${CRED_FILE}"
  echo ""
}

case "${1:-}" in
  add) shift; cmd_add "$@";;
  *) echo "Usage: vajra-site add <domain> <email>";;
esac
VAJRA_SITE_SCRIPT

sed -i "s|__PHP_SOCK__|${PHP_SOCK}|g" /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
}

# ─── Main Flow ──────────────────────────────────────────────────────────
main() {
  log "0/6 — Pre-flight"
  detect_distro
  WWW_USER="$(www_user)"
  if ! id -u "$WWW_USER" >/dev/null 2>&1; then
    if id -u apache >/dev/null 2>&1; then
      WWW_USER="apache"
    elif id -u nginx >/dev/null 2>&1; then
      WWW_USER="nginx"
    else
      useradd -r -s /sbin/nologin -d /var/www -c "Apache" apache 2>/dev/null || true
      WWW_USER="apache"
    fi
    echo "   → adjusted www user to: $WWW_USER"
  fi

  log "1/6 — Base Packages"
  pkg_update
  pkg_install curl wget ca-certificates gnupg openssl python3 certbot || true
  
  # RHEL Family: Firewall, io_uring, SELinux Fix
  if [[ "$DISTRO_FAMILY" == "rhel" ]]; then
    # Disable Apache/Nginx if they were pulled in by PHP
    systemctl stop httpd nginx 2>/dev/null || true
    systemctl disable httpd nginx 2>/dev/null || true

    if command -v firewall-cmd >/dev/null 2>&1; then
      firewall-cmd --permanent --add-service=http >/dev/null 2>&1 || true
      firewall-cmd --permanent --add-service=https >/dev/null 2>&1 || true
      firewall-cmd --reload >/dev/null 2>&1 || true
    fi
    echo 0 > /proc/sys/kernel/io_uring_disabled 2>/dev/null || true
    echo "kernel.io_uring_disabled = 0" > /etc/sysctl.d/99-enable-io-uring.conf
    setenforce 0 2>/dev/null || true
    sed -i 's/^SELINUX=enforcing/SELINUX=permissive/' /etc/selinux/config 2>/dev/null || true
  fi

  if [[ "$DISTRO_FAMILY" == "debian" ]] || [[ "$DISTRO_FAMILY" == "rhel" ]]; then
    pkg_install mariadb-server || true
  elif [[ "$DISTRO_FAMILY" == "arch" ]]; then
    pkg_install mariadb || true
  fi
  systemctl enable --now mariadb || true

  log "2/6 — PHP Setup"
  setup_php
  
  # PHP-FPM को हमेशा चालू रखें
  systemctl enable --now "$PHP_FPM_SVC" 2>/dev/null || true
  for i in {1..30}; do
    [[ -S "$PHP_SOCK" ]] && break
    sleep 1
  done
  if [[ "$DISTRO_FAMILY" == "rhel" ]] && [[ -S "$PHP_SOCK" ]]; then
    chown "$WWW_USER:$WWW_USER" "$PHP_SOCK" 2>/dev/null || true
    chmod 660 "$PHP_SOCK" 2>/dev/null || true
  fi

  log "3/6 — Vajra Binary"
  install_vajra_binary

  log "4/6 — WP-CLI"
  install_wp_cli

  log "5/6 — Vajra Directories & Services"
  mkdir -p /etc/vajra/sites /var/log/vajra /var/www/sites /var/www/certbot
# ── WP-CLI shared cache (distro-aware) ──
mkdir -p /var/www/.wp-cli/cache
chown -R "$WWW_USER:$WWW_USER" /var/www/.wp-cli
chmod 750 /var/www/.wp-cli
  chown -R "$WWW_USER:$WWW_USER" /var/log/vajra /var/www/sites

  if [[ ! -f /etc/vajra/app.toml ]]; then
    cat > /etc/vajra/app.toml <<TOML
[server]
listen  = "0.0.0.0:80"
workers = 1
TOML
  fi

  cat > /etc/systemd/system/vajra.service <<UNIT
[Unit]
Description=Vajra web server
After=network.target ${PHP_FPM_SVC}.service
[Service]
Type=simple
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable vajra.service

  log "6/6 — Installing vajra-site CLI"
  create_vajra_site_cli

  
verify_and_fix_services() {
    log "🔍 Verifying core services (MariaDB, PHP-FPM, Vajra)..."
    
    # 1. MariaDB Check
    local db_svc="mariadb"
    if ! systemctl is-active --quiet "$db_svc"; then
        log "⚠️  MariaDB is down. Attempting to fix..."
        mkdir -p /run/mariadb /var/run/mysqld
        chown -R mysql:mysql /var/lib/mysql /run/mariadb /var/run/mysqld 2>/dev/null || true
        systemctl restart "$db_svc" 2>/dev/null || systemctl restart mysql 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet "$db_svc"; then
        die "❌ MariaDB could not be started. Please check logs."
    fi
    log "✅ MariaDB is running."

    # 2. PHP-FPM Check
    local php_svc="php-fpm"
    
    # Universal Check: अगर डिफ़ॉल्ट php-fpm नहीं है, तो php*-fpm (जैसे Debian में php8.3-fpm) खोजें
    if ! systemctl list-unit-files "php-fpm.service" --no-legend 2>/dev/null | grep -q "php-fpm.service"; then
        local found_php=$(ls /usr/lib/systemd/system/php*-fpm.service /lib/systemd/system/php*-fpm.service 2>/dev/null | head -1)
        if [ -n "$found_php" ]; then
            php_svc=$(basename "$found_php" .service)
        fi
    fi
    
    if ! systemctl is-active --quiet "$php_svc"; then
        log "⚠️  PHP-FPM ($php_svc) is down. Attempting to fix..."
        mkdir -p /run/php-fpm /run/php
        chown -R "$WWW_USER:$WWW_USER" /run/php-fpm /run/php 2>/dev/null || true
        if command -v setsebool >/dev/null; then
            setsebool -P httpd_can_network_connect 1 2>/dev/null || true
            setsebool -P httpd_can_network_connect_db 1 2>/dev/null || true
        fi
        systemctl restart "$php_svc" 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet "$php_svc"; then
        die "❌ PHP-FPM could not be started. Please check logs."
    fi
    log "✅ PHP-FPM is running."

    # Socket presence check
    local _sock="${PHP_SOCK:-}"
    if [[ -z "$_sock" ]] || [[ ! -S "$_sock" ]]; then
        log "⚠️  PHP-FPM socket missing ($_sock) — waiting up to 15s..."
        for _i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
            [[ -S "$_sock" ]] && break
            sleep 1
        done
    fi
    if [[ -z "$_sock" ]] || [[ ! -S "$_sock" ]]; then
        die "❌ PHP-FPM socket never appeared ($_sock). Check: journalctl -u ${php_svc:-php-fpm}"
    fi
    log "✅ PHP-FPM socket present: $_sock"

    # 3. Vajra Check
    if ! systemctl is-active --quiet vajra; then
        log "⚠️  Vajra Web Server is down. Attempting to fix..."
        systemctl reset-failed vajra 2>/dev/null || true
        systemctl restart vajra 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet vajra; then
        die "❌ Vajra Web Server could not be started. Please check logs."
    fi
    log "✅ Vajra Web Server is running."
}

verify_and_fix_services
echo "✅ Vajra Stack Installed!"
  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" "${@:3}" || true
  fi
}

main "$@"
# Updated installer with binary fix
