#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer (v14 — 1-Core VPS Optimized & Bug-Fixed)
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

RETUNE_ONLY=0
for _arg in "$@"; do
    case "$_arg" in --retune|-r) RETUNE_ONLY=1 ;; esac
done
unset _arg

FIRST_DOMAIN="${1:-}"; FIRST_EMAIL="${2:-}"; SITE_MODE="${SITE_MODE:-wordpress}"
if [[ "${3:-}" == "--php"  ]]; then SITE_MODE="php";  fi
if [[ "${3:-}" == "--html" ]]; then SITE_MODE="html"; fi

VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }

DISTRO_ID=""; DISTRO_LIKE=""; DISTRO_VER=""; DISTRO_NAME=""; DISTRO_FAMILY=""
detect_distro() {
  [[ -f /etc/os-release ]] || die "/etc/os-release missing"
  . /etc/os-release
  DISTRO_ID="${ID:-unknown}"; DISTRO_LIKE="${ID_LIKE:-}"; DISTRO_VER="${VERSION_ID:-}"
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
if id -u www-data >/dev/null 2>&1; then WWW_USER="www-data"
elif id -u apache >/dev/null 2>&1; then WWW_USER="apache"
elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"
elif id -u http >/dev/null 2>&1; then WWW_USER="http"
else WWW_USER="nobody"; fi

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
                  "php${PHP_VER}-imagick" "php${PHP_VER}-fileinfo" "php${PHP_VER}-exif" \
                  "php${PHP_VER}-redis" "php${PHP_VER}-igbinary"
      PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"; PHP_FPM_SVC="php${PHP_VER}-fpm"
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
        pkg_install php-intl php-bcmath php-imagick php-fileinfo php-exif php-pecl-redis || true
      fi
      PHP_VER="$(php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || echo "8.3")"
      PHP_SOCK="/run/php-fpm/www.sock"; PHP_FPM_SVC="php-fpm"
      ;;
    arch)
      pkg_install curl ca-certificates php php-fpm php-gd php-imagick php-intl php-redis php-igbinary
      PHP_VER="$(pacman -Si php 2>/dev/null | awk '/^Version/{print $3}' | cut -d. -f1,2)"
      [[ -n "$PHP_VER" ]] || PHP_VER="8"
      PHP_SOCK="/run/php-fpm/php-fpm.sock"; PHP_FPM_SVC="php-fpm"
      ;;
  esac
  echo "   → PHP $PHP_VER (socket: $PHP_SOCK)"
}

install_vajra_binary() {
  if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then return; fi
  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${VAJRA_REPO}/releases/latest" | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
  fi
  echo "   → tag: $VAJRA_VERSION"
  local BASE="https://github.com/${VAJRA_REPO}/releases/download/${VAJRA_VERSION}"
  local VER_NOV="${VAJRA_VERSION#v}"; local TMP; TMP="$(mktemp -d)"
  local -a CANDIDATES=(
    "vajra-${VAJRA_VERSION}-linux-x86_64.tar.gz" "vajra-${VER_NOV}-linux-x86_64.tar.gz"
    "vajra-${VAJRA_VERSION}-x86_64-linux.tar.gz" "vajra-${VER_NOV}-x86_64-linux.tar.gz"
    "vajra-${VAJRA_VERSION}-x86_64-unknown-linux-musl.tar.gz" "vajra-${VER_NOV}-x86_64-unknown-linux-musl.tar.gz"
  )
  local api_url="https://api.github.com/repos/${VAJRA_REPO}/releases/tags/${VAJRA_VERSION}"
  while IFS= read -r a; do
    [[ -z "$a" ]] && continue; [[ "$a" == *.sha256 ]] && continue
    CANDIDATES+=("$a")
  done < <(curl -fsSL "$api_url" 2>/dev/null | grep -oE '"browser_download_url"\s*:\s*"[^"]+\.tar\.gz"' | sed -E 's|.*/||; s|"$||' | grep -E 'x86_64|linux' || true)
  local asset ok=0
  for asset in "${CANDIDATES[@]}"; do
    echo "   → trying: $asset"
    if curl -fL --retry 2 --connect-timeout 10 --max-time 180 -sS "$BASE/$asset" -o "$TMP/$asset" 2>/dev/null; then
      if curl -fL -sS "$BASE/${asset}.sha256" -o "$TMP/${asset}.sha256" 2>/dev/null; then
        if ( cd "$TMP" && sha256sum -c "${asset}.sha256" >/dev/null 2>&1 ); then echo "   → checksum OK"; else echo "   → checksum FAILED"; continue; fi
      fi
      if tar -xzf "$TMP/$asset" -C "$TMP" 2>/dev/null; then
        local SRC=$(find "$TMP" -type f -name vajra -not -name '*.tar.gz' 2>/dev/null | head -1)
        if [[ -n "$SRC" ]]; then install -m 0755 "$SRC" /usr/local/bin/vajra; echo "   → installed vajra"; ok=1; break; fi
      fi
    fi
  done
  rm -rf "$TMP"
  if [[ "$ok" != "1" ]]; then die "Could not download Vajra binary."; fi
}

install_wp_cli() {
  if ! command -v wp >/dev/null 2>&1; then
    curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o /usr/local/bin/wp
    chmod 0755 /usr/local/bin/wp
  fi
}

create_vajra_site_cli() {
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
if id -u www-data >/dev/null 2>&1; then WWW_USER="www-data"; elif id -u apache >/dev/null 2>&1; then WWW_USER="apache"; elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"; else WWW_USER="nobody"; fi
PHP_SOCK="__PHP_SOCK__"
APP_TOML="/etc/vajra/app.toml"
WWW_ROOT="/var/www/sites"
log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }
[[ $EUID -ne 0 ]] && die "Run as root"

cmd_add() {
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local ADMIN_USER="wpadmin"; local ADMIN_PASS=""; local SITE_MODE="wordpress"
  while [[ $# -gt 0 ]]; do
    case "$1" in --admin-user) ADMIN_USER="$2"; shift 2 ;; --admin-pass) ADMIN_PASS="$2"; shift 2 ;; --php) SITE_MODE="php"; shift ;; --html) SITE_MODE="html"; shift ;; *) shift ;; esac
  done
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"
  mkdir -p "$SITE_ROOT"
  if [[ -f "$SITE_ROOT/wp-load.php" ]]; then echo "✅ Site already installed."; exit 0; fi
  
  log "Installing WordPress..."
  local TMP; TMP="$(mktemp -d)"; curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"; cp -a "$TMP/wordpress/." "$SITE_ROOT/"; rm -rf "$TMP"
  
  log "Getting SSL..."
  systemctl stop vajra.service 2>/dev/null || true
  if [[ ! -d "/etc/letsencrypt/live/${DOMAIN}" ]]; then
      if ! certbot certonly --standalone -d "$DOMAIN" -d "www.$DOMAIN" --agree-tos --email "$EMAIL" --non-interactive; then systemctl restart vajra.service || true; die "SSL failed."; fi
  fi
  systemctl restart vajra.service || true

  log "Updating Vajra config..."
  if [[ ! -f "$APP_TOML" ]]; then
    cat > "$APP_TOML" <<TOML_EOF
[server]
listen = "0.0.0.0:80"
workers = $(nproc)
grace_secs = 5
[logging]
access_log = "/var/log/vajra/access.log"
format = "text"
[tls]
listen = "0.0.0.0:443"
cert = "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
key  = "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"
[quic]
listen = "0.0.0.0:443"
[object_cache]
enabled = true
listen = "127.0.0.1:6379"
max_memory_mb = 256
[headers]
hsts = "max-age=31536000; includeSubDomains"
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
  fi
  systemctl restart vajra.service
  
  cd "$SITE_ROOT"
  local DB_NAME="wp_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-30)"
  local DB_USER="u_$(echo "$DOMAIN" | tr '.-' '__' | cut -c1-20)"
  local DB_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
  [[ -z "$ADMIN_PASS" ]] && ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
  
  "$(command -v mariadb || echo mysql)" -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`;"
  "$(command -v mariadb || echo mysql)" -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  "$(command -v mariadb || echo mysql)" -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';"
  "$(command -v mariadb || echo mysql)" -e "FLUSH PRIVILEGES;"

  sudo -u "$WWW_USER" wp plugin install redis-cache --activate --path="$SITE_ROOT" --allow-root 2>/dev/null || true
  sudo -u "$WWW_USER" wp redis enable --path="$SITE_ROOT" --allow-root 2>/dev/null || true

  if [ -d "$SITE_ROOT/wp-content" ]; then
      mkdir -p "$SITE_ROOT/wp-content/mu-plugins"
      cat > "$SITE_ROOT/wp-content/mu-plugins/vajra-cache.php" << 'MU_EOF'
<?php
add_filter('wp_headers', function ($h) {
    if (is_user_logged_in() || is_admin()) return $h;
    foreach (['woocommerce_items_in_cart','comment_author_','wp-postpass_'] as $c)
        if (isset($_COOKIE[$c])) return $h;
    $h['Cache-Control'] = 'public, max-age=300, s-maxage=300';
    unset($h['Pragma'], $h['Expires']);
    return $h;
}, 999);
MU_EOF
  fi

  wp --allow-root config create --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" --dbhost=127.0.0.1 --skip-check --force
  wp --allow-root core install --url="https://${DOMAIN}" --title="WordPress" --admin_user="$ADMIN_USER" --admin_password="$ADMIN_PASS" --admin_email="$EMAIL" --skip-email
  wp --allow-root plugin delete akismet hello 2>/dev/null || true
  
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT"
  find "$SITE_ROOT" -type d -exec chmod 755 {} \; && find "$SITE_ROOT" -type f -exec chmod 644 {} \;
  
  local CRED_FILE="/root/vajra-site-${DOMAIN}.txt"
  echo -e "Site: https://${DOMAIN}\nAdmin User: ${ADMIN_USER}\nAdmin Pass: ${ADMIN_PASS}\nDB: ${DB_NAME} / ${DB_USER} / ${DB_PASS}" > "$CRED_FILE"
  chmod 600 "$CRED_FILE"
  echo "✅ Done. Credentials: $CRED_FILE"
}
case "${1:-}" in add) shift; cmd_add "$@";; *) echo "Usage: vajra-site add <domain> <email>";; esac
VAJRA_SITE_SCRIPT
sed -i "s|__PHP_SOCK__|${PHP_SOCK}|g" /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
}

tune_system() {
    log "System tuning (RAM/nproc-aware)..."
    local NPROC=$(nproc)
    local RAM_MB=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
    local MAX_CHILDREN
    if   [ "$RAM_MB" -ge 4096 ]; then MAX_CHILDREN=32
    elif [ "$RAM_MB" -ge 2048 ]; then MAX_CHILDREN=16
    elif [ "$RAM_MB" -ge 1024 ]; then MAX_CHILDREN=8
    else                              MAX_CHILDREN=4; fi
    echo "   → nproc=$NPROC RAM=${RAM_MB}MB → PHP max_children=$MAX_CHILDREN"

    local REDIS_MB=$((RAM_MB / 4)); [ "$REDIS_MB" -lt 256 ] && REDIS_MB=256; [ "$REDIS_MB" -gt 2048 ] && REDIS_MB=2048
    local CACHE_MB=$((RAM_MB / 8)); [ "$CACHE_MB" -lt 64 ] && CACHE_MB=64; [ "$CACHE_MB" -gt 256 ] && CACHE_MB=256

    SYSCTL_DIR=/etc/sysctl.d; [ -d "$SYSCTL_DIR" ] || SYSCTL_DIR=/etc/sysctl.conf
    cat > "$SYSCTL_DIR/99-vajra-tuning.conf" <<'SYSCTL_EOF'
net.core.somaxconn = 65535
net.core.netdev_max_backlog = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15
net.ipv4.tcp_keepalive_time = 300
net.ipv4.tcp_keepalive_intvl = 30
net.ipv4.tcp_keepalive_probes = 5
fs.file-max = 1000000
fs.nr_open = 1000000
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
SYSCTL_EOF
    sysctl --system 2>/dev/null || true
    if ! sysctl net.ipv4.tcp_congestion_control 2>/dev/null | grep -q bbr; then
        sed -i '/tcp_congestion_control/d; /default_qdisc/d' "$SYSCTL_DIR/99-vajra-tuning.conf" 2>/dev/null || true
    fi

    mkdir -p /dev/shm/vajra-fastcgi
    chmod 755 /dev/shm/vajra-fastcgi
    echo "d /dev/shm/vajra-fastcgi 0755 root root -" > /etc/tmpfiles.d/vajra-cache.conf
    systemd-tmpfiles --create /etc/tmpfiles.d/vajra-cache.conf 2>/dev/null || true
    echo "   ✓ /dev/shm/vajra-fastcgi created and persisted (RAM Cache)"

    if [ -f /etc/vajra/app.toml ]; then
        sed -i "s/^workers\s*=.*/workers = $NPROC/" /etc/vajra/app.toml
        sed -i "s/^max_memory_mb\s*=.*/max_memory_mb = $REDIS_MB/" /etc/vajra/app.toml
        sed -i "s/^max_size_mb\s*=.*/max_size_mb = $CACHE_MB/" /etc/vajra/app.toml
        sed -i "s/^default_ttl_secs\s*=.*/default_ttl_secs = 21600/" /etc/vajra/app.toml
        sed -i "s/^gzip_level\s*=.*/gzip_level = 1/" /etc/vajra/app.toml
        sed -i "s/^brotli\s*=.*/brotli = false/" /etc/vajra/app.toml
        sed -i "s/^brotli_level\s*=.*/brotli_level = 1/" /etc/vajra/app.toml
        sed -i 's|path = "/var/cache/vajra/fastcgi"|path = "/dev/shm/vajra-fastcgi"|g' /etc/vajra/app.toml
        echo "   ✓ app.toml: workers=$NPROC, gzip=L1, brotli=off, RAM cache"
    fi

    mkdir -p /etc/systemd/system/vajra.service.d
    cat > /etc/systemd/system/vajra.service.d/limits.conf <<'SVC_EOF'
[Service]
LimitNOFILE=1000000
LimitNPROC=65535
LimitCORE=0
SVC_EOF
    systemctl daemon-reload

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
            if grep -q 'vajra' "$conf" 2>/dev/null; then
                sed -i "s/^pm\.max_children\s*=.*/pm.max_children = $MAX_CHILDREN/" "$conf"
                sed -i "s/^pm\.max_requests\s*=.*/pm.max_requests = 1000/" "$conf"
                tuned_any=1
            fi
        done
    done
    if [ "$tuned_any" -eq 1 ]; then
        local phpsvc=$(systemctl list-units --type=service --state=running 2>/dev/null | grep -oE 'php[0-9.]*-fpm|php-fpm' | head -1)
        [ -n "$phpsvc" ] && systemctl restart "$phpsvc" 2>/dev/null || true
    fi
}

verify_and_fix_services() {
    log "🔍 Verifying core services..."
    local db_svc="mariadb"
    if ! systemctl is-active --quiet "$db_svc"; then
        mkdir -p /run/mariadb /var/run/mysqld
        chown -R mysql:mysql /var/lib/mysql /run/mariadb /var/run/mysqld 2>/dev/null || true
        systemctl restart "$db_svc" 2>/dev/null || systemctl restart mysql 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet "$db_svc"; then die "❌ MariaDB could not be started."; fi
    log "✅ MariaDB is running."

    local php_svc="php-fpm"
    if ! systemctl list-unit-files "php-fpm.service" --no-legend 2>/dev/null | grep -q "php-fpm.service"; then
        local found_php=$(ls /usr/lib/systemd/system/php*-fpm.service /lib/systemd/system/php*-fpm.service 2>/dev/null | head -1)
        if [ -n "$found_php" ]; then php_svc=$(basename "$found_php" .service); fi
    fi
    if ! systemctl is-active --quiet "$php_svc"; then
        mkdir -p /run/php-fpm /run/php
        chown -R "$WWW_USER:$WWW_USER" /run/php-fpm /run/php 2>/dev/null || true
        systemctl restart "$php_svc" 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet "$php_svc"; then die "❌ PHP-FPM could not be started."; fi
    log "✅ PHP-FPM is running."

    if ! systemctl is-active --quiet vajra; then
        systemctl reset-failed vajra 2>/dev/null || true
        systemctl restart vajra 2>/dev/null || true
    fi
    if ! systemctl is-active --quiet vajra; then die "❌ Vajra could not be started."; fi
    log "✅ Vajra Web Server is running."
}


setup_zram() {
    log "Setting up ZRAM (Memory Compression)..."
    local RAM_MB=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
    
    # ZRAM साइज़: 50% RAM, लेकिन कम से कम 512MB
    local ZRAM_MB=$((RAM_MB / 2))
    [ "$ZRAM_MB" -lt 512 ] && ZRAM_MB=512

    case "$DISTRO_FAMILY" in
        debian)
            pkg_install zram-tools || true
            if [ -d /etc/default ]; then
                cat > /etc/default/zramswap << ZRAMEOF
ALGO=zstd
PERCENT=50
PRIORITY=100
ZRAMEOF
                systemctl restart zramswap 2>/dev/null || true
                systemctl enable zramswap 2>/dev/null || true
            fi
            ;;
        rhel|arch)
            pkg_install zram-generator || true
            if [ -d /etc/systemd ]; then
                cat > /etc/systemd/zram-generator.conf << ZRAMEOF
[zram0]
zram-size = ${ZRAM_MB}
compression-algorithm = zstd
ZRAMEOF
                systemctl daemon-reload
                systemctl start systemd-zram-setup@zram0.service 2>/dev/null || true
                systemctl enable systemd-zram-setup@zram0.service 2>/dev/null || true
            fi
            ;;
        *)
            warn "ZRAM auto-setup not implemented for distro family: $DISTRO_FAMILY"
            ;;
    esac

    # वेरिफिकेशन
    if command -v zramctl >/dev/null 2>&1 && zramctl | grep -q "zram0"; then
        echo "   ✓ ZRAM configured successfully (Size: ~${ZRAM_MB}MB, Algo: zstd)"
    else
        warn "ZRAM setup might have failed or is unavailable."
    fi
}

main() {
  log "0/6 — Pre-flight"
  detect_distro
  WWW_USER="$(www_user)"
  if ! id -u "$WWW_USER" >/dev/null 2>&1; then
    if id -u apache >/dev/null 2>&1; then WWW_USER="apache"
    elif id -u nginx >/dev/null 2>&1; then WWW_USER="nginx"
    else useradd -r -s /sbin/nologin -d /var/www -c "Apache" apache 2>/dev/null || true; WWW_USER="apache"; fi
  fi

  log "1/6 — Base Packages"
  pkg_update
  pkg_install curl wget ca-certificates gnupg openssl python3 certbot || true
  
  if [[ "$DISTRO_FAMILY" == "rhel" ]]; then
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
  systemctl enable --now "$PHP_FPM_SVC" 2>/dev/null || true
  for i in {1..30}; do [[ -S "$PHP_SOCK" ]] && break; sleep 1; done
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
  mkdir -p /var/www/.wp-cli/cache
  chown -R "$WWW_USER:$WWW_USER" /var/www/.wp-cli /var/log/vajra /var/www/sites
  chmod 750 /var/www/.wp-cli

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
cert    = "/etc/vajra/certs/fallback.crt"
key     = "/etc/vajra/certs/fallback.key"
[compression]
enabled = true
gzip = true
gzip_level = 1
brotli = false
brotli_level = 1
min_size = 1024
[headers]
hsts = "max-age=31536000; includeSubDomains"
[fastcgi_cache]
enabled = true
path = "/dev/shm/vajra-fastcgi"
max_size_mb = 64
default_ttl_secs = 3600
TOML
    mkdir -p /dev/shm/vajra-fastcgi; chmod 755 /dev/shm/vajra-fastcgi
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

  tune_system
  setup_zram
  verify_and_fix_services

  echo "✅ Vajra Stack Installed!"
  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" "${@:3}" || true
  fi
}

if [ "$RETUNE_ONLY" = "1" ]; then
    if [ "$(id -u)" -ne 0 ]; then die "❌ retune: must run as root"; fi
    if [ ! -f /etc/vajra/app.toml ]; then die "❌ /etc/vajra/app.toml missing"; fi
    detect_distro
    tune_system
  setup_zram
    log "✅ Retune complete"
    exit 0
fi

main "$@"
