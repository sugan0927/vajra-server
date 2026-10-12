#!/usr/bin/env bash
# Vajra installer.
#
# Usage:
#   curl -sSL https://vajra.example/install.sh | sudo bash
#   sudo ./install.sh [--wp-path=/var/www/site]
#
# What it does:
#   1. Installs system dependencies (io_uring tools, PHP-FPM, WP-CLI).
#   2. Drops `vajra` into /usr/local/bin.
#   3. Writes a default /etc/vajra/config.toml (does not overwrite).
#   4. Exposes `vajra-wp`, a one-line wrapper around `vajra wp` so
#      common WordPress flows are one word shorter:
#          vajra-wp plugin install woocommerce --activate
#          vajra-wp core update
#          vajra-wp db export /var/backups/site.sql
#      These are exactly `vajra wp ...` with the same passthrough
#      semantics — root -> `sudo -u www-data`, `www-data` -> direct,
#      `VAJRA_WP_PATH` -> injected `--path=` if not given explicitly.

set -euo pipefail

# ─── Defaults ────────────────────────────────────────────────────────
VAJRA_BIN_SRC="${VAJRA_BIN_SRC:-}"           # path to the built binary
VAJRA_WP_PATH="${VAJRA_WP_PATH:-/var/www/html}"
WP_USER="${WP_USER:-www-data}"
INSTALL_BIN="/usr/local/bin/vajra"
CONFIG_DIR="/etc/vajra"
CONFIG_FILE="${CONFIG_DIR}/config.toml"

# ─── Parse args ──────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --wp-path=*) VAJRA_WP_PATH="${1#--wp-path=}" ;;
        --bin=*)     VAJRA_BIN_SRC="${1#--bin=}" ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$(id -u)" -ne 0 ]; then
    echo "install.sh: must run as root" >&2
    exit 1
fi

# ─── Dependencies ────────────────────────────────────────────────────
export DEBIAN_FRONTEND=noninteractive
if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg sudo php-fpm
    if ! command -v wp >/dev/null 2>&1; then
        curl -sSL -o /usr/local/bin/wp \
            https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar
        chmod +x /usr/local/bin/wp
    fi
else
    echo "install.sh: no apt-get detected; install php-fpm and wp-cli manually" >&2
fi

# ─── Binary ──────────────────────────────────────────────────────────
if [ -n "$VAJRA_BIN_SRC" ]; then
    install -m 0755 "$VAJRA_BIN_SRC" "$INSTALL_BIN"
elif [ -x "./target/release/vajra" ]; then
    install -m 0755 ./target/release/vajra "$INSTALL_BIN"
elif [ -x "$(command -v vajra 2>/dev/null)" ]; then
    echo "using existing vajra at $(command -v vajra)"
else
    echo "install.sh: no vajra binary found (set --bin= or build with 'cargo build --release')" >&2
    exit 1
fi

# ─── Config ──────────────────────────────────────────────────────────
install -d -m 0755 "$CONFIG_DIR"
if [ ! -f "$CONFIG_FILE" ]; then
    cat > "$CONFIG_FILE" <<'TOML'
# Vajra default configuration. See docs/config.md.
listen   = "0.0.0.0:80"
workers  = 0        # 0 = one per CPU
pin      = true
admin    = "127.0.0.1:9000"

[static_files]
root = "/var/www/html"
TOML
    echo "wrote $CONFIG_FILE"
else
    echo "keeping existing $CONFIG_FILE"
fi

# ─── `vajra-wp` wrapper ──────────────────────────────────────────────
# A tiny shim so that operators can type `vajra-wp ...` instead of
# `vajra wp ...`. Same semantics, same passthrough.
cat > /usr/local/bin/vajra-wp <<'SH'
#!/bin/sh
exec /usr/local/bin/vajra wp "$@"
SH
chmod 0755 /usr/local/bin/vajra-wp

# ─── Smoke test ──────────────────────────────────────────────────────
echo
echo "installed:"
printf '  %-22s %s\n' "vajra"    "$(command -v vajra)"
printf '  %-22s %s\n' "vajra-wp" "$(command -v vajra-wp)"
printf '  %-22s %s\n' "wp"       "$(command -v wp || echo 'NOT FOUND')"
echo
echo "try:"
echo "  sudo vajra wp --path=${VAJRA_WP_PATH} core version"
echo "  sudo vajra wp --path=${VAJRA_WP_PATH} plugin install woocommerce --activate"
echo "  sudo vajra wp --path=${VAJRA_WP_PATH} db export /var/backups/site.sql"
