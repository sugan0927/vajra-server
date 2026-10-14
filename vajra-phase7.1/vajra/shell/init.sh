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
