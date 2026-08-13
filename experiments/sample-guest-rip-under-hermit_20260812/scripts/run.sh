#!/bin/bash
# run.sh <tag> <mode: bare|seed|hermit> [extra qemu args...]
# Boots the lean SCX guest under TCG+icount with a filtered guest-PC trace.
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
TAG=$1; MODE=$2; shift 2
K=/home/newton/work/dev-hermit/ignored/qemu-linux/bzImage
I="$DIR/lean.cpio.gz"
QEMU=/usr/libexec/qemu-kvm
HERMIT=/home/newton/work/dev-hermit/hermit/target/release/hermit
FILTER=0xffffffffa0000000..0xffffffffa1000000
APPEND='console=ttyS0 panic=-1 rdinit=/init'

QARGS=(-accel tcg,thread=single -smp 1 -icount shift=0,sleep=off -m 2048M -cpu max
       -kernel "$K" -initrd "$I" -display none -serial stdio -monitor none -no-reboot
       -d exec -dfilter "$FILTER" -D "$DIR/trace-$TAG.txt"
       -append "$APPEND")
case "$MODE" in
  seed) QARGS+=(-seed 1) ;;
esac
QARGS+=("$@")

start=$(date +%s)
if [ "$MODE" = hermit ]; then
  # HERMIT_ARGS adds hermit-side flags (e.g. --chaos --sched-seed N).
  read -r -a HX <<< "${HERMIT_ARGS:-}"
  timeout --signal=KILL 1800s "$HERMIT" --log error run \
    --no-sequentialize-threads --preemption-timeout disabled --no-virtualize-cpuid \
    "${HX[@]}" -- \
    "$QEMU" "${QARGS[@]}" > "$DIR/console-$TAG.txt" 2>&1
else
  timeout --signal=KILL 1800s "$QEMU" "${QARGS[@]}" > "$DIR/console-$TAG.txt" 2>&1
fi
rc=$?
echo "$TAG mode=$MODE rc=$rc secs=$(( $(date +%s) - start )) trace_lines=$(wc -l < "$DIR/trace-$TAG.txt" 2>/dev/null) done=$(grep -c GUEST-DONE "$DIR/console-$TAG.txt")"
