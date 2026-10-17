#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer (rev 9 — WP Site Health Auto-Fix)
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
MARIADB_VERSION="${MARIADB_VERSION:-mariadb-11.4}"

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
    rhel)   echo "nginx"    ;;
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

# ─── PHP Setup (With All Required Modules) ──────────────────────────────
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
      # Added imagick, intl, fileinfo, exif
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
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml \
                    php-mbstring php-zip php-gd php-intl php-bcmath \
                    php-imagick php-fileinfo php-exif
      else
        dnf install -y "https://rpms.remirepo.net/enterprise/remi-release-$(rpm -E %rhel).rpm" || true
        dnf module reset php -y 2>/dev/null || true
        dnf module enable php:remi-8.4 -y || true
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml \
                    php-mbstring php-zip php-gd php-intl php-bcmath \
                    php-imagick php-fileinfo php-exif
      fi
      PHP_VER="$(php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || echo "8.4")"
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

# ─── The `vajra-site` CLI Tool ──────────────────────────────────────────
create_vajra_site_cli() {
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
DISTRO_FAMILY="__FAMILY__"
WWW_USER="__WWW_USER__"
PHP_SOCK="__PHP_SOCK__"
PHP_VER="__PHP_VER__"
PHP_FPM_SVC="__PHP_FPM_SVC__"
SITES_DIR=/etc/vajra/sites
WWW_ROOT=/var/www/sites
CERTBOT_ROOT=/var/www/certbot
FIRST_PORT=8080

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }
[[ $EUID -ne 0 ]] && die "Run as root"

db_cli() { if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi; }
db_exec() { "$(db_cli)" "$@"; }
read_env() { sed -n "s/^${2}=//p" "$1" | head -1; }
next_port() {
  local used; used="$(grep -h '^PORT=' "${SITES_DIR}"/*.env 2>/dev/null | cut -d= -f2 | sort -n | tail -1 || true)"
  echo $(( ${used:-$((FIRST_PORT-1))} + 1 ))
}

cmd_add() {
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local MODE="wordpress"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --php) MODE="php"; shift;;
      --html) MODE="html"; shift;;
      *) shift;;
    esac
  done

  local WWW_ALIAS="www.${DOMAIN}"
  local PORT; PORT="$(next_port)"
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

    local DB_NAME="wp_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-30)"
    local DB_USER="u_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-20)"
    local DB_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
    local ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"

    db_exec -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`;"
    db_exec -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
    db_exec -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';"
    db_exec -e "FLUSH PRIVILEGES;"

    cd "$SITE_ROOT"
    wp --allow-root config create --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" --dbhost=127.0.0.1 --skip-check --force
    wp --allow-root db create 2>/dev/null || true
    wp --allow-root core install --url="https://${DOMAIN}" --title="WordPress" --admin_user="wpadmin" --admin_password="$ADMIN_PASS" --admin_email="$EMAIL" --skip-email

    # ─── START OF SITE HEALTH AUTO-FIXES ───
    log "Applying Site Health auto-fixes..."

    # 1. Delete default inactive plugins and themes
    wp --allow-root plugin delete akismet hello 2>/dev/null || true
    wp --allow-root theme delete twentytwentythree twentytwentytwo 2>/dev/null || true

    # 2. Install a lightweight page cache
    wp --allow-root plugin install wp-super-cache --activate 2>/dev/null || true

    # 3. Create Loopback mu-plugin to bypass SSL verification for local requests
    mkdir -p "$SITE_ROOT/wp-content/mu-plugins"
    cat > "$SITE_ROOT/wp-content/mu-plugins/vajra-loopback-fix.php" <<'MUEOF'
<?php
// Vajra: Bypass SSL verification for local loopback requests.
add_filter('https_ssl_verify', '__return_false');
add_filter('https_local_ssl_verify', '__return_false');
MUEOF
    chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT/wp-content/mu-plugins"
    # ─── END OF SITE HEALTH AUTO-FIXES ───

  elif [[ "$MODE" == "php" ]]; then
    echo "<?php echo 'PHP is working on $DOMAIN';" > "$SITE_ROOT/index.php"
    chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"
  elif [[ "$MODE" == "html" ]]; then
    echo "<h1>$DOMAIN is live</h1>" > "$SITE_ROOT/index.html"
    chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"
  fi

  mkdir -p "${SITES_DIR}"
  cat > "${SITES_DIR}/${DOMAIN}.toml" <<TOML
[server]
listen  = "127.0.0.1:${PORT}"
workers = 1
[static]
root  = "${SITE_ROOT}"
index = "index.php"
[php]
upstream         = "unix:${PHP_SOCK}"
index            = ["index.php"]
TOML

  systemctl enable --now "vajra-site@${DOMAIN}.service"
  echo "✅ Site created: https://${DOMAIN}"
}

case "${1:-}" in
  add) shift; cmd_add "$@";;
  *) echo "Usage: vajra-site add <domain> <email> [--php|--html]";;
esac
VAJRA_SITE_SCRIPT

# Replace placeholders
sed -i "s|__FAMILY__|${DISTRO_FAMILY}|g; s|__WWW_USER__|${WWW_USER}|g; s|__PHP_SOCK__|${PHP_SOCK}|g; s|__PHP_VER__|${PHP_VER}|g; s|__PHP_FPM_SVC__|${PHP_FPM_SVC}|g" /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
}

# ─── Main Flow ──────────────────────────────────────────────────────────
main() {
  log "0/6 — Pre-flight"
  detect_distro
  WWW_USER="$(www_user)"

  log "1/6 — Base Packages"
  pkg_update
  pkg_install curl wget ca-certificates gnupg openssl python3 nginx certbot
  if [[ "$DISTRO_FAMILY" == "debian" ]]; then
    pkg_install mariadb-server
  elif [[ "$DISTRO_FAMILY" == "rhel" ]]; then
    pkg_install mariadb-server
  elif [[ "$DISTRO_FAMILY" == "arch" ]]; then
    pkg_install mariadb
  fi
  systemctl enable --now mariadb nginx

  log "2/6 — PHP Setup"
  setup_php

  log "3/6 — Vajra Binary"
  install_vajra_binary

  log "4/6 — WP-CLI"
  install_wp_cli

  log "5/6 — Vajra Directories & Services"
  mkdir -p /etc/vajra/sites /var/log/vajra /var/www/sites /var/www/certbot
  chown -R "$WWW_USER:$WWW_USER" /var/log/vajra /var/www/sites

  cat > /etc/systemd/system/vajra-site@.service <<UNIT
[Unit]
Description=Vajra web server for %i
After=network.target ${PHP_FPM_SVC}.service
[Service]
Type=simple
ExecStart=/usr/local/bin/vajra --config /etc/vajra/sites/%i.toml
Restart=on-failure
[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload

  log "6/6 — Installing vajra-site CLI"
  create_vajra_site_cli

  echo "✅ Vajra Stack Installed!"
  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" --$SITE_MODE
  fi
}

main "$@"
