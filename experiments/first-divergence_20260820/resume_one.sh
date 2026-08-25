#!/bin/bash
# One demo-6-shaped hermit QEMU snapshot-resume, faithful to demos/06-qemu-resume.py:
# same hermit flags (--strict --no-rcb-time --target-timeslice 100000
# --max-timeslice disabled -- NOT demo 5's flags), the same
# demos/lib/qemu_controller.py resume controller, and the same QEMU command that
# controller builds (-serial pipe: FIFO pair, -loadvm).
#
# Every path handed to the guest is a CONSTANT, exactly as demo 6 does it: the
# demo points qmp.sock / serial.log / serial-pipe / the working disk at fixed
# names under its assets dir. That matters here because --detlog-stack hashes
# argv, so a per-run path would be a per-run input. Results are copied out
# afterwards, which the guest never sees.
#
# $1 = keep dir for this run's artifacts, $2 = guest command, rest = hermit flags.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
ROOT=/home/newton/work/dev-hermit
HERMIT=${HERMIT_RELEASE:-$ROOT/hermit/target/release/hermit}
ASSETS=$ROOT/ignored/qemu-linux
KEEP=$1; shift
GUESTCMD=$1; shift

# Constant working area, identical for every run in the sweep.
WORK=$HERE/rwork
mkdir -p "$WORK" "$KEEP"
DISK=$WORK/disk.qcow2
QMP=$WORK/qmp.sock
SERLOG=$WORK/serial.log
PIPE=$WORK/serial-pipe
rm -f "$QMP" "$SERLOG" "$PIPE.in" "$PIPE.out"
cp "$ASSETS/hermit-boot.qcow2" "$DISK"

export PYTHONDONTWRITEBYTECODE=1
export RUST_LOG=${RUST_LOG:-warn,detcore=info,reverie_ptrace::task=info}
start=$(date +%s.%N)
"$HERMIT" run --strict --no-rcb-time \
  --target-timeslice 100000 --max-timeslice disabled \
  "$@" \
  -- \
  python3 $ROOT/demos/lib/qemu_controller.py resume \
    --qemu /usr/bin/qemu-system-x86_64 \
    --qmp-socket "$QMP" \
    --serial-pipe "$PIPE" \
    --serial-log "$SERLOG" \
    --disk "$DISK" \
    --kernel "$ASSETS/bzImage" \
    --initrd "$ASSETS/initramfs.cpio.gz" \
    --snapshot-name hermit-boot \
    --timeout 900 \
    --guest-command "$GUESTCMD" \
    --post-snapshot-name post-cmd \
  >"$KEEP/ctrl.out" 2>"$KEEP/ctrl.err"
rc=$?
end=$(date +%s.%N)
cp "$SERLOG" "$KEEP/serial.log" 2>/dev/null
cp "$DISK"   "$KEEP/post.qcow2" 2>/dev/null
echo "$rc" > "$KEEP/rc"
echo "$end - $start" | bc > "$KEEP/wall"
echo "$(basename "$KEEP") rc=$rc wall=$(cat "$KEEP/wall") log=$(stat -c%s "$KEEP/ctrl.err")"
