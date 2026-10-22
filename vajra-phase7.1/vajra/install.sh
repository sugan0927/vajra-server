#!/usr/bin/env bash
# ============================================================
# Vajra + WordPress universal installer (v0.15.11 Tarball)
# Supports: Debian/Ubuntu, RHEL family, Arch, openSUSE, Alpine
# ============================================================
set -euo pipefail

DOMAIN="${1:-}"
EMAIL="${2:-}"
REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-v0.15.11}"

[[ -z "$DOMAIN" || -z "$EMAIL" ]] && { echo "Usage: $0 <domain> <email>"; exit 1; }
[[ $EUID -ne 0 ]] && { echo "❌ Run as root"; exit 1; }

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠ %s\033[0m\n"  "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*"; exit 1; }

detect_distro() {
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    DISTRO_ID="${ID:-unknown}"
    DISTRO_LIKE="${ID_LIKE:-}"
    DISTRO_VER="${VERSION_ID:-}"
  else
    die "Cannot detect distro (/etc/os-release missing)"
  fi

  case "$DISTRO_ID $DISTRO_LIKE" in
    *debian*|*ubuntu*)               FAMILY=debian ;;
    *rhel*|*centos*|*rocky*|*alma*|*fedora*) FAMILY=rhel ;;
    *arch*)                          FAMILY=arch ;;
    *opensuse*|*suse*)               FAMILY=suse ;;
    *alpine*)                        FAMILY=alpine ;;
    *) die "Unsupported distro: $DISTRO_ID $DISTRO_LIKE" ;;
  esac
}

pkg_update() {
  case "$FAMILY" in
    debian)  apt-get update -y ;;
    rhel)    dnf makecache -y 2>/dev/null || yum makecache -y ;;
    arch)    pacman -Sy --noconfirm ;;
    suse)    zypper --non-interactive refresh ;;
    alpine)  apk update ;;
  esac
}

pkg_install() {
  case "$FAMILY" in
    debian)  DEBIAN_FRONTEND=noninteractive apt-get install -y "$@" ;;
    rhel)    dnf install -y "$@" 2>/dev/null || yum install -y "$@" ;;
    arch)    pacman -S --noconfirm --needed "$@" ;;
    suse)    zypper --non-interactive install -y "$@" ;;
    alpine)  apk add --no-cache "$@" ;;
  esac
}

has_systemd() { command -v systemctl >/dev/null 2>&1 && [[ -d /run/systemd/system ]]; }
svc_start()   { if has_systemd; then systemctl start "$1"; else rc-service "$1" start 2>/dev/null || true; fi; }
svc_enable()  { if has_systemd; then systemctl enable "$1"; else rc-update add "$1" default 2>/dev/null || true; fi; }
svc_restart() { if has_systemd; then systemctl restart "$1"; else rc-service "$1" restart 2>/dev/null || true; fi; }
svc_is_active() { if has_systemd; then systemctl is-active --quiet "$1"; else rc-service "$1" status >/dev/null 2>&1; fi; }

detect_php() {
  local candidates=()
  case "$FAMILY" in
    debian|rhel|suse) candidates=(8.5 8.4 8.3 8.2 8.1) ;;
    arch)             candidates=(8.4 8.3 8.2) ;;
    alpine)           candidates=(83 82 81) ;;
  esac
  for v in "${candidates[@]}"; do
    if command -v "php${v}" >/dev/null 2>&1 || command -v "php${v/./}" >/dev/null 2>&1; then
      PHP_VER="$v"; return
    fi
  done
  die "No supported PHP-FPM found"
}

php_socket_path() {
  case "$FAMILY" in
    debian)  echo "/run/php/php${PHP_VER}-fpm.sock" ;;
    rhel)    echo "/run/php-fpm/www.sock" ;;
    arch)    echo "/run/php-fpm/php-fpm.sock" ;;
    suse)    echo "/run/php-fpm.sock" ;;
    alpine)  echo "/run/php-fpm.sock" ;;
  esac
}

php_service_name() {
  case "$FAMILY" in
    debian)  echo "php${PHP_VER}-fpm" ;;
    rhel|arch|suse) echo "php-fpm" ;;
    alpine)  echo "php-fpm83" ;;
  esac
}

detect_distro
log "Detected: ${DISTRO_ID} ${DISTRO_VER} (family: ${FAMILY})"

log "1/10 — Base packages"
pkg_update
pkg_install curl wget ca-certificates openssl tar gzip unzip python3

log "2/10 — PHP-FPM + extensions"
case "$FAMILY" in
  debian) pkg_install php-fpm php-cli php-mysql php-curl php-xml php-mbstring php-zip php-gd php-intl php-bcmath ;;
  rhel)   pkg_install php-fpm php-cli php-mysqlnd php-curl php-xml php-mbstring php-zip php-gd php-intl php-bcmath ;;
  arch)   pkg_install php php-fpm php-gd php-intl ;;
  suse)   pkg_install php-fpm php-cli php-mysql php-curl php-xml php-mbstring php-zip php-gd php-intl php-bcmath ;;
  alpine) pkg_install php83 php83-fpm php83-mysqli php83-curl php83-xml php83-mbstring php83-zip php83-gd php83-intl php83-bcmath ;;
esac

detect_php
PHP_SOCK="$(php_socket_path)"
PHP_SVC="$(php_service_name)"
log "PHP ${PHP_VER}  socket=${PHP_SOCK}  service=${PHP_SVC}"

log "3/10 — MariaDB / MySQL"
case "$FAMILY" in
  debian) pkg_install mariadb-server ;;
  rhel)   pkg_install mariadb-server ;;
  arch)   pkg_install mariadb ;;
  suse)   pkg_install mariadb ;;
  alpine) pkg_install mariadb mariadb-client ;;
esac
for db_svc in mariadb mysql mysqld; do
  svc_enable "$db_svc" 2>/dev/null || true
  svc_start  "$db_svc" 2>/dev/null && break
done

log "4/10 — Vajra binary (from tarball)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)  BIN_ARCH=x86_64 ;;
  aarch64) BIN_ARCH=aarch64 ;;
  *) die "Unsupported arch: $ARCH" ;;
esac

if [[ "$VAJRA_VERSION" == "latest" ]]; then
  VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
fi
VER_NOV="${VAJRA_VERSION#v}"
BASE="https://github.com/${REPO}/releases/download/${VAJRA_VERSION}"
TMP="$(mktemp -d)"

CANDIDATES=(
  "vajra-${VER_NOV}-${BIN_ARCH}-unknown-linux-musl.tar.gz"
  "vajra-${VER_NOV}-${BIN_ARCH}-linux.tar.gz"
  "vajra-${VAJRA_VERSION}-${BIN_ARCH}-unknown-linux-musl.tar.gz"
  "vajra-${VAJRA_VERSION}-${BIN_ARCH}-linux.tar.gz"
)

api_url="https://api.github.com/repos/${REPO}/releases/tags/${VAJRA_VERSION}"
while IFS= read -r a; do
  [[ -z "$a" ]] && continue
  [[ "$a" == *.sha256 ]] && continue
  [[ "$a" == *.sha512 ]] && continue
  CANDIDATES+=("$a")
done < <(curl -fsSL "$api_url" 2>/dev/null | grep -oE '"browser_download_url"\s*:\s*"[^"]+\.tar\.gz"' | sed -E 's|.*/||; s|"$||' | grep -E 'x86_64|aarch64' || true)

ok=0
for asset in "${CANDIDATES[@]}"; do
  echo "   → trying: $asset"
  if curl -fL --retry 2 -sS "$BASE/$asset" -o "$TMP/$asset" 2>/dev/null; then
    if curl -fL -sS "$BASE/${asset}.sha256" -o "$TMP/${asset}.sha256" 2>/dev/null; then
      if ( cd "$TMP" && sha256sum -c "${asset}.sha256" >/dev/null 2>&1 ); then
        echo "   → checksum OK"
      else
        echo "   → checksum FAILED, skipping"
        continue
      fi
    fi
    if tar -xzf "$TMP/$asset" -C "$TMP" 2>/dev/null; then
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
  die "Could not download Vajra binary from ${BASE}."
fi
vajra --version 2>/dev/null | head -1 || true

log "5/10 — Directories + fallback TLS cert"
mkdir -p /etc/vajra/certs /var/log/vajra "/var/www/sites/${DOMAIN}"
chown -R www-data:www-data /var/log/vajra "/var/www/sites/${DOMAIN}" 2>/dev/null || true
if [[ ! -f /etc/vajra/certs/fallback.crt ]]; then
  openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
    -keyout /etc/vajra/certs/fallback.key -out /etc/vajra/certs/fallback.crt \
    -subj "/C=IN/O=Vajra/CN=vajra-fallback" -addext "subjectAltName=DNS:*.${DOMAIN#*.}"
fi
chmod 644 /etc/vajra/certs/fallback.crt
chmod 600 /etc/vajra/certs/fallback.key

log "6/10 — PHP-FPM pool for ${DOMAIN}"
POOL_DIR=""
for d in /etc/php/"${PHP_VER}"/fpm/pool.d /etc/php/fpm/pool.d /etc/php83/php-fpm.d; do
  [[ -d "$d" ]] && { POOL_DIR="$d"; break; }
done
[[ -z "$POOL_DIR" ]] && die "Cannot locate PHP-FPM pool.d"
SOCK_NAME="vajra-${DOMAIN//./_}.sock"
SOCK_PATH="/run/php/${SOCK_NAME}"
cat > "${POOL_DIR}/vajra-${DOMAIN//./_}.conf" <<FPM
[vajra-${DOMAIN//./_}]
user = www-data
group = www-data
listen = ${SOCK_PATH}
listen.owner = www-data
listen.group = www-data
listen.mode = 0660
pm = static
pm.max_children = 4
pm.max_requests = 500
php_admin_value[upload_max_filesize] = 64M
php_admin_value[post_max_size] = 64M
php_admin_value[memory_limit] = 256M
php_admin_value[max_execution_time] = 300
catch_workers_output = yes
decorate_workers_output = no
FPM
if ! id www-data >/dev/null 2>&1; then useradd -r -s /sbin/nologin www-data 2>/dev/null || true; fi
svc_restart "$PHP_SVC" 2>/dev/null || true
sleep 2
[[ -S "$SOCK_PATH" ]] || warn "PHP-FPM socket not found yet: $SOCK_PATH"

log "7/10 — Vajra config (native ACME, no per-site certs)"
cat > /etc/vajra/app.toml <<TOML
[server]
listen = "0.0.0.0:443"
workers = 1

[tls]
listen = "0.0.0.0:443"
cert = "/etc/vajra/certs/fallback.crt"
key  = "/etc/vajra/certs/fallback.key"

[acme]
enabled = true
email = "${EMAIL}"
staging = false

[logging]
access_log = "/var/log/vajra/access.log"

[[site]]
host = "${DOMAIN}"
root = "/var/www/sites/${DOMAIN}"
index = "index.php"
mode = "php"
php_sock = "unix:${SOCK_PATH}"
TOML

log "8/10 — Service unit"
if has_systemd; then
  cat > /etc/systemd/system/vajra.service <<UNIT
[Unit]
Description=Vajra web server (native ACME + PHP-FPM)
After=network.target ${PHP_SVC}.service mariadb.service mysql.service
Wants=network.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
Restart=on-failure
RestartSec=5s
TimeoutStopSec=60s
KillMode=mixed
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable --now vajra
else
  cat > /etc/init.d/vajra <<'RC'
#!/sbin/openrc-run
name="vajra"
command="/usr/local/bin/vajra"
command_args="--config /etc/vajra/app.toml"
command_background=true
pidfile="/run/vajra.pid"
depend() { need net; after php-fpm83 mariadb; }
RC
  chmod +x /etc/init.d/vajra
  rc-update add vajra default
  rc-service vajra start
fi
sleep 2
svc_is_active vajra || die "Vajra failed to start — check: journalctl -u vajra -n 30"

log "9/10 — WordPress"
SITE_ROOT="/var/www/sites/${DOMAIN}"
if [[ ! -f "${SITE_ROOT}/wp-load.php" ]]; then
  curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C /tmp
  cp -a /tmp/wordpress/. "${SITE_ROOT}/"
fi
DB_NAME="wordpress"; DB_USER="wpuser"; DB_PASS="$(openssl rand -base64 18 | tr -d '/+=')"
mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4;"
mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
mysql -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"

if [[ ! -f "${SITE_ROOT}/wp-config.php" ]]; then
  DOMAIN="$DOMAIN" DB_NAME="$DB_NAME" DB_USER="$DB_USER" DB_PASS="$DB_PASS" \
  python3 - <<'PY'
import os
from pathlib import Path
root = Path(f"/var/www/sites/{os.environ['DOMAIN']}")
src  = (root / "wp-config-sample.php").read_text()
src  = (src.replace("database_name_here", os.environ["DB_NAME"])
           .replace("username_here",      os.environ["DB_USER"])
           .replace("password_here",      os.environ["DB_PASS"]))
tweak = f"""
define('WP_HOME',    'https://{os.environ['DOMAIN']}');
define('WP_SITEURL', 'https://{os.environ['DOMAIN']}');
if (!empty($_SERVER['HTTP_X_FORWARDED_PROTO']) && strpos($_SERVER['HTTP_X_FORWARDED_PROTO'], 'https') !== false) {{
    $_SERVER['HTTPS'] = 'on'; $_SERVER['SERVER_PORT'] = 443;
}}
"""
src = src.replace("/* That's all, stop editing!", tweak + "\n/* That's all, stop editing!")
(root / "wp-config.php").write_text(src)
PY
fi
chown -R www-data:www-data "$SITE_ROOT" 2>/dev/null || true
chmod 640 "${SITE_ROOT}/wp-config.php" 2>/dev/null || true

log "10/10 — Final checks"
svc_restart "$PHP_SVC" 2>/dev/null || true
svc_restart vajra    2>/dev/null || true
sleep 2
echo -e "\n\033[1;32m✅  Vajra ${VAJRA_VERSION} ready\033[0m"
echo "    https://${DOMAIN}/wp-admin/install.php"
echo "    DB Name : ${DB_NAME}"
echo "    DB User : ${DB_USER}"
echo "    DB Pass : ${DB_PASS}"

log "7/10 — Vajra config (native ACME + compression)"
cat > /etc/vajra/app.toml <<TOML
[server]
listen = "0.0.0.0:443"
workers = 1

[tls]
listen = "0.0.0.0:443"
cert = "/etc/vajra/certs/fallback.crt"
key  = "/etc/vajra/certs/fallback.key"

[acme]
enabled = true
email = "${EMAIL}"
staging = false

[compression]
enabled = true
gzip = true
gzip_level = 6
brotli = true
brotli_level = 5

[logging]
access_log = "/var/log/vajra/access.log"

[[site]]
host = "${DOMAIN}"
root = "/var/www/sites/${DOMAIN}"
index = "index.php"
mode = "php"
php_sock = "unix:${SOCK_PATH}"
TOML

log "8/10 — Service unit"
if has_systemd; then
  cat > /etc/systemd/system/vajra.service <<UNIT
[Unit]
Description=Vajra web server (native ACME + PHP-FPM)
After=network.target ${PHP_SVC}.service mariadb.service mysql.service
Wants=network.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
Restart=on-failure
RestartSec=5s
TimeoutStopSec=60s
KillMode=mixed
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable --now vajra
else
  cat > /etc/init.d/vajra <<'RC'
#!/sbin/openrc-run
name="vajra"
command="/usr/local/bin/vajra"
command_args="--config /etc/vajra/app.toml"
command_background=true
pidfile="/run/vajra.pid"
depend() { need net; after php-fpm83 mariadb; }
RC
  chmod +x /etc/init.d/vajra
  rc-update add vajra default
  rc-service vajra start
fi
sleep 2
svc_is_active vajra || die "Vajra failed to start — check: journalctl -u vajra -n 30"

log "9/10 — WordPress"
SITE_ROOT="/var/www/sites/${DOMAIN}"
if [[ ! -f "${SITE_ROOT}/wp-load.php" ]]; then
  curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C /tmp
  cp -a /tmp/wordpress/. "${SITE_ROOT}/"
fi
DB_NAME="wordpress"; DB_USER="wpuser"; DB_PASS="$(openssl rand -base64 18 | tr -d '/+=')"
mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4;"
mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
mysql -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"

if [[ ! -f "${SITE_ROOT}/wp-config.php" ]]; then
  DOMAIN="$DOMAIN" DB_NAME="$DB_NAME" DB_USER="$DB_USER" DB_PASS="$DB_PASS" \
  python3 - <<'PY'
import os
from pathlib import Path
root = Path(f"/var/www/sites/{os.environ['DOMAIN']}")
src  = (root / "wp-config-sample.php").read_text()
src  = (src.replace("database_name_here", os.environ["DB_NAME"])
           .replace("username_here",      os.environ["DB_USER"])
           .replace("password_here",      os.environ["DB_PASS"]))
tweak = f"""
define('WP_HOME',    'https://{os.environ['DOMAIN']}');
define('WP_SITEURL', 'https://{os.environ['DOMAIN']}');
if (!empty($_SERVER['HTTP_X_FORWARDED_PROTO']) && strpos($_SERVER['HTTP_X_FORWARDED_PROTO'], 'https') !== false) {{
    $_SERVER['HTTPS'] = 'on'; $_SERVER['SERVER_PORT'] = 443;
}}
"""
src = src.replace("/* That's all, stop editing!", tweak + "\n/* That's all, stop editing!")
(root / "wp-config.php").write_text(src)
PY
fi
chown -R www-data:www-data "$SITE_ROOT" 2>/dev/null || true
chmod 640 "${SITE_ROOT}/wp-config.php" 2>/dev/null || true

log "10/10 — Final checks"
svc_restart "$PHP_SVC" 2>/dev/null || true
svc_restart vajra    2>/dev/null || true
sleep 2
echo -e "\n\033[1;32m✅  Vajra ${VAJRA_VERSION} ready with Brotli & Gzip\033[0m"
echo "    https://${DOMAIN}/wp-admin/install.php"
echo "    DB Name : ${DB_NAME}"
echo "    DB User : ${DB_USER}"
echo "    DB Pass : ${DB_PASS}"
