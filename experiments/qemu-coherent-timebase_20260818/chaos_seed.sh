#!/usr/bin/env bash
# chaos_seed.sh <seed> — strict, no-icount, 2 MTTCG vCPUs, chaos with a fixed seed.
set -uo pipefail
H=/home/newton/work/dev-hermit/hermit
OUT=/home/newton/work/dev-hermit/experiments/qemu-coherent-timebase_20260818/logs
seed=$1
label=chaos-seed${seed}${2:-}
start=$(date +%s.%N)
timeout --signal=KILL 1800s "$H/target/release/hermit" --log error run --strict \
  --chaos --seed "$seed" --record-preemptions-to "$OUT/$label.preempts" -- \
  "$(command -v qemu-system-x86_64)" \
  -nodefaults -nic none -machine q35 -cpu max -m 256M \
  -accel tcg,thread=multi -smp 2 -rtc base=utc,clock=vm \
  -kernel "$H/target/qemu-busybox/bzImage" \
  -initrd "$H/target/qemu-busybox/initramfs-busybox.cpio.gz" \
  -display none -serial stdio -monitor none -no-reboot \
  -append 'console=ttyS0 panic=-1 rdinit=/init no_timer_check lpj=999964' \
  >"$OUT/$label.console" 2>"$OUT/$label.stderr"
rc=$?
end=$(date +%s.%N)
printf '%-18s rc=%-4s wall=%6.1fs bytes=%-7s pass=%s sha256=%s\n' \
  "$label" "$rc" "$(echo "$end - $start" | bc)" \
  "$(stat -c %s "$OUT/$label.console")" \
  "$(grep -c 'HERMIT-QEMU-BUSYBOX-PASS' "$OUT/$label.console")" \
  "$(sha256sum "$OUT/$label.console" | cut -c1-16)"
