#!/bin/bash
# Ground-truth check for "read the -d exec log tail while the guest is stopped".
#
# gdb `interrupt` halts the vCPU wherever it happens to be (unlike a breakpoint,
# which halts BEFORE entering the target TB).  If the log tail mechanism is
# sound, the last logged TB must be the one containing the RIP gdb reports.
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
TAG=${1:-gdbint}
MODE=${2:-bare}
QEMU=/usr/libexec/qemu-kvm
HERMIT=/home/newton/work/dev-hermit/hermit/target/release/hermit
SOCK=$DIR/gdb-$TAG.sock
CONSOLE=$DIR/console-$TAG.txt
rm -f "$SOCK" "$CONSOLE" "$DIR/trace-$TAG.txt"

if [ "$MODE" = hermit ]; then
  PRE=("$HERMIT" --log error run --no-sequentialize-threads --preemption-timeout disabled --no-virtualize-cpuid --)
else
  PRE=()
fi

"${PRE[@]}" "$QEMU" -accel tcg,thread=single -smp 1 -icount shift=0,sleep=off -m 2048M -cpu max \
  -kernel /home/newton/work/dev-hermit/ignored/qemu-linux/bzImage -initrd "$DIR/lean.cpio.gz" \
  -display none -serial file:"$CONSOLE" -monitor none -no-reboot \
  -rtc base=utc,clock=vm \
  -gdb "unix:$SOCK,server=on,wait=off" \
  -d "${DFLAGS:-exec,nochain}" -dfilter 0xffffffffa0000000..0xffffffffa1000000 -D "$DIR/trace-$TAG.txt" \
  -append 'console=ttyS0 panic=-1 rdinit=/init' &
QPID=$!

for i in $(seq 1 180); do
    grep -q -- '--- WORKLOAD BEGIN ---' "$CONSOLE" 2>/dev/null && break
    kill -0 $QPID 2>/dev/null || break
    sleep 1
done

# Interrupt repeatedly; report RIP and the log tail each time.
for n in 1 2 3; do
  gdb -batch -nx -ex 'set confirm off' -ex 'set architecture i386:x86-64' \
      -ex "target remote $SOCK" \
      -ex 'interrupt' -ex 'printf "SAMPLE-RIP %#lx\n", $rip' \
      -ex "shell grep '^Trace' $DIR/trace-$TAG.txt | tail -1 | sed 's/^/LOG-TAIL /'" \
      -ex 'detach' 2>&1 | grep -E 'SAMPLE-RIP|LOG-TAIL'
  sleep 1
done

kill $QPID 2>/dev/null; wait $QPID 2>/dev/null
