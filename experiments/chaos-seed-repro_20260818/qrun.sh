#!/usr/bin/env bash
# qrun.sh <label> <smp> <seed> <stop-after-turn> <timeout>
set -uo pipefail
H=/home/newton/work/dev-hermit/hermit
D=/home/newton/work/dev-hermit/experiments/chaos-seed-repro_20260818
lbl=$1; smp=$2; seed=$3; turn=$4; tmo=$5
timeout --signal=KILL "${tmo}s" "$H/target/release/hermit" --log error run --strict \
  --chaos --seed "$seed" --stop-after-turn "$turn" \
  --record-preemptions-to "$D/rec/$lbl.json" -- \
  "$(command -v qemu-system-x86_64)" -nodefaults -nic none -machine q35 -cpu max -m 256M \
  -accel "tcg,thread=$( [[ $smp == 1 ]] && echo single || echo multi )" -smp "$smp" \
  -rtc base=utc,clock=vm \
  -kernel "$H/target/qemu-busybox/bzImage" \
  -initrd "$H/target/qemu-busybox/initramfs-busybox.cpio.gz" \
  -display none -serial stdio -monitor none -no-reboot \
  -append 'console=ttyS0 panic=-1 rdinit=/init no_timer_check lpj=999964' \
  >"$D/rec/$lbl.console" 2>"$D/rec/$lbl.stderr"
echo "$lbl rc=$? rec=$(stat -c %s "$D/rec/$lbl.json" 2>/dev/null || echo MISSING) console=$(stat -c %s "$D/rec/$lbl.console")"
