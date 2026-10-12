#!/usr/bin/env bash
# vajra-preflight.sh — run before install to check io_uring

echo "=== System Info ==="
uname -r
grep -E "^(NAME|VERSION)=" /etc/os-release
systemd-detect-virt 2>/dev/null || echo "virt: unknown"

echo ""
echo "=== io_uring Status ==="
if [[ -r /proc/sys/kernel/io_uring_disabled ]]; then
  V=$(cat /proc/sys/kernel/io_uring_disabled)
  echo "kernel.io_uring_disabled = $V"
  case "$V" in
    0) echo "✅ enabled for all processes" ;;
    1) echo "⚠️  privileged-only (root gets it)" ;;
    2) echo "❌ disabled for all — Vajra will fail" ;;
  esac
else
  echo "no /proc/sys/kernel/io_uring_disabled (kernel < 6.x)"
  echo "✅ (assume enabled)"
fi

echo ""
echo "=== Real syscall test ==="
if command -v gcc >/dev/null 2>&1; then
  TMP=$(mktemp /tmp/iotest.XXXX.c)
  cat > "$TMP" <<'CCEOF'
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>
int main(void){
    struct io_uring_params p; memset(&p,0,sizeof(p));
    long fd = syscall(__NR_io_uring_setup, 8, &p);
    if (fd < 0) { fprintf(stderr,"failed: %s (errno=%d)\n", strerror(errno), errno); return 1; }
    close((int)fd); return 0;
}
CCEOF
  if gcc -O0 -o "${TMP%.c}" "$TMP" 2>/dev/null; then
    if "${TMP%.c}"; then
      echo "✅ io_uring_setup() syscall works"
      RESULT=ok
    else
      echo "❌ io_uring_setup() syscall FAILED"
      RESULT=fail
    fi
  else
    echo "⚠️  compile failed — skipping syscall test"
    RESULT=unknown
  fi
  rm -f "$TMP" "${TMP%.c}"
else
  echo "(gcc not installed — cannot test syscall)"
  RESULT=unknown
fi

echo ""
echo "=== Conclusion ==="
case "${RESULT:-unknown}" in
  ok) echo "✅ Vajra should work on this host" ;;
  fail) echo "❌ Vajra will FAIL — pick different plan or contact provider" ;;
  *) echo "⚠️  Cannot determine — run install and check for io_uring errors" ;;
esac
