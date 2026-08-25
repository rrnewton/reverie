#!/bin/bash
# Before/after for the detlog_memory_maps logging-off gate.
# SERIAL and identical protocol for both binaries: this is a TIMING measurement,
# so the two arms must differ only in the binary under test.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
printf '%-8s %-16s %10s %10s %s\n' binary flag wall_s logbytes load1
for bin in before after; do
  for flag in "" "--detlog-regs" "--detlog-stack" "--detlog-heap"; do
    out=$HERE/runs/g_${bin}_$(echo ${flag:-none}|tr -d '-')
    rm -rf "$out"
    ( unset RUST_LOG; HERMIT_RELEASE=$HERE/bin2/hermit-$bin \
        "$HERE/boot_one.sh" "$out" $flag >/dev/null 2>&1 )
    printf '%-8s %-16s %10s %10s %s\n' "$bin" "${flag:-<none>}" \
      "$(cut -c1-7 < "$out/wall")" "$(stat -c%s "$out/ctrl.err")" \
      "$(cut -d' ' -f1 /proc/loadavg)"
  done
done
