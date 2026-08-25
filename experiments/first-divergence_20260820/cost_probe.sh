#!/bin/bash
# Cost of each memory/register hash flag, measured as guest syscalls completed
# in a FIXED wall-clock window. Not a determinism measurement -- a budget
# measurement, so the real instrument can be chosen instead of guessed.
#
# $1 = run dir (created), $2 = wall seconds, rest = extra hermit flags.
# hermit run ignores SIGTERM (the guest is a namespace init), so the cap is
# enforced with SIGKILL on the whole match.
set -u
OUT=$1; shift
CAP=$1; shift
mkdir -p "$OUT"
( sleep "$CAP"; pkill -KILL -f "$OUT" >/dev/null 2>&1 ) &
KILLER=$!
start=$(date +%s.%N)
/home/newton/work/dev-hermit/experiments/first-divergence_20260820/boot_one.sh "$OUT" "$@" >/dev/null 2>&1
end=$(date +%s.%N)
kill "$KILLER" 2>/dev/null
wait "$KILLER" 2>/dev/null
syscalls=$(grep -c 'finish syscall' "$OUT/ctrl.err" 2>/dev/null || echo 0)
commits=$(grep -c ' COMMIT turn ' "$OUT/ctrl.err" 2>/dev/null || echo 0)
bytes=$(stat -c%s "$OUT/ctrl.err" 2>/dev/null || echo 0)
wall=$(echo "$end - $start" | bc)
printf '%s\twall=%s\tsyscalls=%s\tcommits=%s\tlogbytes=%s\tflags=%s\n' \
  "$(basename "$OUT")" "$wall" "$syscalls" "$commits" "$bytes" "$*"
