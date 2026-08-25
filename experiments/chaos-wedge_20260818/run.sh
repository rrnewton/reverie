#!/usr/bin/env bash
# run.sh <label> <icount:on|off> <smp> <chaos:none|SEED> <timeout_s> [extra hermit args...]
# Boots the busybox guest under Hermit --strict and samples progress every 10s:
# console bytes, guest-process CPU seconds, and the newest guest printk timestamp.
set -uo pipefail
H=/home/newton/work/dev-hermit/hermit
OUT=/home/newton/work/dev-hermit/experiments/chaos-wedge_20260818/logs
QEMU=$(command -v qemu-system-x86_64)

label=$1; icount=$2; smp=$3; chaos=$4; tmo=$5; shift 5
extra_hermit=("$@")

qemu_args=(-nodefaults -nic none -machine q35 -cpu max -m 256M
  -accel "tcg,thread=$( [[ $smp == 1 ]] && echo single || echo multi )" -smp "$smp")
[[ $icount == on ]] && qemu_args+=(-icount 'shift=0,sleep=on')
qemu_args+=(-rtc 'base=utc,clock=vm'
  -kernel "$H/target/qemu-busybox/bzImage"
  -initrd "$H/target/qemu-busybox/initramfs-busybox.cpio.gz"
  -display none -serial stdio -monitor none -no-reboot
  -append 'console=ttyS0 panic=-1 rdinit=/init no_timer_check lpj=999964')

hermit_args=(--log error run --strict)
[[ $chaos != none ]] && hermit_args+=(--chaos --seed "$chaos")
hermit_args+=("${extra_hermit[@]}")

: >"$OUT/$label.console"
timeout --signal=KILL "${tmo}s" "$H/target/release/hermit" "${hermit_args[@]}" -- \
  "$QEMU" "${qemu_args[@]}" >"$OUT/$label.console" 2>"$OUT/$label.stderr" &
runner=$!

# Sample progress while it runs.
: >"$OUT/$label.progress"
t=0
while kill -0 $runner 2>/dev/null; do
  sleep 10; t=$((t+10))
  bytes=$(stat -c %s "$OUT/$label.console" 2>/dev/null || echo 0)
  # newest guest printk timestamp
  gt=$(grep -oE '^\[[ ]*[0-9]+\.[0-9]+\]' "$OUT/$label.console" 2>/dev/null | tail -1 | tr -d '[] ')
  # total CPU seconds of every qemu thread under this run
  cpu=$(ps -L -o time= --ppid $runner 2>/dev/null | head -1)
  qpid=$(pgrep -f "no_timer_check lpj=999964" 2>/dev/null | tail -1)
  qcpu=$(awk '{print ($14+$15)/100}' /proc/$qpid/stat 2>/dev/null || echo NA)
  printf 't=%-5s bytes=%-7s guest_t=%-12s qemu_cpu_s=%s\n' \
    "$t" "$bytes" "${gt:-none}" "$qcpu" >>"$OUT/$label.progress"
done
wait $runner; rc=$?

printf '%-28s rc=%-4s bytes=%-7s pass=%s last_guest_t=%s\n' \
  "$label" "$rc" "$(stat -c %s "$OUT/$label.console")" \
  "$(grep -c 'HERMIT-QEMU-BUSYBOX-PASS' "$OUT/$label.console")" \
  "$(grep -oE '^\[[ ]*[0-9]+\.[0-9]+\]' "$OUT/$label.console" | tail -1 | tr -d '[] ')"
