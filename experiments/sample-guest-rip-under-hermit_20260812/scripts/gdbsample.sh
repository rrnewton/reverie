#!/bin/bash
# Point-sample a guest RIP through QEMU's gdbstub, stopping the guest at a
# deterministic guest address (a JITed BPF instruction), not at a host moment.
#
# 1. boot the lean SCX guest with the gdbstub listening,
# 2. wait for the guest to publish its own JITed-address -> source map,
# 3. take a subprogram base out of that map,
# 4. set a breakpoint there, continue, and read RIP when it hits.
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
TAG=${1:-gdb1}
MODE=${2:-bare}
HERMIT=/home/newton/work/dev-hermit/hermit/target/release/hermit
QEMU=/usr/libexec/qemu-kvm
SOCK=$DIR/gdb-$TAG.sock
CONSOLE=$DIR/console-$TAG.txt
rm -f "$SOCK" "$CONSOLE"

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
  -d "${DFLAGS:-exec}" -dfilter 0xffffffffa0000000..0xffffffffa1000000 -D "$DIR/trace-$TAG.txt" \
  -append 'console=ttyS0 panic=-1 rdinit=/init' &
QPID=$!
echo "qemu pid $QPID"

for i in $(seq 1 120); do
    grep -q -- '--- MAP END ---' "$CONSOLE" 2>/dev/null && break
    kill -0 $QPID 2>/dev/null || break
    sleep 1
done

ADDR=$(sed -n '/--- MAP BEGIN ---/,/--- MAP END ---/p' "$CONSOLE" \
       | awk '/=== prog .*rustland_enqueu/{f=1} f&&/subprog\[0\]/{print $2; exit}' \
       | sed 's/ksym=//')
echo "breakpoint target (rustland_enqueue subprog 0 base): $ADDR"

gdb -batch -nx \
    -ex 'set confirm off' \
    -ex 'set architecture i386:x86-64' \
    -ex "target remote $SOCK" \
    -ex "break *$ADDR" \
    -ex 'continue' \
    -ex 'info registers rip rsp' \
    -ex 'printf "GDB-RIP %#lx\n", $rip' \
    -ex "shell echo '--- last Trace lines in the -d exec log while the vCPU is stopped ---'" \
    -ex "shell grep '^Trace' $DIR/trace-$TAG.txt | tail -2" \
    -ex 'detach' 2>&1 | tee "$DIR/gdb-$TAG.txt"

kill $QPID 2>/dev/null
wait $QPID 2>/dev/null
echo "console tail:"; tail -3 "$CONSOLE"
