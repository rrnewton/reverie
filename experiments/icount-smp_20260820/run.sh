#!/bin/bash
# Does -icount ALONE permit multiple vCPUs?  Three restrictions have been
# conflated on this project and each arm here isolates exactly one of them:
#
#   rr1   no icount, -smp 2, round-robin        control: 2 vCPUs work at all
#   rr1m  no icount, -smp 2, MTTCG              control: 2 vCPUs, threaded
#   ic1   -icount shift=0, -smp 2, NO rr=       THE QUESTION
#   ic2   -icount shift=0, -smp 2, rr=record    the record/replay blocker
#   ic3   -icount shift=0, -smp 2, thread=multi the MTTCG-vs-icount rejection
#   ic4   -icount shift=0, -smp 1               uniprocessor reference
#
# Every arm is run on BOTH the stock system QEMU and the source-built one, so
# no result can be attributed to the local vCPU-channel patch.  The channel
# env var is never set, so the patched binary takes its stock path either way.
set -uo pipefail

E=/home/newton/work/dev-hermit/experiments/icount-smp_20260820
KERNEL=/home/newton/work/dev-hermit/ignored/qemu-linux/bzImage
INITRD="$E/initramfs.cpio.gz"
WALL=${WALL:-600}
ITERS=${ITERS:-20000000}

for f in "$KERNEL" "$INITRD"; do [ -r "$f" ] || { echo "missing: $f"; exit 1; }; done

# $1 arm name, $2 qemu binary tag (stock|built), $3.. extra qemu args
run_arm() {
    local arm=$1 which=$2; shift 2
    local qemu
    case "$which" in
        stock) qemu=/usr/bin/qemu-system-x86_64 ;;
        built) qemu=/home/newton/work/qemu-vcpu-channel_20260820/build/qemu-system-x86_64 ;;
        *) echo "bad binary tag $which"; return 1 ;;
    esac
    local tag="$arm.$which"
    local serial="$E/runs/$tag.serial.log"
    local stderr="$E/runs/$tag.stderr.log"
    local t0 t1 rc
    echo "################ $tag ################"
    echo "cmd: $qemu $* -smp ... (see below)"
    printf '  args:'; printf ' %q' "$@"; echo
    t0=$(date +%s.%N)
    timeout --foreground -k 10 "$WALL" \
        "$qemu" "$@" \
            -m 512M -kernel "$KERNEL" -initrd "$INITRD" \
            -append "console=ttyS0 panic=-1 rdinit=/init iters=$ITERS" \
            -display none -serial "file:$serial" -monitor none -no-reboot \
        >/dev/null 2>"$stderr"
    rc=$?
    t1=$(date +%s.%N)
    echo "exit=$rc wall=$(echo "$t1 - $t0" | bc)s $([ $rc -eq 124 ] && echo '(TIMED OUT)')"
    echo "--- qemu stderr (verbatim) ---"
    cat "$stderr" 2>/dev/null | head -10
    echo "--- guest markers ---"
    grep -E 'ICOUNT-SMP-GUEST-(START|DONE)|NPROC=|CPUINFO-PROCESSOR-COUNT=|SYS-CPU-ONLINE=|PHASE[12]-.*-(START|DONE)|worker2 cpu=' "$serial" 2>/dev/null
    echo "--- guest per-CPU jiffies, every /proc/stat snapshot ---"
    grep -E '^cpu[0-9]' "$serial" 2>/dev/null
    echo
}

case "${1:-all}" in
  rr1)  run_arm rr1  "${2:-stock}" -accel tcg,thread=single -smp 2 ;;
  rr1m) run_arm rr1m "${2:-stock}" -accel tcg,thread=multi  -smp 2 ;;
  ic1)  run_arm ic1  "${2:-stock}" -accel tcg,thread=single -smp 2 -icount shift=0,sleep=off ;;
  ic2)  run_arm ic2  "${2:-stock}" -accel tcg,thread=single -smp 2 -icount "shift=0,rr=record,rrfile=$E/runs/rr.bin" ;;
  ic3)  run_arm ic3  "${2:-stock}" -accel tcg,thread=multi  -smp 2 -icount shift=0,sleep=off ;;
  ic4)  run_arm ic4  "${2:-stock}" -accel tcg,thread=single -smp 1 -icount shift=0,sleep=off ;;
  ic2s) run_arm ic2s "${2:-stock}" -accel tcg,thread=single -smp 1 -icount "shift=0,rr=record,rrfile=$E/runs/rr1cpu.bin" ;;
  *) echo "usage: $0 {rr1|rr1m|ic1|ic2|ic3|ic4|ic2s} [stock|built]"; exit 2 ;;
esac
