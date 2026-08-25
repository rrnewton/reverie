#!/bin/bash
# One demo-5-shaped boot with ALL FIVE STREAMS CAPTURED SEPARATELY, so each can
# be reported on its own rather than collapsed into a single verdict.
#
#   info log  -- hermit's own INFO stream, sent to its own file with --log-file
#   stdout    -- the guest's standard output, nothing else in it
#   stderr    -- the guest's standard error, nothing else in it
#   snapshot  -- the qcow2 artifact as a file
#   state     -- the guest payload inside that qcow2 (extracted afterwards by
#                the prior experiment's scalars.py: RAM, icount, device sections)
#
# Without --log-file, hermit's INFO stream and the guest's stderr land in the
# same fd and cannot be told apart. --log-file is what separates them. It must
# NOT point under /tmp: measured today, a --log-file under /tmp produces no file
# at all, silently, with exit 0.
#
# Every path the guest sees is a CONSTANT across runs (fixed work dir), because
# --detlog-stack hashes argv.
#
# $1 = keep dir for this run's artifacts, rest = extra hermit flags.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
ROOT=/home/newton/work/dev-hermit
HERMIT=${HERMIT_RELEASE:-$ROOT/hermit/target/release/hermit}
ASSETS=$ROOT/ignored/qemu-linux
KEEP=$1; shift
WORK=$HERE/bwork
mkdir -p "$WORK" "$KEEP"
DISK=$WORK/snap.qcow2
INFO=$WORK/info.log
rm -f "$WORK/qmp.sock" "$WORK/serial.log" "$INFO"
qemu-img create -f qcow2 "$DISK" 64M >/dev/null 2>&1

export PYTHONDONTWRITEBYTECODE=1
export RUST_LOG="warn,detcore=info,reverie_ptrace::task=info"
start=$(date +%s.%N)
"$HERMIT" --log info --log-file "$INFO" run --strict \
  --target-timeslice 100000 --max-timeslice 2000000000 \
  "$@" \
  -- \
  python3 $ROOT/demos/lib/qemu_controller.py boot \
    --qemu /usr/bin/qemu-system-x86_64 \
    --qmp-socket "$WORK/qmp.sock" \
    --serial-log "$WORK/serial.log" \
    --disk "$DISK" \
    --kernel "$ASSETS/bzImage" \
    --initrd "$ASSETS/initramfs.cpio.gz" \
    --snapshot-name hermit-boot \
    --timeout 900 \
  >"$KEEP/stdout" 2>"$KEEP/stderr" </dev/null
rc=$?
end=$(date +%s.%N)
cp "$INFO"            "$KEEP/info.log"
cp "$WORK/serial.log" "$KEEP/serial"     2>/dev/null
cp "$DISK"            "$KEEP/snapshot.qcow2"
echo "$rc" > "$KEEP/rc"
echo "$end - $start" | bc > "$KEEP/wall"
printf '%s rc=%s wall=%s info=%s stdout=%s stderr=%s\n' \
  "$(basename "$KEEP")" "$rc" "$(cat "$KEEP/wall")" \
  "$(stat -c%s "$KEEP/info.log")" "$(stat -c%s "$KEEP/stdout")" "$(stat -c%s "$KEEP/stderr")"
