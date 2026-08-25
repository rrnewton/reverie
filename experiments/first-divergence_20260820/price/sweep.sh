#!/bin/bash
# Fit the two coefficients of guest-memory hashing at a syscall boundary:
#   T(H) - T_noflag = N * (c_call + H * c_byte)
# Syscall count N is FIXED; the hashed region size H is swept. Uses the existing
# --detlog-heap flag, so no Hermit change is needed to get the coefficients.
#
# This is a TIMING measurement, so runs are SERIAL and the load is recorded with
# each point. Three repeats per point; the median is used.
# Logs are discarded (>/dev/null) rather than kept -- nothing here needs the log
# contents, and an unbounded log is what filled this box earlier today.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820/price
H=/home/newton/work/dev-hermit/hermit/target/release/hermit
N=${N:-2000}
REPS=${REPS:-3}
printf '%-10s %-8s %-4s %10s %8s\n' arm heap_kib rep wall_s load1
for kib in 0 256 1024 2048 4096; do
  for rep in $(seq 1 "$REPS"); do
    start=$(date +%s.%N)
    RUST_LOG="warn,detcore=info" "$H" run --strict --detlog-heap \
      -- "$HERE/hashcost" "$kib" "$N" >/dev/null 2>/dev/null </dev/null
    end=$(date +%s.%N)
    printf '%-10s %-8s %-4s %10.3f %8s\n' heapflag "$kib" "$rep" \
      "$(echo "$end - $start" | bc)" "$(cut -d' ' -f1 /proc/loadavg)"
  done
done
# Same guest, same syscall count, flag OFF: the subtraction baseline.
for kib in 0 4096; do
  for rep in $(seq 1 "$REPS"); do
    start=$(date +%s.%N)
    RUST_LOG="warn,detcore=info" "$H" run --strict \
      -- "$HERE/hashcost" "$kib" "$N" >/dev/null 2>/dev/null </dev/null
    end=$(date +%s.%N)
    printf '%-10s %-8s %-4s %10.3f %8s\n' noflag "$kib" "$rep" \
      "$(echo "$end - $start" | bc)" "$(cut -d' ' -f1 /proc/loadavg)"
  done
done
