#!/usr/bin/env python3
"""Add Debian/RHEL nginx default server cleanup to install.sh"""
import sys
from pathlib import Path

SH = Path("install.sh")
src = SH.read_text()

if "VAJRA_NGINX_DEFAULT_FIX" in src:
    print("⚠️  already patched")
    sys.exit(0)

FIX = r'''
# ═══════════════════════════════════════════════════════════════
# VAJRA_NGINX_DEFAULT_FIX
# Debian/Ubuntu ship sites-enabled/000-default.conf; RHEL/Rocky
# ship conf.d/default.conf.  Both define a "default_server" on
# :80 which conflicts with vajra-site's nginx config.  Disable
# them BEFORE nginx -t so the test passes.
# ═══════════════════════════════════════════════════════════════
_disable_default_nginx_server() {
  local disabled=0

  # Debian / Ubuntu
  for f in /etc/nginx/sites-enabled/default \
           /etc/nginx/sites-enabled/000-default.conf; do
    if [[ -L "$f" || -f "$f" ]]; then
      mv -f "$f" "${f}.disabled-by-vajra" 2>/dev/null && disabled=1
      echo "   → disabled: $f"
    fi
  done

  # RHEL / Rocky / Alma / Fedora / Arch
  for f in /etc/nginx/conf.d/default.conf \
           /etc/nginx/conf.d/welcome.conf; do
    if [[ -f "$f" ]]; then
      mv -f "$f" "${f}.disabled-by-vajra" 2>/dev/null && disabled=1
      echo "   → disabled: $f"
    fi
  done

  [[ "$disabled" == "1" ]] && echo "   ✅ default server configs disabled"
  return 0
}
# ═══════════════════════════════════════════════════════════════
'''

# Insert helper right after the shebang + set -e block (before any other function)
lines = src.splitlines(keepends=True)
insert_at = 0
for i, line in enumerate(lines[:10]):
    if line.startswith("#!") or line.strip().startswith("set -"):
        insert_at = i + 1

src = "".join(lines[:insert_at]) + FIX + "".join(lines[insert_at:])

# Now find the "6/8 — Nginx base" section and add the call BEFORE nginx -t
import re
anchor = re.search(r'(▶\s*6/8\s*—\s*Nginx base)', src)
if not anchor:
    sys.exit("❌ cannot find '6/8 — Nginx base' section")

# Insert call right after the section header line
eol = src.find("\n", anchor.end())
src = src[:eol+1] + "\n_disable_default_nginx_server\n" + src[eol+1:]

SH.with_suffix(".sh.pre-nginx-fix").write_text(Path("install.sh.bak-pre-nginx-fix").read_text())
SH.write_text(src)
print("✅ install.sh patched with nginx default server cleanup")
