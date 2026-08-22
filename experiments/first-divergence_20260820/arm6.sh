#!/bin/bash
# N demo-6-shaped resume runs, sequential (they share one constant working dir,
# which is the point: every path the guest sees is identical across runs).
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
TAG=$1; shift
N=$1; shift
CMD=$1; shift
mkdir -p "$HERE/keep/$TAG"
for i in $(seq -w 1 "$N"); do
  rm -rf "$HERE/keep/$TAG/w"
  "$HERE/resume_one.sh" "$HERE/keep/$TAG/w" "$CMD" "$@"
  cp "$HERE/keep/$TAG/w/ctrl.err"   "$HERE/keep/$TAG/r$i.log"
  cp "$HERE/keep/$TAG/w/serial.log" "$HERE/keep/$TAG/r$i.serial" 2>/dev/null
  cp "$HERE/keep/$TAG/w/post.qcow2" "$HERE/keep/$TAG/r$i.qcow2"  2>/dev/null
  cp "$HERE/keep/$TAG/w/rc"         "$HERE/keep/$TAG/r$i.rc"     2>/dev/null
  cp "$HERE/keep/$TAG/w/ctrl.out"   "$HERE/keep/$TAG/r$i.out"    2>/dev/null
done
echo "ARM6 $TAG DONE flags=$*"
