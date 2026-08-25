#!/usr/bin/env bash
# One arm of the host-thread-bias experiment.
#   run.sh <tag> <arm> [host-cpu] [K]
# arm: A none | B share one host cpu | C share + deprioritise vCPU1 | D share + deprioritise vCPU0
set -u
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
TAG=$1; ARM=$2; HCPU=${3:-200}; K=${4:-30000000}
KERNEL=/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage
SER="$D/ser-$TAG.log"; : > "$SER"; T0=$(date +%s.%N)
rm -f "$D/disk-$TAG.qcow2" "$D/qmp-$TAG.sock"
qemu-img create -f qcow2 "$D/disk-$TAG.qcow2" 64M >/dev/null

/usr/bin/qemu-system-x86_64 -nodefaults -nic none -machine q35 -cpu max \
  -m 512M -accel tcg,thread=multi -smp 2 -rtc base=utc,clock=vm \
  -kernel "$KERNEL" -initrd "$D/initramfs-vs.cpio.gz" \
  -display none -monitor none -no-reboot \
  -serial "file:$SER" -qmp "unix:$D/qmp-$TAG.sock,server=on,wait=off" \
  -drive "if=none,id=store,file=$D/disk-$TAG.qcow2,format=qcow2" \
  -append "console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog no_timer_check vsk=$K" &
QPID=$!

# QEMU tells us which host thread is which vCPU; this build does not name its threads.
MAP=$(python3 "$D/vcputids.py" "$D/qmp-$TAG.sock" 2>/dev/null)
V0=$(echo "$MAP" | awk '$1==0{print $2}')
V1=$(echo "$MAP" | awk '$1==1{print $2}')
echo "t=+$(printf %.2f $(echo "$(date +%s.%N) - $T0"|bc))s tag=$TAG arm=$ARM qpid=$QPID vcpu0_tid=$V0 vcpu1_tid=$V1 hostcpu=$HCPU k=$K"
if [ -z "$V0" ] || [ -z "$V1" ]; then echo "FATAL: vcpu threads not found"; kill $QPID 2>/dev/null; exit 1; fi

case "$ARM" in
  A) echo "bias: none" ;;
  B|C|D|N*)
     taskset -pc "$HCPU" "$V0" >/dev/null 2>&1 || echo "WARN taskset v0 failed"
     taskset -pc "$HCPU" "$V1" >/dev/null 2>&1 || echo "WARN taskset v1 failed"
     echo "bias: both vcpu threads on host cpu $HCPU"
     ;;
esac
case "$ARM" in
  C) TARGET=$V1 ;;
  D) TARGET=$V0 ;;
  N*) TARGET=$V1; NICE=${ARM#N} ;;
  *) TARGET="" ;;
esac
if [ -n "$TARGET" ]; then
  case "$ARM" in
    N*) renice -n "$NICE" -p "$TARGET" >/dev/null 2>&1 \
          && echo "bias: tid $TARGET -> nice $NICE" || echo "WARN renice failed" ;;
    *)  chrt -i -p 0 "$TARGET" 2>/dev/null && echo "bias: tid $TARGET -> SCHED_IDLE" \
          || echo "WARN could not deprioritise $TARGET" ;;
  esac
fi
# Record what actually took effect, rather than what was requested.
for t in $V0 $V1; do
  echo "  applied tid=$t aff=$(taskset -pc "$t" 2>/dev/null | sed 's/.*: //') sched=$(chrt -p "$t" 2>/dev/null | tr '\n' ' ')"
done

echo "t=+$(printf %.2f $(echo "$(date +%s.%N) - $T0"|bc))s bias applied"

# HOST CPU TIME PER vCPU THREAD, bracketed to the workload alone.
#
# The arm label says what bias was REQUESTED. This says how much host CPU each
# vCPU thread actually GOT, so the claim "choosing which host thread runs chooses
# which guest CPU advances" is measured on both sides instead of assumed on one.
python3 "$D/sampler.py" "$QPID" "$V0" "$V1" "$SER" "$D/trace-$TAG.tsv" 300 | sed 's/^/HOSTCPU /'
echo "t=+$(printf %.2f $(echo "$(date +%s.%N) - $T0"|bc))s workload finished"
kill $QPID 2>/dev/null; wait $QPID 2>/dev/null
echo "--- result ---"
grep -aE "VS-GUEST-READY|ADVANCE" "$SER" | head -6
