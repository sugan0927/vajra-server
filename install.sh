#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Vajra universal installer  (rev 3 — strict preflight + auto-heal)
#
# Supported:
#   • Debian 11/12/13        • Ubuntu 20.04/22.04/24.04
#   • RHEL/Alma/Rocky 9.x    • Fedora 38+
#   • Arch / Manjaro
#   • (RHEL 8 family: io_uring is NOT available — installer refuses)
#
# Requirements:
#   • Linux kernel 5.1+ with io_uring enabled
#   • systemd
#   • x86_64
#
# Usage:
#   curl -fsSL …/install.sh | bash
#   bash install.sh example.com you@example.com
#
# Env:
#   VAJRA_REPO           GitHub repo (default: sugan0927/vajra-server)
#   VAJRA_VERSION        tag or "latest"
#   VAJRA_SKIP_BINARY    "1" to keep /usr/local/bin/vajra
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

FIRST_DOMAIN="${1:-}"
FIRST_EMAIL="${2:-}"
VAJRA_REPO="${VAJRA_REPO:-sugan0927/vajra-server}"
VAJRA_VERSION="${VAJRA_VERSION:-latest}"
VAJRA_SKIP_BINARY="${VAJRA_SKIP_BINARY:-0}"

log()  { printf "\n\033[1;32m▶ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠  %s\033[0m\n" "$*"; }
die()  { printf "\033[1;31m❌ %s\033[0m\n" "$*" >&2; exit 1; }

# ─── io_uring strict pre-flight ─────────────────────────────────────────
io_uring_syscall_probe() {
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
  if gcc -O0 -o "$tmpd/t" "$tmpd/t.c" 2>/dev/null; then
    local out
    out=$("$tmpd/t" 2>&1 || true)
    rm -rf "$tmpd"
    echo "$out"
    return 0
  fi
  rm -rf "$tmpd"
  echo "SKIP"
}

vajra_preflight_strict() {
  local kmaj kmin
  kmaj=$(uname -r | cut -d. -f1)
  kmin=$(uname -r | cut -d. -f2 | tr -cd '0-9')
  [[ -z "$kmin" ]] && kmin=0

  if (( kmaj < 5 || (kmaj == 5 && kmin < 1) )); then
    echo "   ❌ kernel $(uname -r) < 5.1 — io_uring was introduced in Linux 5.1"
    echo "      RHEL / Alma / Rocky 8 ships kernel 4.18 with NO io_uring backport."
    echo "      Vajra cannot run on this host."
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
    OK)
      echo "   → io_uring_setup() syscall: OK  ✅"
      return 0 ;;
    "FAIL 38")
      echo "   ❌ io_uring_setup() → ENOSYS (syscall not implemented)"
      echo "      Kernel reports 5.1+ but the syscall is missing."
      return 1 ;;
    "FAIL 1")
      echo "   ❌ io_uring_setup() → EPERM (blocked by host)"
      echo "      Hyper-V VPS, Docker (no --privileged), or restrictive LXC."
      return 1 ;;
    FAIL*)
      echo "   ❌ io_uring_setup() failed: $probe"
      return 1 ;;
    SKIP)
      echo "   ⚠  gcc not present — skipping syscall test (kernel gate passed)"
      return 0 ;;
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

# ─── Firewall ───────────────────────────────────────────────────────────
open_ports() {
  local ports=(80 443) opened=0
  if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
    for p in "${ports[@]}"; do ufw allow "${p}/tcp" >/dev/null 2>&1 || true; done
    ufw reload >/dev/null 2>&1 || true; echo "   → ufw: opened ${ports[*]}"; opened=1
  fi
  if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld 2>/dev/null; then
    for p in "${ports[@]}"; do firewall-cmd --permanent --add-port="${p}/tcp" >/dev/null 2>&1 || true; done
    firewall-cmd --reload >/dev/null 2>&1 || true
    echo "   → firewalld: opened ${ports[*]}"; opened=1
  fi
  if command -v nft >/dev/null 2>&1 && nft list ruleset 2>/dev/null | grep -qE 'table (inet|ip) filter'; then
    for p in "${ports[@]}"; do
      nft list chain inet filter input 2>/dev/null | grep -q "dport ${p} accept" \
        || nft add rule inet filter input tcp dport "${p}" accept 2>/dev/null || true
    done
    echo "   → nftables: ensured ${ports[*]}"; opened=1
  fi
  if command -v iptables >/dev/null 2>&1 && [[ "$opened" == 0 ]]; then
    for p in "${ports[@]}"; do
      iptables -C INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null \
        || iptables -I INPUT -p tcp --dport "${p}" -j ACCEPT 2>/dev/null || true
    done
    echo "   → iptables: opened ${ports[*]}"; opened=1
  fi
  [[ "$opened" == 0 ]] && echo "   → no host firewall active"
  echo "   ⚠ cloud firewall (Vultr/Hetzner/DO/AWS) must allow 80, 443"
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
      pkg_install curl ca-certificates
      if [[ "$DISTRO_ID" == "fedora" ]]; then
        dnf install -y "https://rpms.remirepo.net/fedora/remi-release-$(rpm -E %fedora).rpm" 2>/dev/null || true
      else
        pkg_install_soft epel-release dnf-plugins-core
        dnf install -y "https://rpms.remirepo.net/enterprise/remi-release-$(rpm -E %rhel).rpm" 2>/dev/null || true
      fi
      dnf module reset php -y 2>/dev/null || true
      dnf module enable php:remi-8.4 -y 2>/dev/null || true
      PHP_VER="8.4"
      pkg_install php php-cli php-fpm php-mysqlnd php-curl php-xml \
                  php-mbstring php-zip php-gd php-intl php-bcmath
      pkg_install_soft php-imagick
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

# ─── Vajra binary (URL uses plain 'linux', not 'linux-musl') ────────────
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

  # Try multiple asset name variants — GitHub Actions publishes
  # 'vajra-VER-x86_64-linux.tar.gz'; legacy name has '-musl' suffix.
  local _VERSION="$VAJRA_VERSION"
  local _BASE="https://github.com/${VAJRA_REPO}/releases/download/${VAJRA_VERSION}"
  local _CANDIDATES=(
    "vajra-${_VERSION}-x86_64-linux.tar.gz"
    "vajra-${_VERSION}-x86_64-linux-musl.tar.gz"
    "vajra-${_VERSION}-x86_64-unknown-linux-musl.tar.gz"
    "vajra-${_VERSION}-linux-x86_64.tar.gz"
  )
  local TMP; TMP="$(mktemp -d)"
  trap '[[ -n "${TMP:-}" ]] && rm -rf "$TMP"' EXIT

  local _downloaded=0 _used=""
  for _asset in "${_CANDIDATES[@]}"; do
    echo "   → trying: $_asset"
    if curl -fL --retry 2 --retry-delay 1 "$_BASE/$_asset" -o "$TMP/$_asset" 2>/dev/null; then
      if tar tzf "$TMP/$_asset" >/dev/null 2>&1; then
        echo "   ✅ downloaded: $_asset"
        _used="$_asset"; _downloaded=1
        tar -xzf "$TMP/$_asset" -C "$TMP"
        break
      fi
    fi
  done

  if [[ "$_downloaded" == "1" ]]; then
    local SRC
    SRC="$(find "$TMP" -type f -name vajra | head -1)"
    [[ -x "$SRC" ]] || die "tarball missing vajra binary"
    local out; out="$("$SRC" --help 2>&1 || true)"
    if grep -q 'thread-per-core' <<<"$out" && ! grep -q 'GLIBC_' <<<"$out"; then
      install -m 0755 "$SRC" /usr/local/bin/vajra
      echo "   → $(head -1 <<<"$out")"
      return
    fi
    warn "downloaded binary will not run: $(grep -o 'GLIBC_[0-9.]*' <<<"$out" | head -1)"
  fi

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
fix_nginx_default_server() {
  local fixed=0
  for f in /etc/nginx/sites-enabled/default \
           /etc/nginx/sites-enabled/000-default.conf \
           /etc/nginx/conf.d/default.conf \
           /etc/nginx/conf.d/welcome.conf; do
    if [[ -L "$f" || -f "$f" ]]; then
      mv -f "$f" "${f}.disabled-by-vajra" 2>/dev/null && { echo "   → disabled: $f"; fixed=1; }
    fi
  done
  if grep -qE 'listen\s+(\[::\]:)?80\s+default_server' /etc/nginx/nginx.conf 2>/dev/null; then
    cp -f /etc/nginx/nginx.conf /etc/nginx/nginx.conf.vajra-bak 2>/dev/null || true
    sed -i -E 's/(listen\s+(\[::\]:)?80)\s+default_server;/\1;/' /etc/nginx/nginx.conf
    echo "   → neutralised default_server in /etc/nginx/nginx.conf"
    fixed=1
  fi
  [[ "$fixed" == "1" ]] && echo "   ✅ nginx default server conflicts resolved"
  return 0
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
  ensure_service mariadb || errors=$((errors+1))
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

# ═══════════════════════════════════════════════════════════════════════
# MAIN FLOW
# ═══════════════════════════════════════════════════════════════════════

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
║                                                                       ║
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
║  Debug:                                                               ║
║    uname -r                                                           ║
║    cat /proc/sys/kernel/io_uring_disabled 2>/dev/null                 ║
║    systemd-detect-virt                                                ║
║                                                                       ║
╚═══════════════════════════════════════════════════════════════════════╝

GUARDEOF
  exit 1
fi

log "1/8 — Base packages"
pkg_update
case "$DISTRO_FAMILY" in
  debian) pkg_install curl wget unzip ca-certificates gnupg lsb-release \
                       nginx mariadb-server certbot openssl python3 \
                       apt-transport-https software-properties-common ;;
  rhel)   pkg_install curl wget unzip ca-certificates gnupg2 \
                       nginx mariadb-server certbot openssl python3 ;;
  arch)   pkg_install curl wget unzip ca-certificates gnupg \
                       nginx mariadb certbot openssl python ;;
esac

# RHEL family does NOT auto-start mariadb on package install.
case "$DISTRO_FAMILY" in
  rhel) systemctl enable --now mariadb >/dev/null 2>&1 || true ;;
esac

log "2/8 — PHP"
setup_php
update-alternatives --set php "/usr/bin/php${PHP_VER}" 2>/dev/null || true
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

cat > /etc/systemd/system/vajra-site@.service <<UNIT
[Unit]
Description=Vajra web server for %i
After=network.target ${PHP_FPM_SVC}.service
Requires=${PHP_FPM_SVC}.service

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

log "6/8 — Nginx base"
fix_nginx_default_server
rm -f "$NGINX_SITES_ENABLED/default" 2>/dev/null || true
cat > "${NGINX_SITES_AVAIL}/000-default.conf" <<'NGINXBASE'
server {
    listen 80;
    listen [::]:80;
    server_name _;
    location /.well-known/acme-challenge/ { root /var/www/certbot; }
    location / { return 444; }
}
NGINXBASE
[[ "$DISTRO_FAMILY" == "debian" ]] && \
  ln -sf "${NGINX_SITES_AVAIL}/000-default.conf" "${NGINX_SITES_ENABLED}/000-default.conf"

if nginx -t 2>/dev/null; then
  systemctl enable --now nginx
  systemctl restart nginx
  echo "   ✅ nginx base up"
else
  warn "nginx config test failed — attempting auto-heal"
  nginx -t 2>&1 | sed 's/^/      /'
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
# vajra-site — manage multiple WordPress sites on this Vajra install
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

usage() {
cat <<U
vajra-site — manage WordPress sites (each on its own Vajra + Nginx + cert)

USAGE:
  vajra-site add <domain> <email> [--title "Site Title"] [--admin-user NAME] [--admin-pass PASS]
  vajra-site list
  vajra-site info <domain>
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

# Fail-fast if the underlying stack is not running.
ensure_stack() {
  local svc
  for svc in nginx "${PHP_FPM_SVC}" mariadb; do
    [[ -z "$svc" ]] && continue
    if ! systemctl is-active --quiet "$svc"; then
      warn "$svc not running — attempting start"
      systemctl enable --now "$svc" >/dev/null 2>&1 || true
      sleep 2
      if ! systemctl is-active --quiet "$svc"; then
        err "$svc failed to start"
        journalctl -u "$svc" -n 20 --no-pager | sed 's/^/    /' || true
        die "Cannot proceed — fix $svc first"
      fi
      info "$svc started"
    fi
  done
  if [[ ! -S "$PHP_SOCK" ]]; then
    warn "PHP socket $PHP_SOCK missing — restarting PHP-FPM"
    systemctl restart "$PHP_FPM_SVC" >/dev/null 2>&1 || true
    sleep 2
    [[ -S "$PHP_SOCK" ]] || die "PHP-FPM socket still missing: $PHP_SOCK"
  fi
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

cmd_add() {
  [[ $# -ge 2 ]] || { err "Usage: vajra-site add <domain> <email> [opts]"; exit 1; }
  local DOMAIN="$1" EMAIL="$2"; shift 2
  local TITLE="WordPress" ADMIN_USER="wpadmin" ADMIN_PASS=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --title) TITLE="$2"; shift 2;;
      --admin-user) ADMIN_USER="$2"; shift 2;;
      --admin-pass) ADMIN_PASS="$2"; shift 2;;
      *) die "Unknown option: $1";;
    esac
  done
  valid_domain "$DOMAIN" || die "Invalid domain: $DOMAIN"
  valid_email "$EMAIL"   || die "Invalid email: $EMAIL"
  [[ -f "${SITES_DIR}/${DOMAIN}.env" ]] && die "Site '$DOMAIN' already exists"

  ensure_stack

  local WWW_ALIAS="www.${DOMAIN}"
  local DB_NAME="wp_$(sanitize "$DOMAIN")"
  local DB_USER="u_$(sanitize "$DOMAIN")"; DB_USER="${DB_USER:0:20}"
  local DB_PASS; DB_PASS="$(openssl rand -base64 24 | tr -d '/+=\n' | cut -c1-24)"
  [[ -n "$ADMIN_PASS" ]] || ADMIN_PASS="$(openssl rand -base64 18 | tr -d '/+=\n' | cut -c1-16)"
  local PORT; PORT="$(next_port)"
  local SITE_ROOT="${WWW_ROOT}/${DOMAIN}"

  log "1/8 — Allocating resources"
  echo "   → domain: $DOMAIN (+ $WWW_ALIAS)"
  echo "   → port:   $PORT"
  echo "   → db:     $DB_NAME / $DB_USER"

  log "2/8 — WordPress files"
  mkdir -p "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}/.well-known/acme-challenge"
  if [[ ! -f "$SITE_ROOT/wp-load.php" ]]; then
    local TMP; TMP="$(mktemp -d)"
    curl -fL https://wordpress.org/latest.tar.gz | tar -xz -C "$TMP"
    cp -a "$TMP/wordpress/." "$SITE_ROOT/"
    rm -rf "$TMP"
  fi
  chown -R "$WWW_USER:$WWW_USER" "$SITE_ROOT" "${CERTBOT_ROOT}/${DOMAIN}"

  log "3/8 — MariaDB database + user"
  mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
  mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  mysql -e "ALTER USER '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PASS}';"
  mysql -e "GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"

  log "4/8 — wp-config.php + WordPress install (WP-CLI)"
  wp --path="$SITE_ROOT" --allow-root config create \
      --dbname="$DB_NAME" --dbuser="$DB_USER" --dbpass="$DB_PASS" \
      --dbhost=localhost --dbcharset=utf8mb4 --skip-check --force
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

  wp --path="$SITE_ROOT" --allow-root core install \
      --url="https://${DOMAIN}" --title="$TITLE" \
      --admin_user="$ADMIN_USER" --admin_password="$ADMIN_PASS" \
      --admin_email="$EMAIL" --skip-email
  wp --path="$SITE_ROOT" --allow-root option update blogdescription "" 2>/dev/null || true
  wp --path="$SITE_ROOT" --allow-root rewrite structure '/%postname%/' --hard 2>/dev/null || true

  log "5/8 — Vajra config + service"
  cat > "${SITES_DIR}/${DOMAIN}.toml" <<TOML
[server]
listen  = "127.0.0.1:${PORT}"
workers = 1
pin     = false

[static]
root  = "${SITE_ROOT}"
index = "index.html"

[php]
upstream         = "unix:${PHP_SOCK}"
index            = ["index.php"]
front_controller = "/index.php"
timeout_secs     = 60

[limits]
max_body_bytes = 67108864

[logging]
access_log = "/var/log/vajra/${DOMAIN}.access.log"

[admin]
listen = "127.0.0.1:$((PORT + 1000))"
TOML

  cat > "${SITES_DIR}/${DOMAIN}.env" <<ENV
DOMAIN=${DOMAIN}
WWW_ALIAS=${WWW_ALIAS}
EMAIL=${EMAIL}
PORT=${PORT}
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
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
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
  echo "✅ Site ready:  https://${DOMAIN}/"
  echo "   Admin:       https://${DOMAIN}/wp-admin/"
  echo "   Admin user:  ${ADMIN_USER}"
  echo "   Admin pass:  ${ADMIN_PASS}"
  echo "   DB name:     ${DB_NAME}"
  echo "   DB pass:     ${DB_PASS}"
  echo "   Vajra port:  ${PORT}"
  echo "════════════════════════════════════════════════════════════════"
}

cmd_list() {
  local envs; envs=( "${SITES_DIR}"/*.env )
  if [[ ! -e "${envs[0]}" ]]; then info "No sites yet. Add one: vajra-site add example.com you@example.com"; return; fi
  printf '\n%-30s %-6s %-22s %-6s %s\n' "DOMAIN" "PORT" "DB" "SVC" "CREATED"
  for f in "${envs[@]}"; do
    local d p db svc c
    d="$(read_env "$f" DOMAIN)"; p="$(read_env "$f" PORT)"
    db="$(read_env "$f" DB_NAME)"; c="$(read_env "$f" CREATED_AT)"
    systemctl is-active --quiet "vajra-site@${d}.service" && svc="✅" || svc="❌"
    printf '%-30s %-6s %-22s %-6s %s\n' "$d" "$p" "$db" "$svc" "$c"
  done
  echo
}

cmd_info() {
  [[ $# -ge 1 ]] || die "Usage: vajra-site info <domain>"
  local D="$1"; require_site "$D"; local f="${SITES_DIR}/${D}.env"
  echo "Domain:       $(read_env "$f" DOMAIN)"
  echo "WWW alias:    $(read_env "$f" WWW_ALIAS)"
  echo "Vajra port:   $(read_env "$f" PORT)"
  echo "Site root:    $(read_env "$f" SITE_ROOT)"
  echo "DB name:      $(read_env "$f" DB_NAME)"
  echo "DB user:      $(read_env "$f" DB_USER)"
  echo "DB pass:      $(read_env "$f" DB_PASS)"
  echo "Admin user:   $(read_env "$f" ADMIN_USER)"
  echo "Admin pass:   $(read_env "$f" ADMIN_PASS)"
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
  local DB_NAME DB_USER SITE_ROOT
  DB_NAME="$(read_env "$f" DB_NAME)"; DB_USER="$(read_env "$f" DB_USER)"
  SITE_ROOT="$(read_env "$f" SITE_ROOT)"

  warn "Removing site: $D"
  read -r -p "Continue? [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || { info "Aborted"; return; }

  systemctl disable --now "vajra-site@${D}.service" 2>/dev/null || true
  systemctl daemon-reload
  nginx_remove_site "$D"
  nginx -t && systemctl reload nginx
  certbot delete --cert-name "$D" --non-interactive 2>/dev/null || true
  rm -f "${SITES_DIR}/${D}.toml"
  [[ "$KEEP_DB" == 0 ]] && {
    mysql -e "DROP DATABASE IF EXISTS \`${DB_NAME}\`;"
    mysql -e "DROP USER IF EXISTS '${DB_USER}'@'localhost'; FLUSH PRIVILEGES;"
  }
  [[ "$KEEP_FILES" == 0 ]] && rm -rf "$SITE_ROOT" "${CERTBOT_ROOT}/${D}"
  rm -f "$f"
  echo "✅ Site '$D' removed"
}

case "${1:-}" in
  add) shift; cmd_add "$@";;
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
echo "   Manage sites with:"
echo "     vajra-site add <domain> <email>"
echo "     vajra-site list"
echo "     vajra-site info <domain>"
echo "     vajra-site remove <domain>"
echo

if [[ -n "$FIRST_DOMAIN" && -n "$FIRST_EMAIL" ]]; then
  log "Creating first site: $FIRST_DOMAIN"
  /usr/local/bin/vajra-site add "$FIRST_DOMAIN" "$FIRST_EMAIL"
fi