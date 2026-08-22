#!/bin/bash
# ONE full demo-5 boot with --detlog-heap, run to COMPLETION, so the number is a
# measurement and not an extrapolation from an aborted run. Hard wall cap so it
# cannot run away; if the cap fires the partial progress is still reported and
# labelled as partial.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
OUT=$HERE/runs/hf
CAP=${CAP:-9000}
rm -rf "$OUT"; mkdir -p "$OUT"
( sleep "$CAP"; pkill -KILL -f "runs/hf/snap.qcow2" >/dev/null 2>&1 ) &
K=$!
s=$(date +%s.%N)
"$HERE/boot_one.sh" "$OUT" --detlog-heap >/dev/null 2>&1
e=$(date +%s.%N)
kill "$K" 2>/dev/null; wait "$K" 2>/dev/null
echo "FULL --detlog-heap boot: wall=$(echo "$e - $s"|bc) rc=$(cat "$OUT/rc" 2>/dev/null) log=$(stat -c%s "$OUT/ctrl.err" 2>/dev/null) serial=$(stat -c%s "$OUT/serial.log" 2>/dev/null) syscalls=$(grep -c 'finish syscall' "$OUT/ctrl.err" 2>/dev/null)"
