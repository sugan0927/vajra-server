#!/usr/bin/env bash
# certbot deploy hook: reload vajra so new per-site certs take effect.
#
# We call systemctl reload (SIGHUP) rather than POST /reload:
#   - SIGHUP is the documented, non-HTTP path and doesn't need admin
#     port or auth.
#   - reload re-runs SNI resolver + site table but keeps listeners up.
#   - --no-block: don't hold certbot waiting for worker round-trips.
set -euo pipefail

if systemctl is-active --quiet vajra.service; then
    systemctl reload --no-block vajra.service || true
    echo "vajra: reload signalled after cert renewal for ${RENEWED_DOMAINS:-?}"
else
    echo "vajra: service not active, skipping reload" >&2
fi
