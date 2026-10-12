#!/usr/bin/env python3
"""Add io_uring pre-flight check to install.sh"""
import sys
from pathlib import Path

SH = Path("install.sh")
if not SH.exists():
    sys.exit("install.sh not found in current dir")

src = SH.read_text()

if "ensure_io_uring" in src:
    print("⚠️  already patched — skipping")
    sys.exit(0)

HELPERS = r'''
# ─────────────────────────────────────────────────────────────
# io_uring availability check + auto-enable  (vajra patch)
# ─────────────────────────────────────────────────────────────
ensure_io_uring() {
  local current
  if [[ -r /proc/sys/kernel/io_uring_disabled ]]; then
    current=$(cat /proc/sys/kernel/io_uring_disabled)
  else
    current="0"
  fi
  echo "   → io_uring_disabled = ${current}"

  if [[ "$current" == "0" ]]; then
    return 0
  fi

  echo "   ⚠  io_uring disabled — attempting to enable"
  if sysctl -w kernel.io_uring_disabled=0 >/dev/null 2>&1; then
    cat > /etc/sysctl.d/99-enable-io-uring.conf <<'SYSCTL'
# Enabled by vajra-server installer for io_uring support
kernel.io_uring_disabled = 0
SYSCTL
    sysctl --system >/dev/null 2>&1 || true
    current=$(cat /proc/sys/kernel/io_uring_disabled 2>/dev/null || echo "0")
    if [[ "$current" == "0" ]]; then
      echo "   ✅ io_uring enabled (persistent)"
    else
      echo "   ❌ sysctl write succeeded but value is still ${current}"
      return 1
    fi
  else
    echo "   ❌ cannot write kernel.io_uring_disabled (read-only / host-locked)"
    return 1
  fi

  if ! io_uring_real_test; then
    echo "   ❌ kernel reports enabled, but io_uring_setup() syscall is blocked"
    echo "      (likely seccomp / container / hypervisor-level block)"
    return 1
  fi

  return 0
}

io_uring_real_test() {
  local tmp
  tmp=$(mktemp /tmp/io_uring_test.XXXXXX.c)
  cat > "$tmp" <<'CCEOF'
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>
int main(void){
    struct io_uring_params p;
    memset(&p, 0, sizeof(p));
    long fd = syscall(__NR_io_uring_setup, 8, &p);
    if (fd < 0) {
        fprintf(stderr, "io_uring_setup failed: %s (errno=%d)\n",
                strerror(errno), errno);
        return 1;
    }
    close((int)fd);
    return 0;
}
CCEOF

  if ! command -v gcc >/dev/null 2>&1; then
    rm -f "$tmp"
    return 0
  fi

  if gcc -O0 -o "${tmp%.c}" "$tmp" 2>/dev/null && "${tmp%.c}" >/dev/null 2>&1; then
    rm -f "$tmp" "${tmp%.c}"
    return 0
  fi
  rm -f "$tmp" "${tmp%.c}"
  return 1
}

'''

FAIL_BLOCK = r'''
# ─── io_uring fail-fast guard ───────────────────────────────
if ! ensure_io_uring; then
  cat >&2 <<'MSGEOF'

╔══════════════════════════════════════════════════════════════╗
║  ❌ io_uring is not available on this host                  ║
╠══════════════════════════════════════════════════════════════╣
║                                                              ║
║  Vajra एक io_uring-based web server है और इसके बिना        ║
║  start नहीं हो सकता।                                         ║
║                                                              ║
║  संभावित कारण:                                              ║
║    • VPS host (Hyper-V / कुछ KVM plans) ने io_uring block   ║
║      किया है                                                ║
║    • Docker / LXC container में seccomp filter है           ║
║    • Kernel boot parameter या host-level policy              ║
║                                                              ║
║  आपके विकल्प:                                               ║
║    1. KVM-based VPS लें (Hetzner / DO / Linode)             ║
║    2. Provider से कहें:                                     ║
║       "set kernel.io_uring_disabled=0"                      ║
║    3. Nginx + PHP-FPM manually install करें (Vajra skip)    ║
║                                                              ║
║  Debug:                                                      ║
║    cat /proc/sys/kernel/io_uring_disabled                   ║
║    systemd-detect-virt                                      ║
║    uname -r                                                 ║
║                                                              ║
╚══════════════════════════════════════════════════════════════╝

MSGEOF
  exit 1
fi
# ────────────────────────────────────────────────────────────
'''

# 1) Insert helpers after shebang + set -e lines
lines = src.splitlines(keepends=True)
insert_at = 0
for i, line in enumerate(lines[:8]):
    if line.startswith("#!") or line.strip().startswith("set -"):
        insert_at = i + 1

new_src = "".join(lines[:insert_at]) + HELPERS + "".join(lines[insert_at:])

# 2) Insert fail-block right after "port 80 free"
anchor = "port 80 free"
idx = new_src.find(anchor)
if idx == -1:
    sys.exit("❌ anchor 'port 80 free' not found — patch manually")

eol = new_src.find("\n", idx)
new_src = new_src[:eol+1] + "\n" + FAIL_BLOCK + "\n" + new_src[eol+1:]

# 3) Backup + write
SH.with_suffix(".sh.bak").write_text(src)
SH.write_text(new_src)
print("✅ install.sh patched (backup: install.sh.bak)")
