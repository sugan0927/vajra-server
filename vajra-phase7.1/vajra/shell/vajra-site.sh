#!/usr/bin/env bash
# vajra-site — manage Vajra sites (WordPress / PHP / static HTML)
# Rev 8 — added --php, --html modes + `update php` command.
set -euo pipefail

# ── Runtime distro detection (no baked-in values) ────────────────────
DISTRO_FAMILY="debian"
DISTRO_ID=""
if [[ -f /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  DISTRO_ID="${ID:-unknown}"
fi
case "$DISTRO_ID" in
  ubuntu|debian|linuxmint|pop|kali|raspbian) DISTRO_FAMILY="debian" ;;
  rhel|centos|rocky|almalinux|ol|fedora|amzn) DISTRO_FAMILY="rhel" ;;
  arch|manjaro|endeavouros|garuda)           DISTRO_FAMILY="arch" ;;
esac

# nginx layout
if [[ -d /etc/nginx/sites-available ]]; then
  NGINX_SITES_AVAIL=/etc/nginx/sites-available
  NGINX_SITES_ENABLED=/etc/nginx/sites-enabled
else
  NGINX_SITES_AVAIL=/etc/nginx/conf.d
  NGINX_SITES_ENABLED=/etc/nginx/conf.d
fi

# Web user (pick whichever exists)
if   id www-data >/dev/null 2>&1; then WWW_USER="www-data"
elif id nginx    >/dev/null 2>&1; then WWW_USER="nginx"
elif id apache   >/dev/null 2>&1; then WWW_USER="apache"
elif id http     >/dev/null 2>&1; then WWW_USER="http"
else                                    WWW_USER="www-data"
fi

# PHP-FPM layout
if [[ -d /etc/php ]]; then
  PHP_VER="$(ls -1 /etc/php/ 2>/dev/null | grep -E '^[0-9]+\.[0-9]+$' | sort -V | tail -1)"
  [[ -z "$PHP_VER" ]] && PHP_VER="8.5"
  PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
  PHP_FPM_SVC="php${PHP_VER}-fpm"
elif [[ -d /etc/php-fpm.d ]]; then
  PHP_VER="$(php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || true)"
  [[ -z "$PHP_VER" ]] && PHP_VER="8.4"
  PHP_SOCK="/run/php-fpm/www.sock"
  PHP_FPM_SVC="php-fpm"
else
  PHP_VER="8.4"
  PHP_SOCK="/run/php-fpm/www.sock"
  PHP_FPM_SVC="php-fpm"
fi

# ── Service helper adapters ──────────────────────────────────────────
svc_reload()        { systemctl reload  "$1" 2>/dev/null || systemctl restart "$1" 2>/dev/null || true; }
svc_restart()       { systemctl restart "$1" 2>/dev/null || true; }
svc_start()         { systemctl start   "$1" 2>/dev/null || true; }
svc_enable_now()    { systemctl enable --now "$1" 2>/dev/null || true; }
svc_daemon_reload() { systemctl daemon-reload 2>/dev/null || true; }

SITES_DIR=/etc/vajra/sites
WWW_ROOT=/var/www/sites
CERTBOT_ROOT=/var/www/certbot
FIRST_PORT=8080

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
info() { printf "\033[1;36mℹ  %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
err()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; }
die()  { err "$*"; exit 1; }

[[ $EUID -ne 0 ]] && die "Run as root"

# ─── helpers ─────────────────────────────────────────────────────────────
db_cli() {
  if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi
}
db_exec() { "$(db_cli)" "$@"; }

mariadb_ensure_running() {
  systemctl enable mariadb >/dev/null 2>&1 || true
  systemctl start  mariadb >/dev/null 2>&1 || true
  sleep 2
  systemctl is-active --quiet mariadb && return 0
  if command -v mariadb-install-db >/dev/null 2>&1 && [[ ! -d /var/lib/mysql/mysql ]]; then
    mariadb-install-db --user=mysql >/dev/null 2>&1 || true
  fi
  systemctl start mariadb >/dev/null 2>&1 || true
  sleep 3
  systemctl is-active --quiet mariadb
}

php_can_connect_mariadb() {
  local host="$1" user="$2" pass="$3" db="$4"
  php -r '
    $host = $argv[1]; $user = $argv[2]; $pass = $argv[3]; $db = $argv[4];
    $m = @new mysqli($host, $user, $pass, $db);
    if ($m->connect_errno) { fwrite(STDERR, $m->connect_error); exit(1); }
    $m->close();
  ' -- "$host" "$user" "$pass" "$db" 2>/dev/null
}

# Detect installed PHP versions (both Debian SURY and RHEL Remi layouts).
php_versions_installed() {
  local vers=()
  if [[ -d /etc/php ]]; then
    for d in /etc/php/*/; do
      local v; v="$(basename "$d")"
      [[ "$v" =~ ^[0-9]+\.[0-9]+$ ]] && [[ -x "/usr/bin/php${v}" ]] && vers+=("$v")
    done
  fi
  for bin in /usr/bin/php /usr/bin/php8.* /usr/bin/php7.*; do
    [[ -x "$bin" ]] || continue
    local v; v="$("$bin" -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || true)"
    [[ -n "$v" ]] && vers+=("$v")
  done
  printf '%s\n' "${vers[@]}" | sort -u -V
}

php_socket_for_version() {
  local ver="$1"
  case "$DISTRO_FAMILY" in
    debian) echo "/run/php/php${ver}-fpm.sock" ;;
    rhel|arch) echo "/run/php-fpm/www.sock" ;;
  esac
}

php_fpm_service_for_version() {
  local ver="$1"
  case "$DISTRO_FAMILY" in
    debian) echo "php${ver}-fpm" ;;
    rhel|arch) echo "php-fpm" ;;
  esac
}

usage() {
cat <<U
vajra-site — manage Vajra sites

USAGE:
  vajra-site add <domain> <email>                     # WordPress (default)
  vajra-site add <domain> <email> --php               # PHP-only site
  vajra-site add <domain> <email> --html              # Static HTML site
  vajra-site add <domain> <email> --wordpress [opts]

  WordPress options:
    --title "Site Title"
    --admin-user NAME
    --admin-pass PASS

  vajra-site list
  vajra-site info <domain>
  vajra-site update php <domain> [version]            # Switch PHP version
  vajra-site remove <domain> [--keep-files] [--keep-db]
  vajra-site help
U
}

valid_domain() { [[ "$1" =~ ^([a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$ ]]; }
valid_email()  { [[ "$1" =~ ^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$ ]]; }
sanitize()     { tr '[:upper:].-' '[:lower:]__' <<< "$1" | cut -c1-40; }
require_site() { [[ -f "${SITES_DIR}/${1}.env" ]] || die "Site '$1' not found"; }
read_env()     { sed -n "s/^${2}=//p" "$1" | head -1; }
next_port() {
  local used; used="$(grep -h '^PORT=' "${SITES_DIR}"/*.env 2>/dev/null | cut -d= -f2 | sort -n | tail -1 || true)"
  echo $(( ${used:-$((FIRST_PORT-1))} + 1 ))
}

ensure_stack() {
  local mode="${1:-wordpress}"
  local svcs=("nginx")
  [[ "$mode" == "php" || "$mode" == "wordpress" ]] && svcs+=("${PHP_FPM_SVC}")
  [[ "$mode" == "wordpress" ]] && svcs+=("mariadb")

  local svc
  for svc in "${svcs[@]}"; do
    [[ -z "$svc" ]] && continue
    if ! systemctl is-active --quiet "$svc"; then
      warn "$svc not running — attempting start"
      if [[ "$svc" == "mariadb" ]]; then
        mariadb_ensure_running || true
      else
        systemctl enable --now "$svc" >/dev/null 2>&1 || true
        sleep 2
      fi
      if ! systemctl is-active --quiet "$svc"; then
        err "$svc failed to start"
        journalctl -u "$svc" -n 20 --no-pager | sed 's/^/    /' || true
        die "Cannot proceed — fix $svc first"
      fi
      info "$svc started"
    fi
  done
  if [[ "$mode" != "html" ]] && [[ ! -S "$PHP_SOCK" ]]; then
    warn "PHP socket $PHP_SOCK missing — restarting PHP-FPM"
    systemctl restart "$PHP_FPM_SVC" >/dev/null 2>&1 || true
    sleep 2
    [[ -S "$PHP_SOCK" ]] || die "PHP-FPM socket still missing: $PHP_SOCK"
  fi
}

selinux_fix_site() {
  local site_root="$1"
  local mode="${2:-wordpress}"
  command -v getenforce >/dev/null 2>&1 || return 0
  [[ "$(getenforce 2>/dev/null)" == "Enforcing" ]] || return 0

  semanage fcontext -a -t httpd_sys_content_t "${site_root}(/.*)?" 2>/dev/null || true

  case "$mode" in
    wordpress)
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/wp-admin(/.*)?" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/wp-includes(/.*)?" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/wp-content(/.*)?" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/[^/]+\.php" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/license\.txt" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/readme\.html" 2>/dev/null || true
      ;;
    php)
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}(/.*)?" 2>/dev/null || true
      ;;
    html) ;;
  esac
  restorecon -Rv "$site_root" >/dev/null 2>&1 || true
}

nginx_write_site() {
  local domain="$1"; shift
  local content="$*"
  if [[ "$DISTRO_FAMILY" == "debian" ]]; then
    printf '%s\n' "$content" > "${NGINX_SITES_AVAIL}/${domain}.conf"
    ln -sf "${NGINX_SITES_AVAIL}/${domain}.conf" "${NGINX_SITES_ENABLED}/${domain}.conf"
  else
    printf '%s\n' "$content" > "${NGINX_SITES_ENABLED}/${domain}.conf"
  fi
}
nginx_remove_site() {
  local domain="$1"
  rm -f "${NGINX_SITES_ENABLED}/${domain}.conf"
  [[ "$DISTRO_FAMILY" == "debian" ]] && rm -f "${NGINX_SITES_AVAIL}/${domain}.conf"
}

setup_wp_cron() {
  local domain="$1"
  local site_root="$2"

  if ! grep -q "DISABLE_WP_CRON" "$site_root/wp-config.php" 2>/dev/null; then
    python3 - "$site_root/wp-config.php" <<'CRONPY'
import sys
from pathlib import Path
p = Path(sys.argv[1]); s = p.read_text()
anchor = "/* That's all, stop editing!"
inject = (
    "/** Disable WP internal cron — use system cron instead. */\n"
    "define('DISABLE_WP_CRON', true);\n\n"
)
if 'DISABLE_WP_CRON' not in s and anchor in s:
    s = s.replace(anchor, inject + anchor, 1); p.write_text(s)
    print("   → wp-config.php: DISABLE_WP_CRON added")
CRONPY
  fi

  local cron_file="/etc/cron.d/vajra-wp-cron-${domain//./_}"
  cat > "$cron_file" <<CRON
# WP-Cron for ${domain} (managed by vajra-site)
*/5 * * * * ${WWW_USER} /usr/bin/php -q ${site_root}/wp-cron.php >/dev/null 2>&1
CRON
  chmod 644 "$cron_file"
  echo "   → system cron added: $cron_file (every 5 min)"
}

fix_wp_filesystem() {
  local site_root="$1"

  chown -R "$WWW_USER:$WWW_USER" "$site_root"
  find "$site_root" -type d -exec chmod 755 {} \; 2>/dev/null || true
  find "$site_root" -type f -exec chmod 644 {} \; 2>/dev/null || true
  [[ -f "$site_root/wp-config.php" ]] && chmod 640 "$site_root/wp-config.php"
  [[ -d "$site_root/wp-content" ]] && chmod -R 775 "$site_root/wp-content"

  if ! grep -q "FS_METHOD" "$site_root/wp-config.php" 2>/dev/null; then
    python3 - "$site_root/wp-config.php" <<'FSPY'
import sys
from pathlib import Path
p = Path(sys.argv[1]); s = p.read_text()
anchor = "That's all, stop editing!"
inject = (
    "/** Force direct filesystem method (no FTP prompts on updates). */\n"
    "define('FS_METHOD', 'direct');\n\n"
)
if 'FS_METHOD' not in s and anchor in s:
    s = s.replace("/* " + anchor, inject + "/* " + anchor, 1); p.write_text(s)
    print("   → wp-config.php: FS_METHOD=direct added")
FSPY
  fi
}

# ─── vajra TOML generators ───────────────────────────────────────────────
write_toml_wordpress() {
  local domain="$1" port="$2" site_root="$3" php_sock="$4"
  cat > "${SITES_DIR}/${domain}.toml" <<TOML
[server]
listen  = "127.0.0.1:${port}"
workers = 1
pin     = false

[static]
root  = "${site_root}"
index = "index.html"

[php]
upstream         = "unix:${php_sock}"
index            = ["index.php"]
front_controller = "/index.php"
timeout_secs     = 60

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/${domain}.access.log"

[admin]
listen = "127.0.0.1:$((port + 1000))"
TOML
}

write_toml_php() {
  local domain="$1" port="$2" site_root="$3" php_sock="$4"
  cat > "${SITES_DIR}/${domain}.toml" <<TOML
[server]
listen  = "127.0.0.1:${port}"
workers = 1
pin     = false

[static]
root  = "${site_root}"
index = "index.php"

[php]
upstream         = "unix:${php_sock}"
index            = ["index.php"]
front_controller = "/index.php"
timeout_secs     = 60

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/${domain}.access.log"

[admin]
listen = "127.0.0.1:$((port + 1000))"
TOML
}

write_toml_html() {
  local domain="$1" port="$2" site_root="$3"
  cat > "${SITES_DIR}/${domain}.toml" <<TOML
[server]
listen  = "127.0.0.1:${port}"
workers = 1
pin     = false

[static]
root  = "${site_root}"
index = "index.html"

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/${domain}.access.log"

[admin]
listen = "127.0.0.1:$((port + 1000))"
TOML
}

# ─── default content ─────────────────────────────────────────────────────
write_php_welcome() {
  local domain="$1" site_root="$2"
  [[ -f "$site_root/index.php" ]] && return 0
  cat > "$site_root/index.php" <<PHP
<?php
// Welcome page for ${domain} — replace this with your app.
header('Content-Type: text/html; charset=utf-8');
?>
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${domain} — PHP site is live</title>
<style>
  body{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;
       max-width:720px;margin:60px auto;padding:0 24px;color:#222;line-height:1.55}
  h1{color:#0a7d36;margin-bottom:.2em}
  code{background:#f4f4f4;padding:2px 6px;border-radius:4px;font-size:.9em}
  .info{background:#eef7f0;border-left:4px solid #0a7d36;padding:14px 18px;border-radius:6px}
</style>
</head>
<body>
  <h1>✅ PHP is working on ${domain}</h1>
  <div class="info">
    <p><strong>PHP version:</strong> <code><?php echo PHP_VERSION; ?></code></p>
    <p><strong>Server:</strong> <code><?php echo \$_SERVER['SERVER_SOFTWARE'] ?? 'vajra'; ?></code></p>
    <p><strong>Document root:</strong> <code><?php echo __DIR__; ?></code></p>
  </div>
  <p>Edit <code>index.php</code> in <code>/var/www/sites/${domain}/</code> to build your app.</p>
  <p><?php echo date('Y-m-d H:i:s T'); ?></p>
</body>
</html>
PHP
  chown "${WWW_USER}:${WWW_USER}" "$site_root/index.php"
  chmod 644 "$site_root/index.php"
}

write_html_welcome() {
  local domain="$1" site_root="$2"
  [[ -f "$site_root/index.html" ]] && return 0
  cat > "$site_root/index.html" <<HTML
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${domain} — site is live</title>
<style>
  body{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;
       max-width:720px;margin:60px auto;padding:0 24px;color:#222;line-height:1.55}
  h1{color:#0a7d36;margin-bottom:.2em}
  code{background:#f4f4f4;padding:2px 6px;border-radius:4px;font-size:.9em}
</style>
</head>
<body>
  <h1>✅ ${domain} is live</h1>
  <p>This is a static HTML site served by <strong>Vajra</strong> + <strong>Nginx</strong>.</p>
  <p>Edit files in <code>/var/www/sites/${domain}/</code> and refresh to see changes.</p>
</body>
</html>
HTML
  chown "${WWW_USER}:${WWW_USER}" "$site_root/index.html"
  chmod 644 "$site_root/index.html"
}

# ─── cmd_add ─────────────────────────────────────────────────────────────
cmd_add() {
  [[ $# -ge 2 ]] || { err "Usage: vajra-site add <domain> <email> [--php|--html|--wordpress] [opts]"; exit 1; }
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local TITLE="WordPress" ADMIN_USER="wpadmin" ADMIN_PASS=""
  local MODE="wordpress"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --title)       TITLE="$2"; shift 2;;
      --admin-user)  ADMIN_USER="$2"; shift 2;;
      --admin-pass)  ADMIN_PASS="$2"; shift 2;;
      --php)         MODE="php"; shift;;
      --html)        MODE="html"; shift;;
      --wordpress|--wp) MODE="wordpress"; shift;;
      *) die "Unknown option: $1";;
    esac
  done

  valid_domain "$DOMAIN" || die "Invalid domain: $DOMAIN"
  valid_email  "$EMAIL"  || die "Invalid email: $EMAIL"
  [[ -f "${SITES_DIR}/${DOMAIN}.env" ]] && die "Site '$DOMAIN' already exists"

  ensure_stack "$MODE"

  local WWW_ALIAS="www.${DOMAIN}"
  local PORT; PORT="$(next_port)"
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"

  log "1/8 — Allocating resources"
  echo "   → domain: $DOMAIN (+ $WWW_ALIAS)"
  echo "   → mode:   $MODE"
  echo "   → port:   $PORT"

  log "2/8 — Site files"
  mkdir -p "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}/.well-known/acme-challenge"

  local DB_NAME="" DB_USER="" DB_PASS=""
  case "$MODE" in
    wordpress)
      if [[ ! -f "$SITE_ROOT/wp-load.php" ]]; then
        local TMP; TMP="$(mktemp -d)"
        curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
        cp -a "$TMP/wordpress/." "$SITE_ROOT/"
        rm -rf "$TMP"
      fi
      ;;
    php)  write_php_welcome  "$DOMAIN" "$SITE_ROOT" ;;
    html) write_html_welcome "$DOMAIN" "$SITE_ROOT" ;;
  esac
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}"

  if [[ "$MODE" == "wordpress" ]]; then
    DB_NAME="wp_$(sanitize "$DOMAIN")"
    DB_USER="u_$(sanitize "$DOMAIN")"; DB_USER="${DB_USER:0:20}"
    DB_PASS="$(openssl rand -base64 24 | tr -d '/+=\n' | cut -c1-24)"
    [[ -n "$ADMIN_PASS" ]] || ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"

    log "3/8 — MariaDB database + user"
    echo "   → db: $DB_NAME / $DB_USER"
    db_exec -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
    for _h in localhost 127.0.0.1 ::1; do
      db_exec -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'${_h}' IDENTIFIED BY '${DB_PASS}';" 2>/dev/null || true
      db_exec -e "ALTER USER '${DB_USER}'@'${_h}' IDENTIFIED BY '${DB_PASS}';" 2>/dev/null || true
      db_exec -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'${_h}';" 2>/dev/null || true
    done
    db_exec -e "FLUSH PRIVILEGES;"

    log "4/8 — wp-config.php + WordPress install (WP-CLI)"
    wp --path="$SITE_ROOT" --allow-root config create \
        --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" \
        --dbhost=127.0.0.1 --dbcharset=utf8mb4 --skip-check --force
    wp --path="$SITE_ROOT" --allow-root config set WP_HOME    "https://${DOMAIN}" --type=constant
    wp --path="$SITE_ROOT" --allow-root config set WP_SITEURL "https://${DOMAIN}" --type=constant
    wp --path="$SITE_ROOT" --allow-root config set VAJRA_PROXY_FIX "1" --raw --type=constant 2>/dev/null || true

    python3 - "$SITE_ROOT/wp-config.php" <<'WPCFGPY'
import sys
from pathlib import Path
p = Path(sys.argv[1]); s = p.read_text()
anchor = "/* That's all, stop editing!"
inject = (
    "/** Vajra reverse-proxy: force HTTPS (TLS terminated by Nginx). */\n"
    "$_SERVER['HTTPS']       = 'on';\n"
    "$_SERVER['SERVER_PORT'] = 443;\n"
    "if (isset($_SERVER['HTTP_X_FORWARDED_HOST'])) {\n"
    "    $_SERVER['HTTP_HOST'] = $_SERVER['HTTP_X_FORWARDED_HOST'];\n"
    "}\n\n"
)
if 'Vajra reverse-proxy: force HTTPS' not in s and anchor in s:
    s = s.replace(anchor, inject + anchor, 1); p.write_text(s)
    print("   → wp-config.php: HTTPS force-injected")
WPCFGPY

    if ! php_can_connect_mariadb "127.0.0.1" "$DB_USER" "$DB_PASS" "$DB_NAME"; then
      warn "PHP → MariaDB failed — attempting heal"
      systemctl restart mariadb 2>/dev/null || true
      sleep 3
      for _h in localhost 127.0.0.1 ::1; do
        db_exec -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'${_h}';" 2>/dev/null || true
      done
      db_exec -e "FLUSH PRIVILEGES;" 2>/dev/null || true
      php_can_connect_mariadb "127.0.0.1" "$DB_USER" "$DB_PASS" "$DB_NAME" || \
        die "DB connection still failing — check systemctl status mariadb"
    fi

    wp --path="$SITE_ROOT" --allow-root core install \
        --url="https://${DOMAIN}" --title="$TITLE" \
        --admin_user="$ADMIN_USER" --admin_password="$ADMIN_PASS" \
        --admin_email="$EMAIL" --skip-email
    wp --path="$SITE_ROOT" --allow-root option update blogdescription "" 2>/dev/null || true
    wp --path="$SITE_ROOT" --allow-root rewrite structure '/%postname%/' --hard 2>/dev/null || true

    fix_wp_filesystem "$SITE_ROOT"
    selinux_fix_site "$SITE_ROOT" "wordpress"
    setup_wp_cron   "$DOMAIN"    "$SITE_ROOT"
  else
    log "3/8 — (skipped: no database for ${MODE} mode)"
    log "4/8 — (skipped: no WP-CLI for ${MODE} mode)"
    selinux_fix_site "$SITE_ROOT" "$MODE"
  fi

  log "5/8 — Vajra config + service"
  case "$MODE" in
    wordpress) write_toml_wordpress "$DOMAIN" "$PORT" "$SITE_ROOT" "$PHP_SOCK" ;;
    php)       write_toml_php       "$DOMAIN" "$PORT" "$SITE_ROOT" "$PHP_SOCK" ;;
    html)      write_toml_html      "$DOMAIN" "$PORT" "$SITE_ROOT" ;;
  esac

  cat > "${SITES_DIR}/${DOMAIN}.env" <<ENV
MODE=${MODE}
DOMAIN=${DOMAIN}
WWW_ALIAS=${WWW_ALIAS}
EMAIL=${EMAIL}
PORT=${PORT}
PHP_VER=${PHP_VER}
PHP_SOCK=${PHP_SOCK}
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
    || { journalctl -u "vajra-site@${DOMAIN}.service" -n 30 --no-pager; die "Vajra failed"; }
  echo "   → vajra :${PORT}  ✅"

  log "6/8 — Nginx HTTP (for ACME)"
  nginx_write_site "$DOMAIN" "server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN} ${WWW_ALIAS};
    location /.well-known/acme-challenge/ { root ${CERTBOT_ROOT}/${DOMAIN}; }
    location / { return 301 https://\$host\$request_uri; }
}"
  nginx -t && systemctl reload nginx

  log "7/8 — Let's Encrypt certificate"
  certbot certonly --webroot -w "${CERTBOT_ROOT}/${DOMAIN}" \
      -d "${DOMAIN}" -d "${WWW_ALIAS}" \
      --agree-tos --email "$EMAIL" --non-interactive --keep-until-expiring

  log "8/8 — Nginx HTTPS + reload"
  nginx_write_site "$DOMAIN" "server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN} ${WWW_ALIAS};
    location /.well-known/acme-challenge/ { root ${CERTBOT_ROOT}/${DOMAIN}; }
    location / { return 301 https://\$host\$request_uri; }
}
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
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
        proxy_set_header X-Forwarded-Host  \$host;
        proxy_set_header Connection        \"\";
        proxy_read_timeout 120s;
        proxy_buffering off;
    }
}"
  nginx -t && systemctl reload nginx

  echo
  echo "════════════════════════════════════════════════════════════════"
  echo "✅ Site ready:  https://${DOMAIN}/   [${MODE}]"
  if [[ "$MODE" == "wordpress" ]]; then
    echo "   Admin:       https://${DOMAIN}/wp-admin/"
    echo "   Admin user:  ${ADMIN_USER}"
    echo "   Admin pass:  ${ADMIN_PASS}"
    echo "   DB name:     ${DB_NAME}"
    echo "   DB pass:     ${DB_PASS}"
  fi
  echo "   Site root:   ${SITE_ROOT}"
  echo "   Vajra port:  ${PORT}"
  echo "   PHP:         ${PHP_VER} (socket ${PHP_SOCK})"
  echo "════════════════════════════════════════════════════════════════"
}

# ─── cmd_update ──────────────────────────────────────────────────────────
cmd_update() {
  [[ $# -ge 2 ]] || die "Usage: vajra-site update php <domain> [version]"
  local what="$1"; shift
  case "$what" in
    php) cmd_update_php "$@" ;;
    *)   die "Unknown update target: $what (only 'php' is supported)" ;;
  esac
}

cmd_update_php() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site update php <domain> [version]"
  local D="$1"; shift
  local NEW_VER="${1:-}"
  require_site "$D"

  local f="${SITES_DIR}/${D}.env"
  local MODE; MODE="$(read_env "$f" MODE)"; MODE="${MODE:-wordpress}"
  [[ "$MODE" == "html" ]] && die "Site '$D' is HTML mode — no PHP to update"

  local CURRENT_VER; CURRENT_VER="$(read_env "$f" PHP_VER)"
  local CURRENT_SOCK; CURRENT_SOCK="$(read_env "$f" PHP_SOCK)"

  echo ""
  echo "Site:           $D"
  echo "Mode:           $MODE"
  echo "Current PHP:    ${CURRENT_VER:-unknown} (socket ${CURRENT_SOCK:-unknown})"
  echo ""
  echo "Installed PHP versions:"
  local versions
  versions="$(php_versions_installed)"
  if [[ -z "$versions" ]]; then
    die "No PHP versions detected on this host"
  fi
  local i=0 v
  while IFS= read -r v; do
    i=$((i+1))
    local mark=" "
    [[ "$v" == "$CURRENT_VER" ]] && mark="*"
    printf "   %s %d) %s\n" "$mark" "$i" "$v"
  done <<< "$versions"
  echo ""
  echo "   (* = currently active for this site)"
  echo ""

  if [[ -z "$NEW_VER" ]]; then
    read -r -p "Enter new version (or press Enter to abort): " NEW_VER
    [[ -z "$NEW_VER" ]] && { info "Aborted"; return; }
  fi

  if ! grep -qx "$NEW_VER" <<< "$versions"; then
    err "PHP ${NEW_VER} not installed on this host"
    echo ""
    echo "To install it, run one of:"
    case "$DISTRO_FAMILY" in
      debian)
        echo "  apt-get install -y php${NEW_VER} php${NEW_VER}-fpm php${NEW_VER}-mysql \\"
        echo "                     php${NEW_VER}-curl php${NEW_VER}-xml php${NEW_VER}-mbstring \\"
        echo "                     php${NEW_VER}-zip php${NEW_VER}-gd php${NEW_VER}-intl \\"
        echo "                     php${NEW_VER}-bcmath"
        ;;
      rhel)
        echo "  dnf install -y php${NEW_VER//./}-php-fpm php${NEW_VER//./}-php-cli \\"
        echo "                 php${NEW_VER//./}-php-mysqlnd php${NEW_VER//./}-php-gd"
        ;;
      arch)
        echo "  pacman -S php php-fpm   # Arch uses rolling version"
        ;;
    esac
    exit 1
  fi

  if [[ "$NEW_VER" == "$CURRENT_VER" ]]; then
    info "Site is already on PHP ${NEW_VER}"
    return
  fi

  local NEW_SOCK; NEW_SOCK="$(php_socket_for_version "$NEW_VER")"
  local NEW_SVC;  NEW_SVC="$(php_fpm_service_for_version "$NEW_VER")"

  if ! systemctl is-active --quiet "$NEW_SVC"; then
    warn "$NEW_SVC not running — starting"
    systemctl enable --now "$NEW_SVC" >/dev/null 2>&1 || true
    sleep 2
  fi

  if [[ ! -S "$NEW_SOCK" ]]; then
    err "Socket $NEW_SOCK not present — PHP-FPM service $NEW_SVC may not be running"
    err "Check: systemctl status $NEW_SVC"
    exit 1
  fi

  echo ""
  echo "▶ Switching site '$D' from PHP ${CURRENT_VER} → ${NEW_VER}"
  echo "   old socket: ${CURRENT_SOCK}"
  echo "   new socket: ${NEW_SOCK}"

  sed -i "s|^PHP_VER=.*|PHP_VER=${NEW_VER}|" "$f"
  sed -i "s|^PHP_SOCK=.*|PHP_SOCK=${NEW_SOCK}|" "$f"

  sed -i -E "s|^upstream *= *\"unix:[^\"]*\"|upstream         = \"unix:${NEW_SOCK}\"|" \
    "${SITES_DIR}/${D}.toml"

  systemctl restart "vajra-site@${D}.service"
  sleep 2
  if systemctl is-active --quiet "vajra-site@${D}.service"; then
    echo "   ✅ Vajra reloaded"
  else
    err "Vajra failed to restart — rolling back"
    sed -i "s|^PHP_VER=.*|PHP_VER=${CURRENT_VER}|" "$f"
    sed -i "s|^PHP_SOCK=.*|PHP_SOCK=${CURRENT_SOCK}|" "$f"
    sed -i -E "s|^upstream *= *\"unix:[^\"]*\"|upstream         = \"unix:${CURRENT_SOCK}\"|" \
      "${SITES_DIR}/${D}.toml"
    systemctl restart "vajra-site@${D}.service" || true
    exit 1
  fi

  echo ""
  echo "✅ PHP switched: ${CURRENT_VER} → ${NEW_VER} on ${D}"
  echo "   Test: https://${D}/"
}

# ─── cmd_list / cmd_info / cmd_remove ────────────────────────────────────
cmd_list() {
  local envs; envs=( "${SITES_DIR}"/*.env )
  if [[ ! -e "${envs[0]}" ]]; then info "No sites yet. Add one: vajra-site add example.com you@example.com"; return; fi
  printf '\n%-30s %-12s %-6s %-22s %-6s %s\n' "DOMAIN" "MODE" "PORT" "DB" "SVC" "CREATED"
  for f in "${envs[@]}"; do
    local d p db svc c m
    d="$(read_env "$f" DOMAIN)"; p="$(read_env "$f" PORT)"
    db="$(read_env "$f" DB_NAME)"; c="$(read_env "$f" CREATED_AT)"
    m="$(read_env "$f" MODE)"; m="${m:-wordpress}"
    [[ -z "$db" ]] && db="-"
    systemctl is-active --quiet "vajra-site@${d}.service" && svc="✅" || svc="❌"
    printf '%-30s %-12s %-6s %-22s %-6s %s\n' "$d" "$m" "$p" "$db" "$svc" "$c"
  done
  echo
}

cmd_info() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site info <domain>"
  local D="$1"; require_site "$D"; local f="${SITES_DIR}/${D}.env"
  local MODE; MODE="$(read_env "$f" MODE)"; MODE="${MODE:-wordpress}"
  echo "Domain:       $(read_env "$f" DOMAIN)"
  echo "Mode:         $MODE"
  echo "WWW alias:    $(read_env "$f" WWW_ALIAS)"
  echo "Vajra port:   $(read_env "$f" PORT)"
  echo "Site root:    $(read_env "$f" SITE_ROOT)"
  if [[ "$MODE" != "html" ]]; then
    echo "PHP version:  $(read_env "$f" PHP_VER)"
    echo "PHP socket:   $(read_env "$f" PHP_SOCK)"
  fi
  if [[ "$MODE" == "wordpress" ]]; then
    echo "DB name:      $(read_env "$f" DB_NAME)"
    echo "DB user:      $(read_env "$f" DB_USER)"
    echo "DB pass:      $(read_env "$f" DB_PASS)"
    echo "Admin user:   $(read_env "$f" ADMIN_USER)"
    echo "Admin pass:   $(read_env "$f" ADMIN_PASS)"
  fi
  echo "Service:      vajra-site@${D}.service → $(systemctl is-active "vajra-site@${D}.service" || true)"
  if [[ "$MODE" == "wordpress" ]]; then
    echo "MariaDB:      $(db_exec -sN -e 'SELECT VERSION();' 2>/dev/null || echo 'unknown')"
  fi
}

cmd_remove() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site remove <domain> [--keep-files] [--keep-db]"
  local D="$1"; shift
  local KEEP_FILES=0 KEEP_DB=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --keep-files) KEEP_FILES=1; shift;;
      --keep-db)    KEEP_DB=1; shift;;
      *) die "Unknown option: $1";;
    esac
  done
  require_site "$D"
  local f="${SITES_DIR}/${D}.env"
  local MODE; MODE="$(read_env "$f" MODE)"; MODE="${MODE:-wordpress}"
  local DB_NAME DB_USER SITE_ROOT
  DB_NAME="$(read_env "$f" DB_NAME)"; DB_USER="$(read_env "$f" DB_USER)"
  SITE_ROOT="$(read_env "$f" SITE_ROOT)"

  warn "Removing site: $D (mode: $MODE)"
  read -r -p "Continue? [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || { info "Aborted"; return; }

  systemctl disable --now "vajra-site@${D}.service" 2>/dev/null || true
  systemctl daemon-reload
  nginx_remove_site "$D"
  nginx -t && systemctl reload nginx
  certbot delete --cert-name "$D" --non-interactive 2>/dev/null || true
  rm -f "${SITES_DIR}/${D}.toml"
  rm -f "/etc/cron.d/vajra-wp-cron-${D//./_}"
  if [[ "$KEEP_DB" == 0 && "$MODE" == "wordpress" && -n "$DB_NAME" ]]; then
    db_exec -e "DROP DATABASE IF EXISTS \`${DB_NAME}\`;"
    for _h in localhost 127.0.0.1 ::1; do
      db_exec -e "DROP USER IF EXISTS '${DB_USER}'@'${_h}';" 2>/dev/null || true
    done
    db_exec -e "FLUSH PRIVILEGES;"
  fi
  [[ "$KEEP_FILES" == 0 ]] && rm -rf "$SITE_ROOT" "${CERTBOT_ROOT}/${D}"
  rm -f "$f"
  echo "✅ Site '$D' removed"
}

case "${1:-}" in
  add) shift; cmd_add "$@";;
  update) shift; cmd_update "$@";;
  list|ls) cmd_list;;
  info|show) shift; cmd_info "$@";;
  remove|rm) shift; cmd_remove "$@";;
  help|-h|--help|"") usage;;
  *) err "Unknown: $1"; usage; exit 1;;
esac
