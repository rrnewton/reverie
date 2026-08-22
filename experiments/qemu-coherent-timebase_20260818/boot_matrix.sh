#!/usr/bin/env bash
# Boot QEMU under Hermit across the icount x vCPU x scheduling matrix.
# Usage: boot_matrix.sh <label> <icount:on|off> <smp> <profile:strict|compat> <timeout_s>
set -uo pipefail

H=/home/newton/work/dev-hermit/hermit
OUT=/home/newton/work/dev-hermit/experiments/qemu-coherent-timebase_20260818/logs
KERNEL=$H/target/qemu-busybox/bzImage
INITRD=$H/target/qemu-busybox/initramfs-busybox.cpio.gz
QEMU=$(command -v qemu-system-x86_64)
HERMIT=$H/target/release/hermit

label=$1; icount=$2; smp=$3; profile=$4; tmo=$5

qemu_args=(
  -nodefaults -nic none -machine q35 -cpu max -m 256M
  -accel "tcg,thread=$( [[ $smp == 1 ]] && echo single || echo multi )"
  -smp "$smp"
)
if [[ $icount == on ]]; then
  qemu_args+=(-icount 'shift=0,sleep=on')
fi
qemu_args+=(
  -rtc 'base=utc,clock=vm'
  -kernel "$KERNEL" -initrd "$INITRD"
  -display none -serial stdio -monitor none -no-reboot
  -append 'console=ttyS0 panic=-1 rdinit=/init'
)

hermit_args=(--log error run)
case $profile in
  strict)  hermit_args=(--log error run --strict) ;;
  compat)  hermit_args=(--log error run --no-sequentialize-threads --max-timeslice disabled) ;;
esac

start=$(date +%s.%N)
timeout --signal=KILL "${tmo}s" "$HERMIT" "${hermit_args[@]}" -- "$QEMU" "${qemu_args[@]}" \
  >"$OUT/$label.console" 2>"$OUT/$label.stderr"
rc=$?
end=$(date +%s.%N)

wall=$(echo "$end - $start" | bc)
bytes=$(stat -c %s "$OUT/$label.console")
pass=$(grep -c 'HERMIT-QEMU-BUSYBOX-PASS' "$OUT/$label.console")
startm=$(grep -c 'HERMIT-QEMU-BUSYBOX-START' "$OUT/$label.console")
cpus=$(grep -c 'smpboot: Booting Node' "$OUT/$label.console")
printf '%s rc=%s wall=%.1fs console_bytes=%s start_marker=%s pass_marker=%s\n' \
  "$label" "$rc" "$wall" "$bytes" "$startm" "$pass"
