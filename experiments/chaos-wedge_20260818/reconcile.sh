#!/usr/bin/env bash
# reconcile.sh <label> <profile:compat|strict> <machine:q35|default> <kernel:demo|boot> <initrd:busybox|tiny> <timeout>
# All runs: no -icount, -smp 1. Varies only the axes named above.
set -uo pipefail
H=/home/newton/work/dev-hermit/hermit
OUT=/home/newton/work/dev-hermit/experiments/chaos-wedge_20260818/logs
QEMU=$(command -v qemu-system-x86_64)

label=$1; profile=$2; machine=$3; kernel=$4; initrd=$5; tmo=$6

case $kernel in
  demo) KIMG=$H/target/qemu-busybox/bzImage ;;
  boot) KIMG=/boot/vmlinuz ;;
esac
case $initrd in
  busybox) IIMG=$H/target/qemu-busybox/initramfs-busybox.cpio.gz
           APPEND='console=ttyS0 panic=-1 rdinit=/init' ;;
  tiny)    IIMG=$H/target/qemu-boot-smoke/initramfs.cpio.gz
           APPEND='console=ttyS0 panic=-1 rdinit=/init' ;;
esac

qemu_args=(-nodefaults -nic none)
[[ $machine == q35 ]] && qemu_args+=(-machine q35 -cpu max)
qemu_args+=(-m 256M -accel 'tcg,thread=single' -smp 1
  -rtc 'base=utc,clock=vm' -kernel "$KIMG" -initrd "$IIMG"
  -display none -serial stdio -monitor none -no-reboot -append "$APPEND")

case $profile in
  compat) hermit_args=(--log error run --no-sequentialize-threads --max-timeslice disabled) ;;
  strict) hermit_args=(--log error run --strict) ;;
esac

start=$(date +%s.%N)
timeout --signal=KILL "${tmo}s" "$H/target/release/hermit" "${hermit_args[@]}" -- \
  "$QEMU" "${qemu_args[@]}" >"$OUT/$label.console" 2>"$OUT/$label.stderr"
rc=$?
end=$(date +%s.%N)
bytes=$(stat -c %s "$OUT/$label.console")
pass=$(grep -cE 'HERMIT-QEMU-BUSYBOX-PASS|SHARED_FUTEX_QEMU_KERNEL_OK' "$OUT/$label.console")
panic=$(grep -c 'Kernel panic' "$OUT/$label.console")
printf '%-34s prof=%-6s mach=%-7s kern=%-4s rc=%-4s wall=%6.1fs bytes=%-7s pass=%s panic=%s\n' \
  "$label" "$profile" "$machine" "$kernel" "$rc" "$(echo "$end - $start" | bc)" \
  "$bytes" "$pass" "$panic"
