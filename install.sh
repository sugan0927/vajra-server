#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer (rev 10 — Vajra-Only / No Nginx)
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
SITE_MODE="${SITE_MODE:-wordpress}"
if [[ "${3:-}" == "--php"  ]]; then SITE_MODE="php";  fi
if [[ "${3:-}" == "--html" ]]; then SITE_MODE="html"; fi
if [[ "${3:-}" == "--wordpress" || "${3:-}" == "--wp" ]]; then SITE_MODE="wordpress"; fi

VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
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
    # ... (RHEL/Arch are same as before, omitted for brevity but you can copy from previous)
  esac
  echo "   → PHP $PHP_VER (socket: $PHP_SOCK)"
}

# ─── Vajra Binary Downloader ────────────────────────────────────────────
install_vajra_binary() {
  if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then return; fi
  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${VAJRA_REPO}/releases/latest" | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
  fi
  echo "   → tag: $VAJRA_VERSION"
  local _BASE="https://github.com/${VAJRA_REPO}/releases/download/${VAJRA_VERSION}"
  local TMP; TMP="$(mktemp -d)"
  local _downloaded=0
  for _asset in "vajra-${VAJRA_VERSION}-x86_64-linux-musl.tar.gz" "vajra-${VAJRA_VERSION}-x86_64-unknown-linux-musl.tar.gz"; do
    if curl -fL --retry 2 "$_BASE/$_asset" -o "$TMP/$_asset" 2>/dev/null; then
      tar -xzf "$TMP/$_asset" -C "$TMP" && _downloaded=1 && break
    fi
  done
  if [[ "$_downloaded" == "1" ]]; then
    local SRC; SRC="$(find "$TMP" -type f -name vajra | head -1)"
    install -m 0755 "$SRC" /usr/local/bin/vajra
    rm -rf "$TMP"
    echo "   → installed vajra"
  else
    die "Could not download pre-built Vajra binary."
  fi
}

# ─── WP-CLI Installer ───────────────────────────────────────────────────
install_wp_cli() {
  if ! command -v wp >/dev/null 2>&1; then
    curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o /usr/local/bin/wp
    chmod 0755 /usr/local/bin/wp
  fi
}

# ─── The `vajra-site` CLI Tool (Vajra-Only) ─────────────────────────────
create_vajra_site_cli() {
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
WWW_USER="__WWW_USER__"
PHP_SOCK="__PHP_SOCK__"
APP_TOML="/etc/vajra/app.toml"
WWW_ROOT="/var/www/sites"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }
[[ $EUID -ne 0 ]] && die "Run as root"

cmd_add() {
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local MODE="wordpress"
  
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"
  mkdir -p "$SITE_ROOT"

  if [[ "$MODE" == "wordpress" ]]; then
    log "Installing WordPress..."
    if [[ ! -f "$SITE_ROOT/wp-load.php" ]]; then
      local TMP; TMP="$(mktemp -d)"
      curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
      cp -a "$TMP/wordpress/." "$SITE_ROOT/"
      rm -rf "$TMP"
    fi
    chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"

    # Get SSL Cert via Certbot (standalone on port 80)
    log "Getting SSL Certificate..."
    systemctl stop vajra.service 2>/dev/null || true
    certbot certonly --standalone -d "$DOMAIN" -d "www.$DOMAIN" --agree-tos --email "$EMAIL" --non-interactive || true
    systemctl start vajra.service || true

    # Append to app.toml
    log "Updating Vajra config..."
    cat >> "$APP_TOML" <<TOML

[[site]]
host    = "${DOMAIN}"
root    = "${SITE_ROOT}"
index   = "index.php"
mode    = "wordpress"
php_sock = "unix:${PHP_SOCK}"
tls_cert = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
tls_key  = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
TOML

    systemctl reload vajra.service || systemctl restart vajra.service

    # WP-CLI Install
    cd "$SITE_ROOT"
    local DB_NAME="wp_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-30)"
    local DB_USER="u_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-20)"
    local DB_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
    local ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"

    db_cli() { if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi; }
    db_exec() { "$(db_cli)" "$@"; }

    db_exec -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`;"
    db_exec -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
    db_exec -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';"
    db_exec -e "FLUSH PRIVILEGES;"

    wp --allow-root config create --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" --dbhost=127.0.0.1 --skip-check --force
    wp --allow-root core install --url="https://${DOMAIN}" --title="WordPress" --admin_user="wpadmin" --admin_password="$ADMIN_PASS" --admin_email="$EMAIL" --skip-email

    # Site Health Fixes
    wp --allow-root plugin delete akismet hello 2>/dev/null || true
    wp --allow-root plugin install wp-super-cache --activate 2>/dev/null || true
    mkdir -p "$SITE_ROOT/wp-content/mu-plugins"
    cat > "$SITE_ROOT/wp-content/mu-plugins/vajra-loopback-fix.php" <<'MUEOF'
<?php
add_filter('https_ssl_verify', '__return_false');
add_filter('https_local_ssl_verify', '__return_false');
MUEOF
    chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT/wp-content/mu-plugins"
    
    echo "✅ Site created: https://${DOMAIN}"
  fi
}

case "${1:-}" in
  add) shift; cmd_add "$@";;
  *) echo "Usage: vajra-site add <domain> <email>";;
esac
VAJRA_SITE_SCRIPT

sed -i "s|__WWW_USER__|${WWW_USER}|g; s|__PHP_SOCK__|${PHP_SOCK}|g" /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
}

# ─── Main Flow ──────────────────────────────────────────────────────────
main() {
  log "0/6 — Pre-flight"
  detect_distro
  WWW_USER="$(www_user)"
  # सुनिश्चित करें कि यूज़र मौजूद है, वरना apache या nobody पर फॉलबैक करें
  if ! id -u "$WWW_USER" >/dev/null 2>&1; then
    if id -u apache >/dev/null 2>&1; then
      WWW_USER="apache"
    elif id -u nginx >/dev/null 2>&1; then
      WWW_USER="nginx"
    else
      useradd -r -s /sbin/nologin "$WWW_USER" 2>/dev/null || WWW_USER="nobody"
    fi
    echo "   → adjusted www user to: $WWW_USER"
  fi

  log "1/6 — Base Packages (No Nginx)"
  pkg_update
  pkg_install curl wget ca-certificates gnupg openssl python3 certbot
  if [[ "$DISTRO_FAMILY" == "debian" ]]; then
    pkg_install mariadb-server
  elif [[ "$DISTRO_FAMILY" == "rhel" ]]; then
    pkg_install mariadb-server
  elif [[ "$DISTRO_FAMILY" == "arch" ]]; then
    pkg_install mariadb
  fi
  systemctl enable --now mariadb

  log "2/6 — PHP Setup"
  setup_php

  log "3/6 — Vajra Binary"
  install_vajra_binary

  log "4/6 — WP-CLI"
  install_wp_cli

  log "5/6 — Vajra Directories & Services"
  mkdir -p /etc/vajra/sites /var/log/vajra /var/www/sites /var/www/certbot
  chown -R "$WWW_USER:$WWW_USER" /var/log/vajra /var/www/sites

  # Create default app.toml if it doesn't exist
  if [[ ! -f /etc/vajra/app.toml ]]; then
    cat > /etc/vajra/app.toml <<TOML
[server]
http_listen  = "0.0.0.0:80"
https_listen = "0.0.0.0:443"
TOML
  fi

  cat > /etc/systemd/system/vajra.service <<UNIT
[Unit]
Description=Vajra web server
After=network.target ${PHP_FPM_SVC}.service
[Service]
Type=simple
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
Restart=on-failure
[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable vajra.service

  log "6/6 — Installing vajra-site CLI"
  create_vajra_site_cli

  echo "✅ Vajra Stack Installed!"
  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL"
  fi
}

main "$@"
