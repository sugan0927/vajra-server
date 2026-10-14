#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer  (rev 8 — multi-mode: WP / PHP / HTML)
#
# Supported:
#   • Debian 11/12/13        • Ubuntu 20.04/22.04/24.04/26.04
#   • RHEL/Alma/Rocky 9.x    • Fedora 38+
#   • CentOS Stream 9/10     • Arch / Manjaro
#   • (RHEL 8 family: io_uring is NOT available — installer refuses)
#
# Site modes:
#   vajra-site add <domain> <email>              → WordPress (default)
#   vajra-site add <domain> <email> --php        → PHP-only site
#   vajra-site add <domain> <email> --html       → Static HTML site
#   vajra-site update php <domain> [version]     → Switch PHP version
#
# Usage:
#   curl -fsSL …/install.sh | bash
#   bash install.sh example.com you@example.com
#   bash install.sh example.com you@example.com --html
#
# Env:
#   VAJRA_REPO           GitHub repo (default: sugan0927/vajra-server)
#   VAJRA_VERSION        tag or "latest"
#   VAJRA_SKIP_BINARY    "1" to keep /usr/local/bin/vajra
#   MARIADB_VERSION      mariadb-11.4 (default)
#   SITE_MODE            wordpress|php|html (default: wordpress)
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail


# ─── Dry-run shim ──────────────────────────────────────────────────────
# Wraps every destructive command so `vajra init --dry-run` performs zero
# system changes. Uses PATH interception — no edits to the body required.
DRY_RUN=0
for _a in "$@"; do [[ "$_a" == "--dry-run" ]] && DRY_RUN=1; done
if [[ $DRY_RUN -eq 1 ]]; then
  DRYDIR="$(mktemp -d)"
  for _cmd in apt apt-get dpkg systemctl service rm cp mv ln mkdir rmdir \
              chown chmod install useradd groupadd usermod deluser \
              curl wget tar unzip gzip tee; do
    cat > "$DRYDIR/$_cmd" <<'SH'
#!/usr/bin/env bash
echo "  [dry-run] $(basename "$0") $*"
exit 0
SH
    chmod +x "$DRYDIR/$_cmd"
  done
  export PATH="$DRYDIR:$PATH"
  trap 'rm -rf "$DRYDIR"' EXIT
  echo "🧪 DRY-RUN MODE — no changes will be applied"
fi
FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
SITE_MODE="${SITE_MODE:-wordpress}"
# Allow `bash install.sh domain email --html`
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

# ─── DB CLI detection (MariaDB 11+ renamed mysql → mariadb) ─────────────
db_cli() {
  if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi
}
db_exec() { "$(db_cli)" "$@"; }

# ─── io_uring strict pre-flight ─────────────────────────────────────────
io_uring_syscall_probe() {
  if ! command -v gcc >/dev/null 2>&1; then
    case "${DISTRO_FAMILY:-}" in
      debian) DEBIAN_FRONTEND=noninteractive apt-get install -y gcc >/dev/null 2>&1 || true ;;
      rhel)   dnf install -y gcc >/dev/null 2>&1 || true ;;
      arch)   pacman -S --noconfirm --needed gcc >/dev/null 2>&1 || true ;;
    esac
  fi
  command -v gcc >/dev/null 2>&1 || { echo "SKIP"; return 0; }
  local tmpd
  tmpd=$(mktemp -d 2>/dev/null) || { echo "SKIP"; return 0; }
  cat > "$tmpd/t.c" <<'CCEOF'
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>
int main(void) {
    struct io_uring_params p;
    memset(&p, 0, sizeof(p));
    long fd = syscall(__NR_io_uring_setup, 8, &p);
    if (fd < 0) { printf("FAIL %d\n", errno); return 0; }
    close((int)fd);
    printf("OK\n");
    return 0;
}
CCEOF
  local out="SKIP"
  if gcc -O0 -o "$tmpd/t" "$tmpd/t.c" 2>/dev/null; then
    out=$("$tmpd/t" 2>&1 || true)
  fi
  rm -rf "$tmpd"
  echo "$out"
}

vajra_preflight_strict() {
  local kmaj kmin
  kmaj=$(uname -r | cut -d. -f1)
  kmin=$(uname -r | cut -d. -f2 | tr -cd '0-9')
  [[ -z "$kmin" ]] && kmin=0

  if (( kmaj < 5 || (kmaj == 5 && kmin < 1) )); then
    echo "   ❌ kernel $(uname -r) < 5.1 — io_uring was introduced in Linux 5.1"
    echo "      RHEL / Alma / Rocky 8 ships kernel 4.18 with NO io_uring backport."
    return 1
  fi
  echo "   → kernel $(uname -r) ≥ 5.1  ✅"

  if [[ -r /proc/sys/kernel/io_uring_disabled ]]; then
    local v
    v=$(cat /proc/sys/kernel/io_uring_disabled)
    echo "   → /proc/sys/kernel/io_uring_disabled = $v"
    if [[ "$v" == "2" ]]; then
      echo "   ⚠  disabled — attempting sysctl enable"
      if ! sysctl -w kernel.io_uring_disabled=0 >/dev/null 2>&1; then
        echo "   ❌ cannot write — host-locked (Hyper-V / container / seccomp)"
        return 1
      fi
      mkdir -p /etc/sysctl.d
      cat > /etc/sysctl.d/99-enable-io-uring.conf <<'SYSCTL'
# Enabled by vajra-server installer for io_uring support
kernel.io_uring_disabled = 0
SYSCTL
      sysctl --system >/dev/null 2>&1 || true
      v=$(cat /proc/sys/kernel/io_uring_disabled 2>/dev/null || echo 0)
      [[ "$v" == "0" ]] || { echo "   ❌ sysctl write rejected"; return 1; }
      echo "   ✅ io_uring enabled (persistent)"
    fi
  else
    echo "   → no /proc/sys/kernel/io_uring_disabled (kernel < 6.6 — default enabled)"
  fi

  local probe
  probe=$(io_uring_syscall_probe)
  case "$probe" in
    OK)     echo "   → io_uring_setup() syscall: OK  ✅"; return 0 ;;
    "FAIL 38") echo "   ❌ io_uring_setup() → ENOSYS (syscall not implemented)"; return 1 ;;
    "FAIL 1")  echo "   ❌ io_uring_setup() → EPERM (blocked by host)"; return 1 ;;
    FAIL*)  echo "   ❌ io_uring_setup() failed: $probe"; return 1 ;;
    SKIP)   echo "   ⚠  gcc unavailable — skipping syscall test (kernel gate passed)"; return 0 ;;
  esac
}

# ─── Distro detection ───────────────────────────────────────────────────
DISTRO_ID=""; DISTRO_LIKE=""; DISTRO_VER=""; DISTRO_NAME=""; DISTRO_FAMILY=""
detect_distro() {
  [[ -f /etc/os-release ]] || die "/etc/os-release missing"
  # shellcheck disable=SC1091
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
  [[ "$(uname -m)" == "x86_64" ]] || die "Only x86_64 supported (got $(uname -m))"

  echo "   → distro: $DISTRO_NAME"
  echo "   → family: $DISTRO_FAMILY"
  echo "   → kernel: $(uname -r)"
  echo "   → user:   $(www_user)"
}

www_user() {
  case "$DISTRO_FAMILY" in
    debian) echo "www-data" ;;
    rhel)   echo "nginx"    ;;
    arch)   echo "http"     ;;
  esac
}
WWW_USER=""

_align_www_user_with_fpm() {
  local pool_conf="" fpm_user
  for f in /etc/php-fpm.d/www.conf /etc/php/8.*/fpm/pool.d/www.conf; do
    [[ -f "$f" ]] && { pool_conf="$f"; break; }
  done
  [[ -z "$pool_conf" ]] && return 0
  fpm_user="$(grep -E '^user\s*=' "$pool_conf" | head -1 | awk -F= '{print $2}' | tr -d ' ')"
  if [[ -n "$fpm_user" ]] && [[ "$fpm_user" != "$WWW_USER" ]]; then
    echo "   → PHP-FPM pool user is '$fpm_user' (installer was using '$WWW_USER')"
    echo "   → aligning WWW_USER to '$fpm_user'"
    WWW_USER="$fpm_user"
  fi
}

# ─── MariaDB helpers ────────────────────────────────────────────────────
install_mariadb_server() {
  pkg_install mariadb-server 2>/dev/null && return 0
  if [[ "$DISTRO_FAMILY" == "rhel" ]]; then
    dnf install -y MariaDB-server MariaDB-client 2>/dev/null && return 0
  fi
  if [[ "$DISTRO_FAMILY" == "debian" ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y mariadb-server 2>/dev/null && return 0
  fi
  return 1
}

mariadb_socket_path() {
  local sock=""
  command -v mariadb >/dev/null 2>&1 && sock=$(mariadb -sN -e "SELECT @@socket;" 2>/dev/null || true)
  if [[ -z "$sock" ]] && command -v mysql >/dev/null 2>&1; then
    sock=$(mysql -sN -e "SELECT @@socket;" 2>/dev/null || true)
  fi
  [[ -z "$sock" ]] && sock="/var/lib/mysql/mysql.sock"
  echo "$sock"
}

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

# ─── Nginx path layout ──────────────────────────────────────────────────
NGINX_SITES_AVAIL=""; NGINX_SITES_ENABLED=""
nginx_paths() {
  case "$DISTRO_FAMILY" in
    debian)
      NGINX_SITES_AVAIL="/etc/nginx/sites-available"
      NGINX_SITES_ENABLED="/etc/nginx/sites-enabled" ;;
    rhel|arch)
      NGINX_SITES_AVAIL="/etc/nginx/conf.d"
      NGINX_SITES_ENABLED="/etc/nginx/conf.d" ;;
  esac
  mkdir -p "$NGINX_SITES_AVAIL" "$NGINX_SITES_ENABLED"
}

# ─── Package manager ────────────────────────────────────────────────────
pkg_update() {
  case "$DISTRO_FAMILY" in
    debian) DEBIAN_FRONTEND=noninteractive apt-get update -y ;;
    rhel)   dnf -y makecache >/dev/null 2>&1 || dnf -y check-update >/dev/null 2>&1 || true ;;
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
pkg_install_soft() { pkg_install "$@" 2>/dev/null || warn "optional: $* not installed"; }

rhel_enable_extra_repos() {
  [[ "$DISTRO_FAMILY" == "rhel" ]] || return 0
  case "$DISTRO_ID" in
    rhel)
      dnf install -y epel-release 2>/dev/null || \
        dnf install -y "https://dl.fedoraproject.org/pub/epel/epel-release-latest-$(rpm -E %rhel).noarch.rpm" 2>/dev/null || true
      ;;
    *)
      dnf install -y epel-release 2>/dev/null || true
      ;;
  esac
  dnf config-manager --set-enabled crb 2>/dev/null || \
    dnf config-manager --set-enabled powertools 2>/dev/null || true
}

setup_mariadb_repo() {
  echo "   → Configuring MariaDB ${MARIADB_VERSION} (LTS) repository"
  local setup_url="https://r.mariadb.com/downloads/mariadb_repo_setup"
  local tmp_setup="/tmp/mariadb_repo_setup"
  if ! curl -fsSL "$setup_url" -o "$tmp_setup"; then
    warn "failed to download mariadb_repo_setup — falling back to distro repo"
    return 1
  fi
  chmod +x "$tmp_setup"
  if "$tmp_setup" --mariadb-server-version="${MARIADB_VERSION}" \
      --skip-maxscale --skip-tools >/dev/null 2>&1; then
    echo "   ✅ MariaDB ${MARIADB_VERSION} repository configured"
  else
    warn "mariadb_repo_setup failed — falling back to distro repo"
    rm -f "$tmp_setup"
    return 1
  fi
  rm -f "$tmp_setup"
  pkg_update
  return 0
}

# ─── Firewall ───────────────────────────────────────────────────────────
open_ports() {
  local opened=0
  if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
    ufw allow 80/tcp >/dev/null 2>&1 || true
    ufw allow 443/tcp >/dev/null 2>&1 || true
    ufw reload >/dev/null 2>&1 || true
    echo "   → ufw: opened 80 443"; opened=1
  fi
  if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld 2>/dev/null; then
    firewall-cmd --permanent --add-service=http >/dev/null 2>&1 || true
    firewall-cmd --permanent --add-service=https >/dev/null 2>&1 || true
    firewall-cmd --reload >/dev/null 2>&1 || true
    echo "   → firewalld: opened http https"; opened=1
  fi
  if command -v nft >/dev/null 2>&1 && nft list ruleset 2>/dev/null | grep -qE 'table (inet|ip) filter'; then
    for p in 80 443; do
      nft list chain inet filter input 2>/dev/null | grep -q "dport ${p} accept" \
        || nft add rule inet filter input tcp dport "${p}" accept 2>/dev/null || true
    done
    echo "   → nftables: ensured 80 443"; opened=1
  fi
  if command -v iptables >/dev/null 2>&1 && [[ "$opened" == 0 ]]; then
    for p in 80 443; do
      iptables -C INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null \
        || iptables -I INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null || true
    done
    echo "   → iptables: opened 80 443"; opened=1
  fi
  [[ "$opened" == 0 ]] && echo "   → no host firewall active"
  echo "   ⚠ cloud firewall (Vultr/Hetzner/DO/AWS) must allow 80, 443"
}

# ─── SELinux base (RHEL family, enforcing) ──────────────────────────────
selinux_fix() {
  command -v getenforce >/dev/null 2>&1 || return 0
  [[ "$(getenforce 2>/dev/null)" == "Enforcing" ]] || return 0
  pkg_install_soft policycoreutils-python-utils
  setsebool -P httpd_can_network_connect_db 1 2>/dev/null || true
  setsebool -P httpd_can_network_connect 1 2>/dev/null || true
  if [[ -S /var/lib/mysql/mysql.sock ]]; then
    chcon -t mysqld_var_run_t /var/lib/mysql/mysql.sock 2>/dev/null || true
  fi
  echo "   → SELinux base booleans set"
}

# ─── PHP ────────────────────────────────────────────────────────────────
PHP_VER=""; PHP_SOCK=""; PHP_FPM_SVC=""
setup_php() {
  case "$DISTRO_FAMILY" in
    debian)
      pkg_install curl ca-certificates gnupg lsb-release
      if [[ ! -f /etc/apt/sources.list.d/php.list ]]; then
        curl -fsSL https://packages.sury.org/php/apt.gpg \
          -o /etc/apt/trusted.gpg.d/php.gpg
        echo "deb https://packages.sury.org/php/ $(lsb_release -sc) main" \
          > /etc/apt/sources.list.d/php.list
      fi
      pkg_update
      local v
      for v in 8.5 8.4; do
        apt-cache show "php${v}-fpm" >/dev/null 2>&1 && { PHP_VER="$v"; break; }
      done
      [[ -n "$PHP_VER" ]] || die "PHP 8.5/8.4 not in SURY repo"
      pkg_install "php${PHP_VER}" "php${PHP_VER}-cli" "php${PHP_VER}-fpm" \
                  "php${PHP_VER}-mysql" "php${PHP_VER}-curl" "php${PHP_VER}-xml" \
                  "php${PHP_VER}-mbstring" "php${PHP_VER}-zip" "php${PHP_VER}-gd" \
                  "php${PHP_VER}-intl" "php${PHP_VER}-bcmath"
      pkg_install_soft "php${PHP_VER}-imagick"
      PHP_SOCK="/run/php/php${PHP_VER}-fpm.sock"
      PHP_FPM_SVC="php${PHP_VER}-fpm"
      ;;
    rhel)
      rhel_enable_extra_repos
      if [[ "$DISTRO_ID" == "fedora" ]]; then
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml \
                    php-mbstring php-zip php-gd php-intl php-bcmath
        pkg_install_soft php-imagick
      else
        if ! rpm -q remi-release >/dev/null 2>&1; then
          dnf install -y "https://rpms.remirepo.net/enterprise/remi-release-$(rpm -E %rhel).rpm" \
            || die "Failed to install Remi repository"
        fi
        dnf module reset php -y 2>/dev/null || true
        dnf module enable php:remi-8.4 -y || die "Failed to enable Remi PHP 8.4 module"
        pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml \
                    php-mbstring php-zip php-gd php-intl php-bcmath
        pkg_install_soft php-imagick
      fi
      PHP_VER="$(php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null)"
      [[ -z "$PHP_VER" ]] && PHP_VER="8.4"
      PHP_SOCK="/run/php-fpm/www.sock"
      PHP_FPM_SVC="php-fpm"
      ;;
    arch)
      pkg_install curl ca-certificates php php-fpm php-gd
      pkg_install_soft php-imagick
      PHP_VER="$(pacman -Si php 2>/dev/null | awk '/^Version/{print $3}' | cut -d. -f1,2)"
      [[ -n "$PHP_VER" ]] || PHP_VER="8"
      PHP_SOCK="/run/php-fpm/php-fpm.sock"
      PHP_FPM_SVC="php-fpm"
      ;;
  esac
  echo "   → PHP $PHP_VER (socket: $PHP_SOCK)"
}

# ─── Vajra binary ───────────────────────────────────────────────────────
install_vajra_binary() {
  if [[ "$VAJRA_SKIP_BINARY" == "1" ]]; then
    [[ -x /usr/local/bin/vajra ]] || die "/usr/local/bin/vajra missing"
    local out; out="$(/usr/local/bin/vajra --help 2>&1 || true)"
    grep -q 'thread-per-core' <<<"$out" || die "existing vajra does not run"
    warn "VAJRA_SKIP_BINARY=1 — using existing binary"
    echo "   → $(head -1 <<<"$out")"
    return
  fi

  if [[ "$VAJRA_VERSION" == "latest" ]]; then
    VAJRA_VERSION="$(curl -fsSL "https://api.github.com/repos/${VAJRA_REPO}/releases/latest" \
      | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
    if [[ -z "$VAJRA_VERSION" ]]; then
      VAJRA_VERSION="$(curl -fsIL "https://github.com/${VAJRA_REPO}/releases/latest" \
        | grep -i '^location:' | head -1 | sed 's|.*/||' | tr -d '\r')"
    fi
    [[ -n "$VAJRA_VERSION" ]] || die "Cannot resolve latest Vajra tag"
  fi
  echo "   → tag: $VAJRA_VERSION"

  local _VERSION="$VAJRA_VERSION"
  local _BASE="https://github.com/${VAJRA_REPO}/releases/download/${VAJRA_VERSION}"
  local _CANDIDATES=(
    "vajra-${_VERSION}-x86_64-linux.tar.gz"
    "vajra-${_VERSION}-x86_64-linux-musl.tar.gz"
    "vajra-${_VERSION}-x86_64-unknown-linux-musl.tar.gz"
    "vajra-${_VERSION}-linux-x86_64.tar.gz"
  )
  local TMP; TMP="$(mktemp -d)"

  local _downloaded=0
  for _asset in "${_CANDIDATES[@]}"; do
    echo "   → trying: $_asset"
    if curl -fL --retry 2 --retry-delay 1 "$_BASE/$_asset" -o "$TMP/$_asset" 2>/dev/null; then
      if tar tzf "$TMP/$_asset" >/dev/null 2>&1; then
        echo "   ✅ downloaded: $_asset"
        _downloaded=1
        tar -xzf "$TMP/$_asset" -C "$TMP"
        break
      fi
    fi
  done

  if [[ "$_downloaded" == "1" ]]; then
    local SRC
    SRC="$(find "$TMP" -type f -name vajra | head -1)"
    if [[ -x "$SRC" ]]; then
      local out; out="$("$SRC" --help 2>&1 || true)"
      if grep -q 'thread-per-core' <<<"$out" && ! grep -q 'GLIBC_' <<<"$out"; then
        install -m 0755 "$SRC" /usr/local/bin/vajra
        rm -rf "$TMP"
        echo "   → $(head -1 <<<"$out")"
        return
      fi
      warn "downloaded binary will not run: $(grep -o 'GLIBC_[0-9.]*' <<<"$out" | head -1)"
    fi
  fi
  rm -rf "$TMP"

  warn "no pre-built tarball available for ${VAJRA_VERSION}"
  warn "building Vajra from source — first build 8-10 min"
  case "$DISTRO_FAMILY" in
    debian) pkg_install build-essential pkg-config git curl ;;
    rhel)   pkg_install gcc gcc-c++ make pkgconfig git curl ;;
    arch)   pkg_install base-devel git curl ;;
  esac
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi
  local SRC_DIR="/opt/vajra-src"
  rm -rf "$SRC_DIR"
  git clone --depth 1 "https://github.com/${VAJRA_REPO}.git" "$SRC_DIR"
  pushd "$SRC_DIR/vajra-phase7.1/vajra" >/dev/null
    mkdir -p .cargo
    cat > .cargo/config.toml <<'CARGOCFG'
[profile.release]
debug = 0
strip = "symbols"
CARGOCFG
    cargo build --release --locked 2>&1 | tail -3
    install -m 0755 target/release/vajra /usr/local/bin/vajra
  popd >/dev/null
  echo "   → built: $(/usr/local/bin/vajra --help 2>&1 | head -1)"
}

# ─── Default welcome page ───────────────────────────────────────────────
install_welcome_page() {
  mkdir -p /var/www/vajra-welcome
  cat > /var/www/vajra-welcome/index.html <<'WELCOME_HTML'
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Vajra Server — Welcome</title>
<style>
  :root{--fg:#1f2328;--muted:#57606a;--accent:#0a7d36;--bg:#f6f8fa;--card:#fff;--border:#d0d7de}
  *{box-sizing:border-box}
  body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;
       font-family:system-ui,-apple-system,"Segoe UI",Roboto,Ubuntu,sans-serif;
       background:linear-gradient(135deg,#0a7d36 0%,#064d21 100%);color:var(--fg);padding:24px}
  .card{background:var(--card);border:1px solid var(--border);border-radius:14px;
        box-shadow:0 20px 60px rgba(0,0,0,.25);max-width:640px;width:100%;
        padding:40px 40px 32px;line-height:1.55}
  .badge{display:inline-block;background:var(--accent);color:#fff;font-size:12px;
         font-weight:600;letter-spacing:.08em;text-transform:uppercase;
         padding:4px 10px;border-radius:999px;margin-bottom:14px}
  h1{margin:0 0 6px;font-size:28px;color:var(--fg)}
  .sub{color:var(--muted);margin:0 0 24px;font-size:15px}
  .row{display:flex;justify-content:space-between;padding:9px 0;
       border-bottom:1px solid var(--border);font-size:14px}
  .row:last-of-type{border-bottom:none}
  .row .k{color:var(--muted)}
  .row .v{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px}
  .ok{color:var(--accent);font-weight:600}
  .cmd{background:var(--bg);border:1px solid var(--border);border-radius:8px;
       padding:12px 16px;margin-top:22px;font-family:ui-monospace,Menlo,monospace;
       font-size:13px;overflow-x:auto;white-space:pre}
  .foot{margin-top:22px;font-size:12px;color:var(--muted);text-align:center}
  code{background:var(--bg);padding:2px 6px;border-radius:4px;font-size:12px}
</style>
</head>
<body>
  <div class="card">
    <span class="badge">Vajra Server</span>
    <h1>✅ Your server is ready</h1>
    <p class="sub">No site is configured for this hostname yet.</p>
    <div class="row"><span class="k">Stack</span><span class="v">Vajra · Nginx · PHP-FPM · MariaDB</span></div>
    <div class="row"><span class="k">Reverse proxy</span><span class="v">Nginx (TLS) → Vajra (io_uring)</span></div>
    <div class="row"><span class="k">Status</span><span class="v ok">● operational</span></div>

    <div class="cmd">vajra site add example.com you@example.com            # WordPress
vajra site add example.com you@example.com --php     # PHP site
vajra site add example.com you@example.com --html    # static HTML</div>

    <div class="foot">
      Edit <code>/var/www/vajra-welcome/index.html</code> to customise this page.<br>
      Reachable only when no vhost matches the requested Host header.
    </div>
  </div>
</body>
</html>
WELCOME_HTML
  chmod 644 /var/www/vajra-welcome/index.html
}

# ─── Service verification ───────────────────────────────────────────────
ensure_service() {
  local svc="$1"
  [[ -z "$svc" ]] && return 0
  if systemctl is-active --quiet "$svc"; then
    printf "   ✅ %-22s active\n" "$svc"; return 0
  fi
  printf "   ⚠  %-22s not running — starting\n" "$svc"
  systemctl enable --now "$svc" >/dev/null 2>&1 || true
  sleep 2
  if systemctl is-active --quiet "$svc"; then
    printf "   ✅ %-22s started\n" "$svc"; return 0
  fi
  printf "   ❌ %-22s FAILED to start\n" "$svc"
  journalctl -u "$svc" -n 15 --no-pager 2>/dev/null | sed 's/^/        /' || true
  return 1
}

verify_wp_stack() {
  local errors=0
  echo ""
  echo "▶ Post-install verification"
  echo ""
  fix_nginx_default_server
  echo ""
  ensure_service mariadb || {
    warn "mariadb not running — attempting auto-heal"
    mariadb_ensure_running || true
    ensure_service mariadb || errors=$((errors+1))
  }
  [[ -n "$PHP_FPM_SVC" ]] && { ensure_service "$PHP_FPM_SVC" || errors=$((errors+1)); }
  ensure_service nginx || errors=$((errors+1))
  if ss -lntp 2>/dev/null | grep -qE ':80\b'; then
    echo "   ✅ port 80 listening"
  else
    echo "   ❌ port 80 NOT listening"; errors=$((errors+1))
  fi
  if ss -lntp 2>/dev/null | grep -qE ':3306\b'; then
    echo "   ✅ MariaDB :3306 listening"
  else
    echo "   ❌ MariaDB :3306 NOT listening"; errors=$((errors+1))
  fi
  if nginx -t >/dev/null 2>&1; then
    echo "   ✅ nginx config valid"
  else
    echo "   ❌ nginx config invalid:"
    nginx -t 2>&1 | sed 's/^/        /'; errors=$((errors+1))
  fi
  echo ""
  if (( errors == 0 )); then
    echo "   🎉 All checks passed — stack is healthy"
  else
    echo "   ⚠  $errors check(s) failed — review output above"
  fi
  return 0
}

# ─── vajra-site forwarder ───────────────────────────────────────────────
# In the new CLI layout, `vajra site ...` is the primary interface (the
# shell orchestrator is embedded in the binary). This thin wrapper keeps
# the legacy `vajra-site add ...` command name working.
install_vajra_site_wrapper() {
  cat > /usr/local/bin/vajra-site <<'FORWARDER'
#!/usr/bin/env bash
# vajra-site — thin forwarder to `vajra site`
exec /usr/local/bin/vajra site "$@"
FORWARDER
  chmod 0755 /usr/local/bin/vajra-site
}

# ─── Nginx default_server conflict resolver ─────────────────────────────
# Aggressively removes ANY enabled vhost that owns `default_server`,
# except one we explicitly designate as "keep". Backups go to
# /var/backups/vajra/nginx/ — NOT sites-enabled/, where nginx would
# still load them regardless of extension.
fix_nginx_default_server() {
  local keep="${1:-}"
  local bdir=/var/backups/vajra/nginx
  mkdir -p "$bdir"
  local ts; ts="$(date +%s)"
  local purged=0

  for f in /etc/nginx/sites-enabled/* /etc/nginx/conf.d/*.conf; do
    [[ -e "$f" || -L "$f" ]] || continue
    local name; name="$(basename "$f")"
    [[ -n "$keep" && "$name" == "$keep" ]] && continue

    if grep -qE 'listen[^;]*default_server' "$f" 2>/dev/null; then
      echo "   → disabling conflicting default_server: $name"
      local tgt="$f"
      [[ -L "$f" ]] && tgt="$(readlink -f "$f")"
      cp -a "$tgt" "$bdir/${name}.${ts}" 2>/dev/null || true
      rm -f "$f"
      purged=1
    fi
  done

  [[ $purged -eq 1 ]] && echo "   (backups in $bdir)"
  return 0
}

# ═══════════════════════════════════════════════════════════════════════
# MAIN FLOW — mode-aware installer
# ═══════════════════════════════════════════════════════════════════════
# vajra_init_main --mode={legacy|single} [--dry-run] [domain email [--wp|--php|--html]]
#
# legacy (default): nginx + per-domain vajra-site@<domain> units.
# single:           one vajra.service reading /etc/vajra/app.toml. No nginx.

vajra_init_main() {
  local VAJRA_MODE="legacy"
  local _args=()
  local a
  for a in "$@"; do
    case "$a" in
      --mode=*)  VAJRA_MODE="${a#--mode=}";;
      --dry-run) ;;   # already consumed by the shim
      *)         _args+=("$a");;
    esac
  done
  case "$VAJRA_MODE" in
    legacy|single) ;;
    *) die "Unknown --mode: $VAJRA_MODE (want legacy or single)";;
  esac
  # Rebind positionals — guard against empty array under `set -u`.
  if (( ${#_args[@]} > 0 )); then
    set -- "${_args[@]}"
  else
    set --
  fi
  FIRST_DOMAIN="${1:-}"
  FIRST_EMAIL="${2:-}"

  log "0/8 — Pre-flight"
  detect_distro
  WWW_USER="$(www_user)"
  open_ports
  echo "   → mode: $VAJRA_MODE"
  if [[ "$VAJRA_MODE" == "legacy" ]]; then
    nginx_paths
    if ss -lntp 2>/dev/null | grep -q ':80\b'; then
      systemctl stop nginx 2>/dev/null || true
      systemctl stop apache2 2>/dev/null || true
      systemctl stop httpd 2>/dev/null || true
      sleep 1
      ss -lntp 2>/dev/null | grep -q ':80\b' && { ss -lntp | grep ':80\b'; die "port 80 busy"; }
    fi
    echo "   ✅ port 80 free"
  else
    echo "   → single mode: Vajra will bind 80/443 directly"
  fi

  echo ""
  echo "▶ io_uring strict check (required by Vajra)"
  vajra_preflight_strict || { echo "❌ io_uring not available"; exit 1; }

  # ── 1/8: base packages ─────────────────────────────────────────
  log "1/8 — Base packages"
  pkg_update
  case "$DISTRO_FAMILY" in
    debian)
      local _base=(curl wget unzip ca-certificates gnupg lsb-release \
                   openssl python3 apt-transport-https software-properties-common)
      [[ "$VAJRA_MODE" == "legacy" ]] && _base+=(nginx)
      pkg_install "${_base[@]}"
      setup_mariadb_repo || true
      install_mariadb_server || die "Failed to install MariaDB (debian)"
      pkg_install certbot
      ;;
    rhel)
      rhel_enable_extra_repos
      local _base=(curl wget unzip ca-certificates gnupg2 openssl python3)
      [[ "$VAJRA_MODE" == "legacy" ]] && _base+=(nginx)
      pkg_install "${_base[@]}"
      if [[ "$DISTRO_ID" == "fedora" ]]; then
        pkg_install mariadb-server || die "Failed to install MariaDB (fedora)"
      else
        setup_mariadb_repo || true
        install_mariadb_server || die "Failed to install MariaDB (rhel)"
      fi
      pkg_install certbot || die "certbot not installable — check EPEL"
      ;;
    arch)
      local _base=(curl wget unzip ca-certificates gnupg mariadb certbot openssl python)
      [[ "$VAJRA_MODE" == "legacy" ]] && _base+=(nginx)
      pkg_install "${_base[@]}"
      ;;
  esac
  [[ "$DISTRO_FAMILY" == "rhel" ]] && \
    { mariadb_ensure_running || warn "MariaDB not ready yet — will retry at verify"; }

  # ── 2/8: PHP ───────────────────────────────────────────────────
  log "2/8 — PHP"
  setup_php
  _align_www_user_with_fpm
  if [[ "$DISTRO_FAMILY" == "debian" ]] && command -v update-alternatives >/dev/null 2>&1; then
    update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
  fi
  php -v | head -1

  # ── 3/8: Vajra binary ──────────────────────────────────────────
  log "3/8 — Vajra binary"
  install_vajra_binary

  # ── 4/8: WP-CLI ────────────────────────────────────────────────
  log "4/8 — WP-CLI"
  if ! command -v wp >/dev/null 2>&1; then
    curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar \
      -o /usr/local/bin/wp
    chmod 0755 /usr/local/bin/wp
  fi
  wp --version --allow-root 2>/dev/null || true

  # ── 5/8: directories + shared scaffolding ──────────────────────
  log "5/8 — Directories + runtime scaffolding"
  mkdir -p /etc/vajra/sites /etc/vajra/certs /var/log/vajra \
           /var/www/sites /var/www/certbot
  chown -R "$WWW_USER:$WWW_USER" /var/log/vajra /var/www/sites
  selinux_fix
  install_vajra_site_wrapper
  echo "   → /usr/local/bin/vajra-site → vajra site"

  if [[ "$VAJRA_MODE" == "legacy" ]]; then
    cat > /etc/systemd/system/vajra-site@.service <<UNIT
[Unit]
Description=Vajra web server for %i
After=network.target \${PHP_FPM_SVC}.service
Wants=\${PHP_FPM_SVC}.service

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
    echo "   → /etc/systemd/system/vajra-site@.service"
  else
    echo ""
    echo "▶ Single-process scaffolding"

    if [[ ! -f /etc/vajra/globals.toml ]]; then
      cat > /etc/vajra/globals.toml <<'GLOBALS'
[server]
listen     = "0.0.0.0:80"
grace_secs = 10
workers    = 0
pin        = true

[limits]
max_body_bytes = 67108864

[cache]
max_entries      = 4096
max_bytes        = 67108864
max_object_bytes = 1048576

[logging]
access_log = "/var/log/vajra/access.log"

[admin]
listen = "127.0.0.1:9100"
GLOBALS
      echo "   → /etc/vajra/globals.toml"
    fi

    echo single > /etc/vajra/mode
    echo "   → /etc/vajra/mode = single"

    if [[ -f "$(dirname "$0")/vajra-config-gen" ]]; then
      install -m 0755 "$(dirname "$0")/vajra-config-gen" /usr/local/bin/vajra-config-gen
    fi
    [[ -x /usr/local/bin/vajra-config-gen ]] && echo "   → /usr/local/bin/vajra-config-gen"

    if [[ -x /usr/local/bin/vajra-config-gen ]]; then
      /usr/local/bin/vajra-config-gen || warn "config-gen failed at bootstrap"
    fi

    cat > /etc/systemd/system/vajra.service <<'UNIT'
[Unit]
Description=Vajra web server (single process, multi-site)
After=network.target

[Service]
Type=simple
User=root
ExecStartPre=/bin/mkdir -p /var/www/certbot/.well-known/acme-challenge
ExecStart=/usr/local/bin/vajra --config /etc/vajra/app.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=3
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT
    systemctl daemon-reload
    echo "   → /etc/systemd/system/vajra.service"


    # Phase 1b.5: install certbot deploy hook so renewals reload vajra.
    if [[ -f "$(dirname "$0")/certbot-hooks/10-vajra-reload.sh" ]]; then
      install -d -m 0755 /etc/letsencrypt/renewal-hooks/deploy 2>/dev/null || true
      install -m 0755 "$(dirname "$0")/certbot-hooks/10-vajra-reload.sh" \
        /etc/letsencrypt/renewal-hooks/deploy/10-vajra-reload.sh 2>/dev/null || \
        warn "deploy hook install skipped (no /etc/letsencrypt)"
      echo "   → /etc/letsencrypt/renewal-hooks/deploy/10-vajra-reload.sh"
    fi

    # Phase 1b.5: seed ACME contact email if not present
    if [[ -n "${FIRST_EMAIL:-}" && ! -f /etc/vajra/certbot.email ]]; then
      printf '%s\n' "$FIRST_EMAIL" > /etc/vajra/certbot.email
      chmod 0644 /etc/vajra/certbot.email
      echo "   → /etc/vajra/certbot.email"
    fi
    if /usr/local/bin/vajra --config /etc/vajra/app.toml --check >/dev/null 2>&1; then
      systemctl enable --now vajra.service
      echo "   ✅ vajra.service started"
    else
      warn "initial /etc/vajra/app.toml failed --check — not starting service"
    fi
  fi

  # ── 6/8: HTTP front-end ────────────────────────────────────────
  if [[ "$VAJRA_MODE" == "legacy" ]]; then
    log "6/8 — Nginx base + welcome page"
    nginx_paths
    fix_nginx_default_server
    rm -f "$NGINX_SITES_ENABLED/default" 2>/dev/null || true
    install_welcome_page

    cat > "\${NGINX_SITES_AVAIL}/_vajra-welcome.conf" <<'NGINXBASE'
# Vajra — canonical default_server (catch-all)
server {
    listen 80 default_server;
    listen [::]:80 default_server;
    server_name _;

    location /.well-known/acme-challenge/ { root /var/www/certbot; }

    root /var/www/vajra-welcome;
    index index.html;
    location / { try_files $uri $uri/ /index.html; }
}
NGINXBASE
    [[ "$DISTRO_FAMILY" == "debian" ]] && \
      ln -sf "\${NGINX_SITES_AVAIL}/_vajra-welcome.conf" \
             "\${NGINX_SITES_ENABLED}/_vajra-welcome.conf"
    fix_nginx_default_server "_vajra-welcome.conf"

    if nginx -t 2>/dev/null; then
      systemctl enable --now nginx
      systemctl restart nginx
      echo "   ✅ nginx base up (welcome page active)"
    else
      warn "nginx config test failed — attempting auto-heal"
      nginx -t 2>&1 | sed 's/^/      /'
      fix_nginx_default_server "_vajra-welcome.conf"
      if nginx -t 2>/dev/null; then
        systemctl enable --now nginx
        systemctl restart nginx
        echo "   ✅ nginx base up (after auto-heal)"
      else
        die "nginx config still invalid"
      fi
    fi
  else
    log "6/8 — HTTP front-end (single mode — Vajra binds directly)"
    echo "   → skipped nginx; Vajra owns :80/:443 per globals.toml"
  fi

  # ── 7/8: site CLI ──────────────────────────────────────────────
  log "7/8 — Site CLI"
  echo "   → /usr/local/bin/vajra-site → vajra site"

  # ── 8/8: done ──────────────────────────────────────────────────
  log "8/8 — Done"
  if [[ "$VAJRA_MODE" == "legacy" ]]; then
    verify_wp_stack
  else
    echo ""
    echo "▶ Post-install verification (single mode)"
    ensure_service mariadb || true
    ensure_service "$PHP_FPM_SVC" || true
    if systemctl is-active --quiet vajra.service; then
      echo "   ✅ vajra.service active"
    else
      echo "   ⚠  vajra.service not active — check: systemctl status vajra"
    fi
    /usr/local/bin/vajra --config /etc/vajra/app.toml --check >/dev/null 2>&1 \
      && echo "   ✅ /etc/vajra/app.toml --check OK" \
      || echo "   ❌ /etc/vajra/app.toml --check FAILED"
  fi

  echo
  echo "✅ Vajra stack installed on $DISTRO_NAME  [mode: $VAJRA_MODE]"
  echo
  echo "   MariaDB: $(db_exec -sN -e 'SELECT VERSION();' 2>/dev/null || echo 'unknown')"
  echo "   PHP:     $PHP_VER  (socket $PHP_SOCK)"
  if [[ "$VAJRA_MODE" == "legacy" ]]; then
    echo "   Welcome: http://$(hostname -I 2>/dev/null | awk '{print $1}')/"
  else
    echo "   Config:  /etc/vajra/app.toml"
    echo "   Mode:    /etc/vajra/mode = single"
  fi
  echo ""
  echo "   Manage sites with:"
  echo "     vajra site add <domain> <email>              # WordPress"
  echo "     vajra site add <domain> <email> --php        # PHP only"
  echo "     vajra site add <domain> <email> --html       # static HTML"
  echo "     vajra site update php <domain> [version]"
  echo "     vajra site list | info <domain> | remove <domain>"
  echo "     vajra list"
  echo

  if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
    log "Creating first site: $FIRST_DOMAIN"
    case "$SITE_MODE" in
      php)  /usr/local/bin/vajra site add "$FIRST_DOMAIN" "$FIRST_EMAIL" --php ;;
      html) /usr/local/bin/vajra site add "$FIRST_DOMAIN" "$FIRST_EMAIL" --html ;;
      *)    /usr/local/bin/vajra site add "$FIRST_DOMAIN" "$FIRST_EMAIL" ;;
    esac
  fi
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  vajra_init_main "$@"
fi
