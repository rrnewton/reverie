#!/bin/sh
# Lean init for guest-RIP sampling runs: attach a real SCX scheduler, publish
# the JITed-address -> source table for THIS load, run a fixed workload so the
# scheduler ops actually execute, then power off.
mount -t proc     none /proc        2>/dev/null
mount -t sysfs    none /sys         2>/dev/null
mount -t devtmpfs none /dev         2>/dev/null || mount -t tmpfs none /dev 2>/dev/null
mount -t bpf      none /sys/fs/bpf  2>/dev/null
mkdir -p /sys/fs/cgroup 2>/dev/null
mount -t cgroup2  none /sys/fs/cgroup 2>/dev/null
[ -c /dev/ttyS0 ] || mknod /dev/ttyS0 c 4 64 2>/dev/null
export LD_LIBRARY_PATH=/lib64

echo "GUEST-KERNEL: $(uname -r)"
echo "GUEST-JIT-HARDEN: $(cat /proc/sys/net/core/bpf_jit_harden)"

/bin/scx_rlfifo >/tmp/sched.log 2>&1 &
P=$!
i=0
while [ $i -lt 30 ]; do
    [ "$(cat /sys/kernel/sched_ext/state 2>/dev/null)" = "enabled" ] && break
    kill -0 $P 2>/dev/null || break
    sleep 1; i=$((i+1))
done
echo "SCHED-STATE: $(cat /sys/kernel/sched_ext/state 2>/dev/null)"
[ -e /sys/kernel/sched_ext/root/ops ] && echo "SCHED-OPS: $(cat /sys/kernel/sched_ext/root/ops)"

echo "--- MAP BEGIN ---"
/bin/bpf_riplines --all-struct-ops
echo "--- MAP END ---"

# Everything the module/vmalloc range contains, so a traced PC that is not in a
# struct_ops program can still be attributed.
echo "--- KALLSYMS BEGIN ---"
grep -E '^ffffffffa[0-9a-f]{7} ' /proc/kallsyms | sort
echo "--- KALLSYMS END ---"

# Fixed workload: four concurrent CPU-bound tasks the scheduler must dispatch.
echo "--- WORKLOAD BEGIN ---"
W=""
for n in 1 2 3 4; do
    ( seq 1 4000 | md5sum > /tmp/w$n.out ) &
    W="$W $!"
done
for w in $W; do wait $w; done
for n in 1 2 3 4; do echo "w$n: $(cat /tmp/w$n.out)"; done
echo "--- WORKLOAD END ---"

echo "ENABLE-SEQ: $(cat /sys/kernel/sched_ext/enable_seq 2>/dev/null)"
kill $P 2>/dev/null
echo "GUEST-DONE"
sync
poweroff -f
