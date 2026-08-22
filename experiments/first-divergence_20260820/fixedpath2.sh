#!/bin/bash
# N boots into ONE fixed pathname, run SEQUENTIALLY, with the INFO log copied
# out after each run.
#
# WHY A FIXED PATH IS MANDATORY HERE, and it is a stronger requirement than the
# prior experiment needed. That work showed the run-dir pathname LENGTH is a
# guest-visible input (one character = 10 ns of logical time), so it used
# fixed-WIDTH names. Fixed width is not enough once the stack is hashed: the
# run-dir path is passed in argv to the controller, argv lives on the initial
# process stack, and --detlog-stack hashes that stack. Two same-length names
# that differ in one CHARACTER therefore give different stack hashes from the
# first sample onward -- an input difference, not nondeterminism. Only a
# byte-identical pathname isolates the run-to-run question.
#
# $1 = tag (names the keep dir), $2 = count, rest = extra hermit flags.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
TAG=$1; shift
N=$1; shift
export RUST_LOG="warn,detcore=info,reverie_ptrace::task=info"
KEEP=$HERE/keep/$TAG
mkdir -p "$KEEP"
for i in $(seq -w 1 "$N"); do
  rm -rf "$HERE/runs/f2"
  "$HERE/boot_one.sh" "$HERE/runs/f2" "$@" >/dev/null 2>&1
  cp "$HERE/runs/f2/ctrl.err"   "$KEEP/r$i.log"
  cp "$HERE/runs/f2/serial.log" "$KEEP/r$i.serial" 2>/dev/null
  cp "$HERE/runs/f2/snap.qcow2" "$KEEP/r$i.qcow2"  2>/dev/null
  cp "$HERE/runs/f2/rc"         "$KEEP/r$i.rc"     2>/dev/null
  echo "$TAG run $i done $(date +%T) log=$(stat -c%s "$KEEP/r$i.log")"
done
echo "FIXEDPATH $TAG DONE flags=$*"
