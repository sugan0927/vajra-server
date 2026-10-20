#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer (v13 — Ultimate Universal Fix)
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

# ─── Retune mode detection (does not occupy positional arg slots) ───
RETUNE_ONLY=0
for _arg in "$@"; do
    case "$_arg" in
        --retune|-r) RETUNE_ONLY=1 ;;
    esac
done
unset _arg


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
# ── Dynamic web-user detection ──
if id -u www-data >/dev/null 2>&1; then WWW_USER="www-data"
elif id -u apache >/dev/null 2>&1; then WWW_USER="apache"
elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"
elif id -u http >/dev/null 2>&1; then WWW_USER="http"
else WWW_USER="nobody"; fi

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

# ── Dynamic web-user detection ──
if id -u www-data >/dev/null 2>&1; then WWW_USER="www-data"
elif id -u apache >/dev/null 2>&1; then WWW_USER="apache"
elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"
elif id -u http >/dev/null 2>&1; then WWW_USER="http"
else WWW_USER="nobody"; fi

PHP_SOCK="__PHP_SOCK__"
APP_TOML="/etc/vajra/app.toml"
WWW_ROOT="/var/www/sites"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }
[[ $EUID -ne 0 ]] && die "Run as root"

cmd_add() {
  if [[ $# -lt 2 ]]; then
    echo "Usage: vajra-site add <domain> <email> [--php|--html] [--admin-user <user>] [--admin-pass <pass>]"
    exit 1
  fi
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local ADMIN_USER="wpadmin"
  local ADMIN_PASS=""
  local SITE_MODE="wordpress"

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
  mkdir -p "$SITE_ROOT"

  # Idempotency: skip if already installed
  if [[ -f "$SITE_ROOT/wp-load.php" ]]; then
    echo "✅ Site ${DOMAIN} is already installed at ${SITE_ROOT}."
    echo "   To reinstall, remove it first: rm -rf ${SITE_ROOT}"
    exit 0
  fi

  log "Installing WordPress..."
  local TMP; TMP="$(mktemp -d)"
  curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
  cp -a "$TMP/wordpress/." "$SITE_ROOT/"
  rm -rf "$TMP"

  log "Getting SSL Certificate..."
  systemctl stop vajra.service 2>/dev/null || true

  if [[ -d "/etc/letsencrypt/live/${DOMAIN}" ]]; then
      echo "   → SSL Certificate for ${DOMAIN} already exists. Skipping certbot."
  else
      # STRICT: die on failure — do not continue with broken config
      if ! certbot certonly --standalone -d "$DOMAIN" -d "www.$DOMAIN" \
           --agree-tos --email "$EMAIL" --non-interactive; then
          systemctl restart vajra.service || true
          die "❌ SSL Certificate generation failed for ${DOMAIN}. Check DNS / Let's Encrypt rate limits."
      fi
  fi
  systemctl restart vajra.service || true

  log "Updating Vajra config (append mode)..."
  if [[ ! -f "$APP_TOML" ]]; then
    cat > "$APP_TOML" <<TOML_EOF
[server]
listen  = "0.0.0.0:80"
workers = 1

[logging]
access_log = "/var/log/vajra/access.log"
format = "text"

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

  if grep -q "^host *= *\"${DOMAIN}\"" "$APP_TOML"; then
    echo "   → site ${DOMAIN} already present — skipping"
  else
    cat >> "$APP_TOML" <<TOML_EOF


[object_cache]
enabled = true
listen = "127.0.0.1:6379"
max_memory_mb = 128


[security]
enabled = true
max_conns_per_ip = 10000
auto_ban_enabled = true
ban_duration_secs = 600
max_errors_per_minute = 50

[[site]]
host     = "${DOMAIN}"
root     = "${SITE_ROOT}"
index    = "index.php"
mode     = "${SITE_MODE}"
php_sock = "unix:${PHP_SOCK}"
tls_cert = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
tls_key  = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
TOML_EOF
  fi

  # Validate: no unresolved placeholders before restarting Vajra
  if grep -q '__[A-Z_]*__' "$APP_TOML"; then
    die "❌ app.toml contains unresolved placeholders (e.g. __DEFAULT_SITE_MODE__). Aborting before Vajra restart."
  fi

  systemctl restart vajra.service

  # ── WP-CLI Install ──
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

  wp --allow-root config create --dbname="$DB_NAME" --dbuser="$DB_USER" \
     --dbpass="$DB_PASS" --dbhost=127.0.0.1 --skip-check --force
  wp --allow-root config set WP_HTTP_BLOCK_EXTERNAL false --raw
  wp --allow-root config set WP_ACCESSIBLE_HOSTS "*.wordpress.org,*.wp.com,${DOMAIN},127.0.0.1,localhost"
  wp --allow-root core install --url="https://${DOMAIN}" --title="WordPress" \
     --admin_user="$ADMIN_USER" --admin_password="$ADMIN_PASS" \
     --admin_email="$EMAIL" --skip-email

  wp --allow-root plugin delete akismet hello 2>/dev/null || true
  wp --allow-root plugin install wp-super-cache --activate 2>/dev/null || true

  mkdir -p "$SITE_ROOT/wp-content/mu-plugins"
  cat > "$SITE_ROOT/wp-content/mu-plugins/vajra-loopback-fix.php" <<'MUEOF'
<?php
add_filter('https_ssl_verify', '__return_false');
add_filter('https_local_ssl_verify', '__return_false');
MUEOF

  # ── CRITICAL: Final ownership + permissions (single authoritative pass) ──
  log "Fixing file ownership and permissions (critical for CentOS/RHEL)..."
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"
  find "$SITE_ROOT" -type d -exec chmod 755 {} \;
  find "$SITE_ROOT" -type f -exec chmod 644 {} \;
  chmod 640 "$SITE_ROOT/wp-config.php" 2>/dev/null || true

  # SELinux context restore (RHEL/CentOS only)
  if command -v restorecon >/dev/null 2>&1; then
    restorecon -R "$SITE_ROOT" 2>/dev/null || true
    echo "   → SELinux context restored for $SITE_ROOT"
  fi

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

# Only substitute PHP_SOCK (no more WWW_USER or SITE_MODE placeholders)
sed -i "s|__PHP_SOCK__|${PHP_SOCK}|g" /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
}

# ─── Main Flow ──────────────────────────────────────────────────────────
# ═══════════════════════════════════════════════════════════════════════
# System tuning for high-traffic production (v0.15.9+)
# RAM/nproc-aware — safe on 512MB VPS to 8GB VPS
# Idempotent: re-running will not break anything
# ═══════════════════════════════════════════════════════════════════════
tune_system() {
    log "System tuning (RAM/nproc-aware)..."

    # ─── Read system resources ───
    local NPROC=$(nproc)
    local RAM_MB=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)

    # ─── PHP-FPM max_children (RAM-based) ───
    local MAX_CHILDREN
    if   [ "$RAM_MB" -ge 4096 ]; then MAX_CHILDREN=32
    elif [ "$RAM_MB" -ge 2048 ]; then MAX_CHILDREN=16
    elif [ "$RAM_MB" -ge 1024 ]; then MAX_CHILDREN=8
    else                              MAX_CHILDREN=4
    fi
    echo "   → nproc=$NPROC RAM=${RAM_MB}MB → PHP max_children=$MAX_CHILDREN"

    # ─── Redis max_memory (25% RAM, min 256, max 2048 MB) ───
    local REDIS_MB=$((RAM_MB / 4))
    [ "$REDIS_MB" -lt 256  ] && REDIS_MB=256
    [ "$REDIS_MB" -gt 2048 ] && REDIS_MB=2048

    # ─── Cache size (12.5% RAM, min 1024, max 4096 MB) ───
    local CACHE_MB=$((RAM_MB / 8))
    [ "$CACHE_MB" -lt 1024 ] && CACHE_MB=1024
    [ "$CACHE_MB" -gt 4096 ] && CACHE_MB=4096

    # ─── Kernel sysctl (universal, safe on all distros) ───
    SYSCTL_DIR=/etc/sysctl.d
    [ -d "$SYSCTL_DIR" ] || SYSCTL_DIR=/etc/sysctl.conf

    cat > "$SYSCTL_DIR/99-vajra-tuning.conf" <<'SYSCTL_EOF'
# Vajra production kernel tuning
# Safe for 512MB+ VPS, includes BBR (kernel >= 4.9)

# Connection queues
net.core.somaxconn = 65535
net.core.netdev_max_backlog = 65535
net.ipv4.tcp_max_syn_backlog = 65535

# TIME_WAIT reuse (critical for many short connections)
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15

# Keepalive
net.ipv4.tcp_keepalive_time = 300
net.ipv4.tcp_keepalive_intvl = 30
net.ipv4.tcp_keepalive_probes = 5

# File descriptors
fs.file-max = 1000000
fs.nr_open = 1000000

# BBR congestion control (auto-detected)
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
SYSCTL_EOF

    # Apply safely (BBR fallback if kernel too old)
    sysctl --system 2>/dev/null || true
    if ! sysctl net.ipv4.tcp_congestion_control 2>/dev/null | grep -q bbr; then
        sed -i '/tcp_congestion_control/d; /default_qdisc/d' "$SYSCTL_DIR/99-vajra-tuning.conf" 2>/dev/null || true
        echo "   ⚠ BBR unavailable (kernel < 4.9?) — skipped BBR, kept other tuning"
    else
        echo "   ✓ BBR enabled"
    fi

    # ─── Update Vajra app.toml (workers, cache, redis) ───
    if [ -f /etc/vajra/app.toml ]; then
        # workers = nproc
        sed -i "s/^workers\s*=.*/workers = $NPROC/" /etc/vajra/app.toml

        # Redis memory
        sed -i "s/^max_memory_mb\s*=.*/max_memory_mb = $REDIS_MB/" /etc/vajra/app.toml

        # Cache size + TTL
        sed -i "s/^max_size_mb\s*=.*/max_size_mb = $CACHE_MB/" /etc/vajra/app.toml
        sed -i "s/^default_ttl_secs\s*=.*/default_ttl_secs = 21600/" /etc/vajra/app.toml

        echo "   ✓ app.toml: workers=$NPROC, redis=${REDIS_MB}MB, cache=${CACHE_MB}MB/6h"
    fi

    # ─── systemd LimitNOFILE ───
    mkdir -p /etc/systemd/system/vajra.service.d
    cat > /etc/systemd/system/vajra.service.d/limits.conf <<'SVC_EOF'
[Service]
LimitNOFILE=1000000
LimitNPROC=65535
LimitCORE=0
SVC_EOF
    systemctl daemon-reload
    echo "   ✓ systemd LimitNOFILE=1000000"

    # ─── PHP-FPM pool tuning (per family) ───
    local php_pool_dirs=()
    case "$DISTRO_FAMILY" in
        debian) php_pool_dirs=(/etc/php/*/fpm/pool.d) ;;
        rhel)   php_pool_dirs=(/etc/php-fpm.d) ;;
        arch)   php_pool_dirs=(/etc/php/php-fpm.d) ;;
    esac

    local tuned_any=0
    for dir in "${php_pool_dirs[@]}"; do
        [ -d "$dir" ] || continue
        for conf in "$dir"/*.conf; do
            [ -f "$conf" ] || continue
            # Only touch vajra pools
            if grep -q 'vajra' "$conf" 2>/dev/null; then
                sed -i "s/^pm\.max_children\s*=.*/pm.max_children = $MAX_CHILDREN/" "$conf"
                sed -i "s/^pm\.max_requests\s*=.*/pm.max_requests = 1000/" "$conf"
                # Ensure pm=dynamic exists
                if ! grep -q '^pm\s*=' "$conf"; then
                    sed -i "s/^\[vajra-.*\]/[&]\npm = dynamic\npm.max_children = $MAX_CHILDREN\npm.start_servers = $((MAX_CHILDREN/4))\npm.min_spare_servers = $((MAX_CHILDREN/4))\npm.max_spare_servers = $((MAX_CHILDREN/2))\npm.max_requests = 1000/" "$conf"
                fi
                tuned_any=1
            fi
        done
    done

    if [ "$tuned_any" -eq 1 ]; then
        # Restart PHP-FPM
        local phpsvc=$(systemctl list-units --type=service --state=running 2>/dev/null | grep -oE 'php[0-9.]*-fpm|php-fpm' | head -1)
        [ -n "$phpsvc" ] && systemctl restart "$phpsvc" 2>/dev/null || true
        echo "   ✓ PHP-FPM max_children=$MAX_CHILDREN"
    else
        echo "   ⚠ No vajra PHP-FPM pools found — will tune after site add"
    fi

    log "✅ System tuning applied"
}

# ═══════════════════════════════════════════════════════════════════════
# Production app.toml sections (rate_limit, access, PHP-FPM spare)
# Idempotent: only adds what's missing, never overwrites
# ═══════════════════════════════════════════════════════════════════════
# ═══════════════════════════════════════════════════════════════════════
# Re-tune PHP-FPM after a site is added (fixes "No vajra pools" warning
# that appears on first install when no [[site]] exists yet)
# ═══════════════════════════════════════════════════════════════════════
retune_php_fpm_after_site() {
    # Read RAM to pick max_children (same logic as tune_system)
    local RAM_MB=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
    local MAX_CHILDREN
    if   [ "$RAM_MB" -ge 4096 ]; then MAX_CHILDREN=32
    elif [ "$RAM_MB" -ge 2048 ]; then MAX_CHILDREN=16
    elif [ "$RAM_MB" -ge 1024 ]; then MAX_CHILDREN=8
    else                              MAX_CHILDREN=4
    fi

    local php_pool_dirs=()
    case "$DISTRO_FAMILY" in
        debian) php_pool_dirs=(/etc/php/*/fpm/pool.d) ;;
        rhel)   php_pool_dirs=(/etc/php-fpm.d) ;;
        arch)   php_pool_dirs=(/etc/php/php-fpm.d) ;;
    esac

    local tuned=0
    for dir in "${php_pool_dirs[@]}"; do
        [ -d "$dir" ] || continue
        for conf in "$dir"/vajra-*.conf; do
            [ -f "$conf" ] || continue
            sed -i "s/^pm\.max_children\s*=.*/pm.max_children = $MAX_CHILDREN/" "$conf" 2>/dev/null || true
            sed -i "s/^pm\.max_requests\s*=.*/pm.max_requests = 1000/" "$conf" 2>/dev/null || true
            if ! grep -q '^pm\.min_spare_servers' "$conf"; then
                sed -i '/^pm\.start_servers/a pm.min_spare_servers = 2\npm.max_spare_servers = 6' "$conf" 2>/dev/null || true
            fi
            tuned=1
        done
    done

    if [ "$tuned" -eq 1 ]; then
        local phpsvc=$(systemctl list-units --type=service --state=running 2>/dev/null | grep -oE 'php[0-9.]*-fpm|php-fpm' | head -1)
        [ -n "$phpsvc" ] && systemctl restart "$phpsvc" 2>/dev/null || true
        log "   ✓ PHP-FPM retuned: max_children=$MAX_CHILDREN for vajra pools"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Install /usr/local/bin/vajra-retune — one-command re-tune for existing
# installations. Fetches latest install.sh and re-applies tuning only.
# Idempotent: safe to run any number of times.
# ═══════════════════════════════════════════════════════════════════════
install_retune_wrapper() {
    cat > /usr/local/bin/vajra-retune <<'RETUNE_WRAPPER_EOF'
#!/usr/bin/env bash
# vajra-retune — re-apply production tuning on existing Vajra install
# Idempotent: safe to run multiple times
# Usage: sudo vajra-retune [--offline]
set -euo pipefail

VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
CACHE="/var/cache/vajra/install.sh"
URL="https://raw.githubusercontent.com/${VAJRA_REPO}/main/install.sh"

if [ "$(id -u)" -ne 0 ]; then
    echo "❌ vajra-retune: must run as root"
    exit 1
fi

if [ ! -f /etc/vajra/app.toml ]; then
    echo "❌ vajra-retune: /etc/vajra/app.toml not found"
    echo "   Vajra is not installed."
    echo "   Run the installer:"
    echo "   curl -fsSL ${URL} | sudo bash"
    exit 1
fi

mkdir -p "$(dirname "$CACHE")"

# Refresh cache if older than 24h (or --offline skips fetch)
if [ "${1:-}" != "--offline" ]; then
    if [ ! -f "$CACHE" ] || [ -z "$(find "$CACHE" -mmin -1440 2>/dev/null)" ]; then
        echo "▶ Fetching latest installer from GitHub..."
        if curl -fsSL --max-time 15 "$URL" -o "$CACHE.tmp" 2>/dev/null; then
            mv "$CACHE.tmp" "$CACHE"
            echo "▶ Fetched: $CACHE ($(wc -l < "$CACHE") lines)"
        else
            rm -f "$CACHE.tmp"
            if [ -f "$CACHE" ]; then
                echo "⚠ Fetch failed, using stale cache: $CACHE"
            else
                echo "❌ Fetch failed and no cache available"
                exit 1
            fi
        fi
    fi
fi

# Fallback: local install.sh copy if cache empty
if [ ! -f "$CACHE" ] && [ -f /opt/vajra-server/install.sh ]; then
    cp /opt/vajra-server/install.sh "$CACHE"
    echo "▶ Using local /opt/vajra-server/install.sh"
fi

if [ ! -f "$CACHE" ]; then
    echo "❌ No installer available (offline, no cache, no local copy)"
    exit 1
fi

exec bash "$CACHE" --retune
RETUNE_WRAPPER_EOF
    chmod +x /usr/local/bin/vajra-retune
    echo "   ✓ /usr/local/bin/vajra-retune installed"
}

ensure_app_toml_sections() {
    log "Ensuring production app.toml sections..."

    if [ ! -f /etc/vajra/app.toml ]; then
        warn "app.toml missing — skipping section ensure (will be created on first site add)"
        return 0
    fi

    # ─── [rate_limit] — safe defaults, won't trigger for real users ───
    if ! grep -q '^\[rate_limit\]' /etc/vajra/app.toml; then
        cat >> /etc/vajra/app.toml <<'RATE_LIMIT_EOF'

[rate_limit]
enabled = true
requests_per_second = 200
burst = 400
whitelist_ips = ["127.0.0.1/32", "::1/128"]
max_tracked_ips = 100000
cleanup_interval_secs = 300
aggregate_ipv6_64 = true

# Aggressive limits for known attack paths (optional, uncomment to enable)
# [[rate_limit.overrides]]
# path = "/wp-login.php"
# requests_per_second = 5
# burst = 10
#
# [[rate_limit.overrides]]
# path = "/xmlrpc.php"
# requests_per_second = 2
# burst = 5
RATE_LIMIT_EOF
        echo "   ✓ [rate_limit] added (200 rps / 400 burst — CDN safe)"
    else
        echo "   ℹ [rate_limit] already present — skipped"
    fi

    # ─── [access] — empty trusted_proxies, Cloudflare IPs commented ───
    if ! grep -q '^\[access\]' /etc/vajra/app.toml; then
        cat >> /etc/vajra/app.toml <<'ACCESS_EOF'

[access]
trusted_proxies = []

# ─── Cloudflare IP ranges (uncomment + remove `trusted_proxies = []` above) ───
# Only needed if Vajra is behind Cloudflare (orange cloud). Without this,
# Cloudflare-proxied traffic will be rate-limited on the edge IPs.
#trusted_proxies = [
#    "173.245.48.0/20",   "103.21.244.0/22",  "103.22.200.0/22",
#    "103.31.4.0/22",     "141.101.64.0/18",  "108.162.192.0/18",
#    "190.93.240.0/20",   "188.114.96.0/20",  "197.234.240.0/22",
#    "198.41.128.0/17",   "162.158.0.0/15",   "104.16.0.0/13",
#    "104.24.0.0/14",     "172.64.0.0/13",    "131.0.72.0/22",
#    "2400:cb00::/32",    "2606:4700::/32",   "2803:f800::/32",
#    "2405:b500::/32",    "2405:8100::/32",   "2a06:98c0::/29",
#    "2c0f:f248::/32",
#]
ACCESS_EOF
        echo "   ✓ [access] added (empty; Cloudflare IPs ready to uncomment)"
    else
        echo "   ℹ [access] already present — skipped"
    fi

    # ─── PHP-FPM spare servers fix (pm=dynamic needs these) ───
    local php_pool_dirs=()
    case "$DISTRO_FAMILY" in
        debian) php_pool_dirs=(/etc/php/*/fpm/pool.d) ;;
        rhel)   php_pool_dirs=(/etc/php-fpm.d) ;;
        arch)   php_pool_dirs=(/etc/php/php-fpm.d) ;;
    esac

    local tuned_any=0
    for dir in "${php_pool_dirs[@]}"; do
        [ -d "$dir" ] || continue
        for conf in "$dir"/vajra-*.conf; do
            [ -f "$conf" ] || continue
            if ! grep -q '^pm\.min_spare_servers' "$conf"; then
                # Insert after pm.start_servers
                sed -i '/^pm\.start_servers/a pm.min_spare_servers = 2\npm.max_spare_servers = 6' "$conf" 2>/dev/null || true
                tuned_any=1
            fi
        done
    done

    if [ "$tuned_any" -eq 1 ]; then
        local phpsvc=$(systemctl list-units --type=service --state=running 2>/dev/null | grep -oE 'php[0-9.]*-fpm|php-fpm' | head -1)
        [ -n "$phpsvc" ] && systemctl restart "$phpsvc" 2>/dev/null || true
        echo "   ✓ PHP-FPM spare servers fixed (min=2, max=6)"
    else
        echo "   ℹ PHP-FPM spare servers already set"
    fi

    # ─── Reload vajra to pick up new sections ───
    systemctl reload vajra 2>/dev/null || systemctl restart vajra 2>/dev/null || true
    echo "   ✓ vajra reloaded"

    log "✅ Production sections ready"
}

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

[logging]
access_log = "/var/log/vajra/access.log"
format = "text"

[tls]
listen  = "0.0.0.0:443"
cert    = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
key     = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"

[compression]
brotli = true
gzip = true
level = 6
gzip_level = 6
min_size = 1024

[headers]
hsts = "max-age=31536000; includeSubDomains"
x_content_type_options = "nosniff"
x_frame_options = "SAMEORIGIN"
referrer_policy = "strict-origin-when-cross-origin"

[fastcgi_cache]
enabled = true
path = "/var/cache/vajra/fastcgi"
max_size_mb = 512
default_ttl_secs = 3600
exclude_paths = ["/wp-admin/*", "/wp-login.php", "/wp-json/*"]
bypass_cookie = ["woocommerce_items_in_cart", "wp_woocommerce_session_", "comment_author_"]

TOML

    # 3. FastCGI कैश डायरेक्टरी बनाएँ और परमिशन सेट करें (Debian/CentOS दोनों के लिए)
    mkdir -p /var/log/vajra
    mkdir -p /var/cache/vajra/fastcgi
    chown -R nobody:nobody /var/log/vajra 2>/dev/null || chown -R apache:apache /var/log/vajra 2>/dev/null || true
    chown -R nobody:nobody /var/cache/vajra/fastcgi 2>/dev/null || chown -R www-data:www-data /var/cache/vajra/fastcgi 2>/dev/null || true

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

    tune_system
    ensure_app_toml_sections

verify_and_fix_services
echo "✅ Vajra Stack Installed!"
  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" "${@:3}" || true
        retune_php_fpm_after_site

  fi
}

# ═══════════════════════════════════════════════════════════════════════
# Retune mode — skip full install, only re-apply tuning
# Usage: sudo bash install.sh --retune
# ═══════════════════════════════════════════════════════════════════════
if [ "$RETUNE_ONLY" = "1" ]; then
    if [ "$(id -u)" -ne 0 ]; then
        die "❌ retune: must run as root"
    fi

    log "═══════════════════════════════════════════════════════════"
    log "  VAJRA RETUNE MODE — tuning only, no installation"
    log "═══════════════════════════════════════════════════════════"

    if [ ! -f /etc/vajra/app.toml ]; then
        die "❌ /etc/vajra/app.toml missing — install Vajra first"
    fi

    detect_distro
    tune_system
    ensure_app_toml_sections
    retune_php_fpm_after_site

    log ""
    log "✅ Retune complete"
    log ""
    log "Verify:"
    log "  systemctl status vajra"
    log "  grep -E 'workers|max_memory_mb|default_ttl_secs' /etc/vajra/app.toml"
    log "  sysctl net.core.somaxconn net.ipv4.tcp_congestion_control"
    log ""

    # Ensure vajra-retune wrapper exists (idempotent self-heal)
    install_retune_wrapper 2>/dev/null || true
    log "💡 Next time: just run 'sudo vajra-retune'"
    exit 0
fi

main "$@"

# Post-install: ensure /usr/local/bin/vajra-retune exists (idempotent)
install_retune_wrapper 2>/dev/null || true

# Updated installer with binary fix
