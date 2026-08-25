#!/usr/bin/env bash
# boot_one.sh <label> <icount:on|off> <smp> <profile> <timeout_s> [extra kernel cmdline]
set -uo pipefail
H=/home/newton/work/dev-hermit/hermit
OUT=/home/newton/work/dev-hermit/experiments/qemu-coherent-timebase_20260818/logs
KERNEL=$H/target/qemu-busybox/bzImage
INITRD=$H/target/qemu-busybox/initramfs-busybox.cpio.gz
QEMU=$(command -v qemu-system-x86_64)
HERMIT=${HERMIT_BIN:-$H/target/release/hermit}

label=$1; icount=$2; smp=$3; profile=$4; tmo=$5; extra=${6:-}

qemu_args=(-nodefaults -nic none -machine q35 -cpu max -m 256M
  -accel "tcg,thread=$( [[ $smp == 1 ]] && echo single || echo multi )" -smp "$smp")
[[ $icount == on ]] && qemu_args+=(-icount 'shift=0,sleep=on')
qemu_args+=(-rtc 'base=utc,clock=vm' -kernel "$KERNEL" -initrd "$INITRD"
  -display none -serial stdio -monitor none -no-reboot
  -append "console=ttyS0 panic=-1 rdinit=/init $extra")

case $profile in
  strict) hermit_args=(--log error run --strict) ;;
  compat) hermit_args=(--log error run --no-sequentialize-threads --max-timeslice disabled) ;;
  *)      hermit_args=(--log error run) ;;
esac

start=$(date +%s.%N)
timeout --signal=KILL "${tmo}s" "$HERMIT" "${hermit_args[@]}" -- "$QEMU" "${qemu_args[@]}" \
  >"$OUT/$label.console" 2>"$OUT/$label.stderr"
rc=$?
end=$(date +%s.%N)
printf '%-34s rc=%-4s wall=%6.1fs bytes=%-7s pass=%s panic=%s\n' \
  "$label" "$rc" "$(echo "$end - $start" | bc)" "$(stat -c %s "$OUT/$label.console")" \
  "$(grep -c 'HERMIT-QEMU-BUSYBOX-PASS' "$OUT/$label.console")" \
  "$(grep -c 'Kernel panic' "$OUT/$label.console")"
