#!/bin/bash
# One demo-5-shaped hermit QEMU boot -> snapshot, into $1 (created).
# Faithful to demos/05-qemu-boot.py: same hermit flags, same controller, same
# QEMU command shape (built by demos/lib/qemu_controller.py itself).
set -u
OUT=$1; shift
ROOT=/home/newton/work/dev-hermit
HERMIT=${HERMIT_RELEASE:-$ROOT/hermit/target/release/hermit}
ASSETS=$ROOT/ignored/qemu-linux
mkdir -p "$OUT"
DISK=$OUT/snap.qcow2
qemu-img create -f qcow2 "$DISK" 64M >/dev/null 2>&1
export PYTHONDONTWRITEBYTECODE=1
export RUST_LOG=${RUST_LOG:-}
start=$(date +%s.%N)
"$HERMIT" run --strict \
  --target-timeslice 100000 --max-timeslice 2000000000 \
  "$@" \
  -- \
  python3 $ROOT/demos/lib/qemu_controller.py boot \
    --qemu /usr/bin/qemu-system-x86_64 \
    --qmp-socket "$OUT/qmp.sock" \
    --serial-log "$OUT/serial.log" \
    --disk "$DISK" \
    --kernel "$ASSETS/bzImage" \
    --initrd "$ASSETS/initramfs.cpio.gz" \
    --snapshot-name hermit-boot \
    --timeout 600 \
  >"$OUT/ctrl.out" 2>"$OUT/ctrl.err"
rc=$?
end=$(date +%s.%N)
echo "$rc" > "$OUT/rc"
echo "$end - $start" | bc > "$OUT/wall"
echo "$OUT rc=$rc wall=$(cat $OUT/wall)"
