#!/bin/sh
# In-guest extraction of the JITed-address -> scheduler-source-line mapping.
#
# Phase A: default JIT settings, load scx_rlfifo, dump mapping.
# Phase B: bpf_jit_harden=2 (constant blinding for everyone), reload, dump.
# Everything goes to the serial console with greppable markers.
mount -t proc     none /proc        2>/dev/null
mount -t sysfs    none /sys         2>/dev/null
mount -t devtmpfs none /dev         2>/dev/null || mount -t tmpfs none /dev 2>/dev/null
mount -t bpf      none /sys/fs/bpf  2>/dev/null
mkdir -p /sys/fs/cgroup 2>/dev/null
mount -t cgroup2  none /sys/fs/cgroup 2>/dev/null
[ -c /dev/ttyS0 ] || mknod /dev/ttyS0 c 4 64 2>/dev/null
export LD_LIBRARY_PATH=/lib64

echo "GUEST-KERNEL: $(uname -r)"
echo "GUEST-JIT-ENABLE: $(cat /proc/sys/net/core/bpf_jit_enable)"
echo "GUEST-JIT-HARDEN: $(cat /proc/sys/net/core/bpf_jit_harden)"
echo "GUEST-JIT-KALLSYMS: $(cat /proc/sys/net/core/bpf_jit_kallsyms)"
echo "GUEST-KPTR-RESTRICT: $(cat /proc/sys/kernel/kptr_restrict)"

run_phase() {
    TAG=$1
    echo "===== PHASE $TAG BEGIN (harden=$(cat /proc/sys/net/core/bpf_jit_harden)) ====="
    /bin/scx_rlfifo >/tmp/sched-$TAG.log 2>&1 &
    P=$!
    i=0
    while [ $i -lt 30 ]; do
        [ "$(cat /sys/kernel/sched_ext/state 2>/dev/null)" = "enabled" ] && break
        kill -0 $P 2>/dev/null || break
        sleep 1; i=$((i+1))
    done
    echo "PHASE $TAG sched_ext state: $(cat /sys/kernel/sched_ext/state 2>/dev/null)"
    [ -e /sys/kernel/sched_ext/root/ops ] && \
        echo "PHASE $TAG active ops: $(cat /sys/kernel/sched_ext/root/ops)"

    /bin/bpf_riplines --all-struct-ops > /tmp/map-$TAG.txt 2>&1
    echo "PHASE $TAG dump bytes: $(wc -c < /tmp/map-$TAG.txt)"
    echo "PHASE $TAG dump md5: $(md5sum < /tmp/map-$TAG.txt)"
    echo "--- PHASE $TAG MAP BEGIN ---"
    cat /tmp/map-$TAG.txt
    echo "--- PHASE $TAG MAP END ---"

    echo "PHASE $TAG kallsyms bpf_prog entries: $(grep -c bpf_prog_ /proc/kallsyms)"
    echo "PHASE $TAG kallsyms sample:"
    grep bpf_prog_.*rustland /proc/kallsyms | head -6

    kill $P 2>/dev/null
    i=0
    while [ $i -lt 20 ]; do
        [ "$(cat /sys/kernel/sched_ext/state 2>/dev/null)" != "enabled" ] && break
        sleep 1; i=$((i+1))
    done
    echo "PHASE $TAG sched_ext state after unload: $(cat /sys/kernel/sched_ext/state 2>/dev/null)"
    echo "===== PHASE $TAG END ====="
}

run_phase A

echo 2 > /proc/sys/net/core/bpf_jit_harden
echo "SET bpf_jit_harden -> $(cat /proc/sys/net/core/bpf_jit_harden)"
run_phase B

echo "GUEST-DONE"
sync
poweroff -f
