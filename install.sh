#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer  (rev 9 — cross-distro nginx + certbot fixes)
#
# Supported:
#   • Debian 11/12/13        • Ubuntu 20.04/22.04/24.04/26.04
#   • RHEL/Alma/Rocky 9.x    • Fedora 38+
#   • CentOS Stream 9/10     • Arch / Manjaro
#   • (RHEL 8 family: io_uring is NOT available — installer refuses)
#
# Rev 9 fixes:
#   • fix: move disabled vhosts OUT of nginx include glob (Debian glob bug)
#   • fix: welcome vhost renamed to _vajra-welcome.conf (never conflict)
#   • fix: skip our own *vajra* files during default_server cleanup
#   • fix: cleanup leftover *.disabled-by-vajra files on every run
#   • feat: --skip-ssl flag for vajra-site add
#   • feat: auto-skip SSL for reserved TLDs (.local/.test/.invalid)
#   • feat: reject ACME-reserved emails (example.com/org/net)
#   • fix: graceful HTTP-only fallback when certbot fails
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
SITE_MODE="${SITE_MODE:-wordpress}"
FIRST_EXTRA_ARGS=()
if [[ "${3:-}" == "--php"  ]]; then SITE_MODE="php";  FIRST_EXTRA_ARGS=(--php);  fi
if [[ "${3:-}" == "--html" ]]; then SITE_MODE="html"; FIRST_EXTRA_ARGS=(--html); fi
if [[ "${3:-}" == "--skip-ssl" ]]; then FIRST_EXTRA_ARGS=(--skip-ssl); fi
if [[ "${4:-}" == "--skip-ssl" ]]; then FIRST_EXTRA_ARGS+=(--skip-ssl); fi

VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"
MARIADB_VERSION="${MARIADB_VERSION:-mariadb-11.4}"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }

db_cli() {
  if command -v mariadb >/dev/null 2>&1; then echo "mariadb"; else echo "mysql"; fi
}
db_exec() { "$(db_cli)" "$@"; }

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

fix_nginx_default_server() {
  local fixed=0
  local disabled_dir="/etc/nginx/vajra-disabled"
  mkdir -p "$disabled_dir"

  for f in /etc/nginx/sites-enabled/default \
           /etc/nginx/sites-enabled/000-default.conf \
           /etc/nginx/conf.d/default.conf \
           /etc/nginx/conf.d/welcome.conf; do
    [[ -L "$f" || -f "$f" ]] || continue
    local base; base="$(basename "$f")"
    [[ "$base" == *vajra* ]] && continue
    if [[ -L "$f" ]]; then
      local target; target="$(readlink -f "$f" 2>/dev/null || true)"
      [[ "$target" == *vajra* ]] && continue
    fi
    mv -f "$f" "${disabled_dir}/${base}.vajra-disabled" 2>/dev/null && {
      echo "   → disabled: $f  (moved to ${disabled_dir}/)"
      fixed=1
    }
  done

  local leftover
  for leftover in /etc/nginx/sites-enabled/*.disabled-by-vajra \
                  /etc/nginx/conf.d/*.disabled-by-vajra; do
    [[ -e "$leftover" ]] || continue
    local base; base="$(basename "$leftover")"
    mv -f "$leftover" "${disabled_dir}/${base}" 2>/dev/null && {
      echo "   → cleaned leftover: $leftover"
      fixed=1
    }
  done

  if [[ -f /etc/nginx/nginx.conf ]] && grep -q 'default_server' /etc/nginx/nginx.conf 2>/dev/null; then
    cp -f /etc/nginx/nginx.conf /etc/nginx/nginx.conf.vajra-bak 2>/dev/null || true
    if command -v python3 >/dev/null 2>&1; then
      python3 - /etc/nginx/nginx.conf <<'PYEOF' >/dev/null 2>&1 || true
import re, sys
p = sys.argv[1]
with open(p) as f: src = f.read()
out, i, n = [], 0, len(src)
server_re = re.compile(r'server\s*\{')
while i < n:
    m = server_re.search(src, i)
    if not m:
        out.append(src[i:]); break
    out.append(src[i:m.start()])
    j, depth = m.end(), 1
    while j < n and depth > 0:
        if src[j] == '{': depth += 1
        elif src[j] == '}': depth -= 1
        j += 1
    block = src[m.start():j]
    if 'default_server' in block:
        commented = '\n'.join(
            ('# VAJRA-DISABLED ' + line) if line.strip() else line
            for line in block.split('\n')
        )
        out.append(commented)
    else:
        out.append(block)
    i = j
with open(p, 'w') as f: f.write(''.join(out))
PYEOF
      echo "   → commented out default server block in /etc/nginx/nginx.conf"
    else
      sed -i -E 's/(listen\s+(\[::\]:)?80)\s+default_server;/\1;/' /etc/nginx/nginx.conf
    fi
    fixed=1
  fi

  [[ "$fixed" == "1" ]] && echo "   ✅ nginx default server conflicts resolved"
  return 0
}

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
    <div class="cmd">vajra-site add example.com you@example.com            # WordPress
vajra-site add example.com you@example.com --php     # PHP site
vajra-site add example.com you@example.com --html    # static HTML</div>
    <div class="foot">
      Edit <code>/var/www/vajra-welcome/index.html</code> to customise this page.
    </div>
  </div>
</body>
</html>
WELCOME_HTML
  chmod 644 /var/www/vajra-welcome/index.html
}

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

log "0/8 — Pre-flight"
detect_distro
nginx_paths
WWW_USER="$(www_user)"
open_ports
if ss -lntp 2>/dev/null | grep -q ':80\b'; then
  systemctl stop nginx 2>/dev/null || true
  systemctl stop apache2 2>/dev/null || true
  systemctl stop httpd 2>/dev/null || true
  sleep 1
  ss -lntp 2>/dev/null | grep -q ':80\b' && { ss -lntp | grep ':80\b'; die "port 80 busy"; }
fi
echo "   ✅ port 80 free"

echo ""
echo "▶ io_uring strict check (required by Vajra)"
if ! vajra_preflight_strict; then
  cat >&2 <<'GUARDEOF'

╔═══════════════════════════════════════════════════════════════════════╗
║  ❌  Vajra cannot run on this host                                    ║
╠═══════════════════════════════════════════════════════════════════════╣
║  Vajra is an io_uring-based web server. io_uring is not available.    ║
║                                                                       ║
║  Common causes:                                                       ║
║    • RHEL / Alma / Rocky 8.x  (kernel 4.18 — io_uring not present)    ║
║    • Hyper-V VPS              (host blocks io_uring_setup)            ║
║    • Docker / LXC             (seccomp filter)                        ║
║                                                                       ║
║  Options:                                                             ║
║    1. Use Debian 11/12/13, Ubuntu 20.04+, or RHEL/Alma/Rocky 9.x      ║
║    2. Use a KVM-based VPS (Hetzner / DO / Linode / Vultr KVM)         ║
║    3. Install Nginx + PHP-FPM without Vajra (WordPress still works)   ║
║                                                                       ║
║  Debug: uname -r ; cat /proc/sys/kernel/io_uring_disabled             ║
╚═══════════════════════════════════════════════════════════════════════╝

GUARDEOF
  exit 1
fi

log "1/8 — Base packages"
pkg_update
case "$DISTRO_FAMILY" in
  debian)
    pkg_install curl wget unzip ca-certificates gnupg lsb-release \
                nginx openssl python3 apt-transport-https \
                software-properties-common
    setup_mariadb_repo || true
    install_mariadb_server || die "Failed to install MariaDB (debian)"
    pkg_install certbot
    ;;
  rhel)
    rhel_enable_extra_repos
    pkg_install curl wget unzip ca-certificates gnupg2 openssl python3
    pkg_install nginx
    if [[ "$DISTRO_ID" == "fedora" ]]; then
      pkg_install mariadb-server || die "Failed to install MariaDB (fedora)"
    else
      setup_mariadb_repo || true
      install_mariadb_server || die "Failed to install MariaDB (rhel)"
    fi
    pkg_install certbot || die "certbot not installable — check EPEL is reachable"
    ;;
  arch)
    pkg_install curl wget unzip ca-certificates gnupg \
                nginx mariadb certbot openssl python
    ;;
esac

case "$DISTRO_FAMILY" in
  rhel) mariadb_ensure_running || warn "MariaDB not ready yet — will retry at verify" ;;
esac

log "2/8 — PHP"
setup_php
_align_www_user_with_fpm
if [[ "$DISTRO_FAMILY" == "debian" ]] && command -v update-alternatives >/dev/null 2>&1; then
  update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
fi
php -v | head -1

log "3/8 — Vajra binary"
install_vajra_binary

log "4/8 — WP-CLI"
if ! command -v wp >/dev/null 2>&1; then
  curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar \
    -o /usr/local/bin/wp
  chmod 0755 /usr/local/bin/wp
fi
wp --version --allow-root 2>/dev/null || true

log "5/8 — Directories + systemd template"
mkdir -p /etc/vajra/sites /etc/vajra/certs /var/log/vajra \
         /var/www/sites /var/www/certbot
chown -R "$WWW_USER:$WWW_USER" /var/log/vajra /var/www/sites

selinux_fix

cat > /etc/systemd/system/vajra-site@.service <<UNIT
[Unit]
Description=Vajra web server for %i
After=network.target ${PHP_FPM_SVC}.service
Wants=${PHP_FPM_SVC}.service

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

log "6/8 — Nginx base + welcome page"
rm -f /etc/nginx/sites-enabled/default \
      /etc/nginx/sites-enabled/000-default.conf 2>/dev/null || true
find /etc/nginx/sites-enabled /etc/nginx/conf.d \
     -maxdepth 1 -name '*.disabled-by-vajra' -delete 2>/dev/null || true

fix_nginx_default_server
install_welcome_page

cat > "${NGINX_SITES_AVAIL}/_vajra-welcome.conf" <<'NGINXBASE'
# Vajra — default server (catch-all for unrecognised Host headers)
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
  ln -sf "${NGINX_SITES_AVAIL}/_vajra-welcome.conf" \
         "${NGINX_SITES_ENABLED}/_vajra-welcome.conf"

if nginx -t 2>/dev/null; then
  systemctl enable --now nginx
  systemctl restart nginx
  echo "   ✅ nginx base up (welcome page active)"
else
  warn "nginx config test failed — attempting auto-heal"
  nginx -t 2>&1 | sed 's/^/      /'
  rm -f /etc/nginx/sites-enabled/*.disabled-by-vajra 2>/dev/null || true
  fix_nginx_default_server
  if nginx -t 2>/dev/null; then
    systemctl enable --now nginx
    systemctl restart nginx
    echo "   ✅ nginx base up (after auto-heal)"
  else
    die "nginx config still invalid — check /etc/nginx/nginx.conf"
  fi
fi

log "7/8 — Installing vajra-site CLI"
cat > /usr/local/bin/vajra-site <<'VAJRA_SITE_TEMPLATE'
#!/usr/bin/env bash
# vajra-site — manage Vajra sites (WordPress / PHP / static HTML)
# Rev 9 — --skip-ssl, reserved-domain detection, graceful certbot fallback
set -euo pipefail

DISTRO_FAMILY="__FAMILY__"
NGINX_SITES_AVAIL="__NGINX_AVAIL__"
NGINX_SITES_ENABLED="__NGINX_ENABLED__"
WWW_USER="__WWW_USER__"
PHP_SOCK="__PHP_SOCK__"
PHP_VER="__PHP_VER__"
PHP_FPM_SVC="__PHP_FPM_SVC__"

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
  vajra-site add <domain> <email> [--php|--html|--wordpress] [--skip-ssl] [opts]
  vajra-site list
  vajra-site info <domain>
  vajra-site update php <domain> [version]
  vajra-site remove <domain> [--keep-files] [--keep-db]
  vajra-site help

SITE MODES (default: --wordpress):
  --wordpress / --wp    Full WordPress install
  --php                 PHP-only site
  --html                Static HTML site

WORDPRESS OPTIONS:
  --title "Site Title"
  --admin-user NAME
  --admin-pass PASS

SSL OPTIONS:
  --skip-ssl            Skip Let's Encrypt (HTTP-only)
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

is_reserved_domain() {
  case "$1" in
    *.local|*.localhost|*.test|*.invalid|*.example|localhost) return 0 ;;
    *) return 1 ;;
  esac
}

is_reserved_email() {
  case "$1" in
    *@example.com|*@example.org|*@example.net|*@localhost|*@test|*@invalid) return 0 ;;
    *) return 1 ;;
  esac
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
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/wp-content(/.*)?" 2>/dev/null || true
      semanage fcontext -a -t httpd_sys_rw_content_t "${site_root}/[^/]+\.php" 2>/dev/null || true
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

write_php_welcome() {
  local domain="$1" site_root="$2"
  [[ -f "$site_root/index.php" ]] && return 0
  cat > "$site_root/index.php" <<PHP
<?php
header('Content-Type: text/html; charset=utf-8');
?>
<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>${domain} — PHP site is live</title></head>
<body style="font-family:system-ui;max-width:720px;margin:60px auto;padding:0 24px">
  <h1 style="color:#0a7d36">✅ PHP is working on ${domain}</h1>
  <p><strong>PHP:</strong> <?php echo PHP_VERSION; ?></p>
  <p>Edit <code>index.php</code> in <code>/var/www/sites/${domain}/</code>.</p>
</body></html>
PHP
  chown "${WWW_USER}:${WWW_USER}" "$site_root/index.php"
  chmod 644 "$site_root/index.php"
}

write_html_welcome() {
  local domain="$1" site_root="$2"
  [[ -f "$site_root/index.html" ]] && return 0
  cat > "$site_root/index.html" <<HTML
<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>${domain} — site is live</title></head>
<body style="font-family:system-ui;max-width:720px;margin:60px auto;padding:0 24px">
  <h1 style="color:#0a7d36">✅ ${domain} is live</h1>
  <p>Static HTML site served by <strong>Vajra</strong> + <strong>Nginx</strong>.</p>
  <p>Edit files in <code>/var/www/sites/${domain}/</code>.</p>
</body></html>
HTML
  chown "${WWW_USER}:${WWW_USER}" "$site_root/index.html"
  chmod 644 "$site_root/index.html"
}

cmd_add() {
  [[ $# -ge 2 ]] || { err "Usage: vajra-site add <domain> <email> [--php|--html|--wordpress] [--skip-ssl] [opts]"; exit 1; }
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local TITLE="WordPress" ADMIN_USER="wpadmin" ADMIN_PASS=""
  local MODE="wordpress"
  local SKIP_SSL=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --title)       TITLE="$2"; shift 2;;
      --admin-user)  ADMIN_USER="$2"; shift 2;;
      --admin-pass)  ADMIN_PASS="$2"; shift 2;;
      --php)         MODE="php"; shift;;
      --html)        MODE="html"; shift;;
      --wordpress|--wp) MODE="wordpress"; shift;;
      --skip-ssl|--no-ssl) SKIP_SSL=1; shift;;
      *) die "Unknown option: $1";;
    esac
  done

  valid_domain "$DOMAIN" || die "Invalid domain: $DOMAIN"
  valid_email  "$EMAIL"  || die "Invalid email: $EMAIL"
  [[ -f "${SITES_DIR}/${DOMAIN}.env" ]] && die "Site '$DOMAIN' already exists"

  if is_reserved_domain "$DOMAIN"; then
    warn "Domain '$DOMAIN' uses a reserved TLD — Let's Encrypt cannot issue certificates"
    warn "Auto-enabling --skip-ssl (site will be HTTP-only)"
    SKIP_SSL=1
  fi
  if is_reserved_email "$EMAIL"; then
    warn "Email '$EMAIL' uses a reserved domain — Let's Encrypt will reject it"
    if [[ "$SKIP_SSL" == "0" ]]; then
      die "Provide a real email, or pass --skip-ssl"
    fi
  fi

  ensure_stack "$MODE"

  local WWW_ALIAS="www.${DOMAIN}"
  local PORT; PORT="$(next_port)"
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"

  log "1/8 — Allocating resources"
  echo "   → domain: $DOMAIN (+ $WWW_ALIAS)"
  echo "   → mode:   $MODE"
  echo "   → port:   $PORT"
  echo "   → ssl:    $([[ "$SKIP_SSL" == "1" ]] && echo "disabled" || echo "enabled")"

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
SSL_ENABLED=$([[ "$SKIP_SSL" == "1" ]] && echo "no" || echo "yes")
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

  local SSL_OK=0
  if [[ "$SKIP_SSL" == "1" ]]; then
    log "7/8 — Skipping Let's Encrypt (--skip-ssl / reserved domain)"
  else
    log "7/8 — Let's Encrypt certificate"
    if certbot certonly --webroot -w "${CERTBOT_ROOT}/${DOMAIN}" \
        -d "${DOMAIN}" -d "${WWW_ALIAS}" \
        --agree-tos --email "$EMAIL" --non-interactive --keep-until-expiring; then
      SSL_OK=1
    else
      warn "certbot failed — falling back to HTTP-only"
      warn "  check: certbot certificates; dig +short ${DOMAIN}"
    fi
  fi

  if [[ "$SSL_OK" == "1" ]]; then
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
  else
    log "8/8 — Nginx HTTP-only (no TLS)"
    nginx_write_site "$DOMAIN" "server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN} ${WWW_ALIAS};
    client_max_body_size 64m;
    location /.well-known/acme-challenge/ { root ${CERTBOT_ROOT}/${DOMAIN}; }
    location / {
        proxy_pass http://127.0.0.1:${PORT};
        proxy_http_version 1.1;
        proxy_set_header Host              \$host;
        proxy_set_header X-Real-IP         \$remote_addr;
        proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto http;
        proxy_set_header Connection        \"\";
        proxy_read_timeout 120s;
        proxy_buffering off;
    }
}"
  fi
  nginx -t && systemctl reload nginx

  echo
  echo "════════════════════════════════════════════════════════════════"
  if [[ "$SSL_OK" == "1" ]]; then
    echo "✅ Site ready:  https://${DOMAIN}/   [${MODE}]"
  else
    echo "✅ Site ready:  http://${DOMAIN}/   [${MODE}, HTTP-only]"
  fi
  if [[ "$MODE" == "wordpress" ]]; then
    echo "   Admin:       http${SSL_OK:+s}://${DOMAIN}/wp-admin/"
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
    err "Socket $NEW_SOCK not present — service $NEW_SVC may not be running"
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
}

cmd_list() {
  local envs; envs=( "${SITES_DIR}"/*.env )
  if [[ ! -e "${envs[0]}" ]]; then info "No sites yet."; return; fi
  printf '\n%-30s %-12s %-6s %-22s %-6s %-4s %s\n' "DOMAIN" "MODE" "PORT" "DB" "SVC" "SSL" "CREATED"
  for f in "${envs[@]}"; do
    local d p db svc c m ssl
    d="$(read_env "$f" DOMAIN)"; p="$(read_env "$f" PORT)"
    db="$(read_env "$f" DB_NAME)"; c="$(read_env "$f" CREATED_AT)"
    m="$(read_env "$f" MODE)"; m="${m:-wordpress}"
    ssl="$(read_env "$f" SSL_ENABLED)"; ssl="${ssl:-yes}"
    [[ -z "$db" ]] && db="-"
    [[ "$ssl" == "yes" ]] && ssl="yes" || ssl="no"
    systemctl is-active --quiet "vajra-site@${d}.service" && svc="OK" || svc="FAIL"
    printf '%-30s %-12s %-6s %-22s %-6s %-4s %s\n' "$d" "$m" "$p" "$db" "$svc" "$ssl" "$c"
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
  echo "SSL:          $(read_env "$f" SSL_ENABLED)"
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
VAJRA_SITE_TEMPLATE

sed -i \
  -e "s|__FAMILY__|${DISTRO_FAMILY}|g" \
  -e "s|__NGINX_AVAIL__|${NGINX_SITES_AVAIL}|g" \
  -e "s|__NGINX_ENABLED__|${NGINX_SITES_ENABLED}|g" \
  -e "s|__WWW_USER__|${WWW_USER}|g" \
  -e "s|__PHP_SOCK__|${PHP_SOCK}|g" \
  -e "s|__PHP_VER__|${PHP_VER}|g" \
  -e "s|__PHP_FPM_SVC__|${PHP_FPM_SVC}|g" \
  /usr/local/bin/vajra-site
chmod 0755 /usr/local/bin/vajra-site
echo "   ✅ vajra-site CLI installed"

log "8/8 — Done"

verify_wp_stack

echo
echo "✅ Vajra stack installed on $DISTRO_NAME"
echo
echo "   MariaDB: $(db_exec -sN -e 'SELECT VERSION();' 2>/dev/null || echo 'unknown')"
echo "   Welcome: http://$(hostname -I 2>/dev/null | awk '{print $1}')/  (default vhost)"
echo ""
echo "   Manage sites with:"
echo "     vajra-site add <domain> <email>                     # WordPress"
echo "     vajra-site add <domain> <email> --php               # PHP only"
echo "     vajra-site add <domain> <email> --html              # static HTML"
echo "     vajra-site add <domain> <email> --html --skip-ssl   # HTTP-only"
echo "     vajra-site update php <domain> [version]            # switch PHP"
echo "     vajra-site list"
echo "     vajra-site info <domain>"
echo "     vajra-site remove <domain>"
echo

if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
  log "Creating first site: $FIRST_DOMAIN (mode: $SITE_MODE)"
  case "$SITE_MODE" in
    php)  /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" --php "${FIRST_EXTRA_ARGS[@]}" ;;
    html) /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" --html "${FIRST_EXTRA_ARGS[@]}" ;;
    *)    /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL" "${FIRST_EXTRA_ARGS[@]}" ;;
  esac
fi

# ─────────────────────────────────────────────────────────────────────
# Install vajra-harden (security hardening tool)
# ─────────────────────────────────────────────────────────────────────
log "Installing vajra-harden (security hardening)"

# Required directories for hardening (idempotent)
mkdir -p /etc/vajra/hardening/fintech-ca
chmod 700 /etc/vajra/hardening/fintech-ca
mkdir -p /etc/nginx/conf.d
mkdir -p /etc/nginx/vajra-disabled
mkdir -p /etc/nginx/snippets

# Install vajra-harden from the repo directory
HARDEN_SRC=""
for cand in \
    "$(dirname "$0")/vajra-harden" \
    "$(dirname "$0")/bin/vajra-harden" \
    "./vajra-harden" \
    "/root/vajra-server/vajra-harden"; do
  if [[ -f "$cand" ]]; then HARDEN_SRC="$cand"; break; fi
done

if [[ -z "$HARDEN_SRC" ]]; then
    warn "vajra-harden source not found — skipping (hardening unavailable)"
else
    install -m 0755 "$HARDEN_SRC" /usr/local/bin/vajra-harden
    echo "   → /usr/local/bin/vajra-harden  ✅"

    # Syntax check
    if bash -n /usr/local/bin/vajra-harden 2>/dev/null; then
        echo "   → syntax OK"
    else
        err "vajra-harden syntax check failed — removing broken binary"
        rm -f /usr/local/bin/vajra-harden
    fi
fi

# Shared security-headers snippet (included by nginx locations)
cat > /etc/nginx/snippets/vajra-security-headers.conf <<'SNIP'
# Vajra hardened — security headers
add_header Strict-Transport-Security "max-age=63072000; includeSubDomains; preload" always;
add_header X-Content-Type-Options    "nosniff" always;
add_header X-Frame-Options           "SAMEORIGIN" always;
add_header Referrer-Policy           "strict-origin-when-cross-origin" always;
add_header Permissions-Policy        "geolocation=(), microphone=(), camera=()" always;
add_header Content-Security-Policy   "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self';" always;
SNIP
echo "   → /etc/nginx/snippets/vajra-security-headers.conf  ✅"

# ─────────────────────────────────────────────────────────────────────


# ===== END OF VAJRA INSTALL =====
