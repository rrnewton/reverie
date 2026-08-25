#!/usr/bin/env python3
"""A non-advisory actuator, and a bias that changes DURING the run.

`nice` and SCHED_IDLE turned out to be advisory: a deprioritised vCPU thread runs
freely whenever the other one blocks, and 5 of 21 runs went the opposite way to
the request. Contention is not advisory -- a thread sharing a host CPU with H busy
loops gets about 1/(H+1) of it whatever its priority says.

Arms:
  E   vCPU0 on a free host CPU, vCPU1 on the contended one
  F   the mirror
  G   alternate the two every --period seconds, so the bias changes mid-run

The bias is applied when the guest prints ADVANCE-START, so the guest boot -- which
crawls when a vCPU is starved -- is outside the measured window.

Usage: steer.py <arm> <tag> [--hogs N] [--period S] [--k K]
"""
import json, os, signal, socket, subprocess, sys, time
from pathlib import Path

D = Path("/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820")
KERNEL = "/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage"
FREE_CPU, BUSY_CPU = "200", "201"

arm, tag = sys.argv[1], sys.argv[2]
args = sys.argv[3:]
def opt(name, default):
    return type(default)(args[args.index(name) + 1]) if name in args else default
hogs_n, period, K = opt("--hogs", 7), opt("--period", 0.2), opt("--k", 100_000_000)

ser = D / f"ser-{tag}.log"
ser.write_text("")
qmp = D / f"qmp-{tag}.sock"
if qmp.exists():
    qmp.unlink()
disk = D / f"disk-{tag}.qcow2"
subprocess.run(["qemu-img", "create", "-f", "qcow2", str(disk), "64M"],
               check=True, capture_output=True)

qemu = subprocess.Popen([
    "/usr/bin/qemu-system-x86_64", "-nodefaults", "-nic", "none", "-machine", "q35",
    "-cpu", "max", "-m", "512M", "-accel", "tcg,thread=multi", "-smp", "2",
    "-rtc", "base=utc,clock=vm", "-kernel", KERNEL,
    "-initrd", str(D / "initramfs-vs.cpio.gz"), "-display", "none", "-monitor", "none",
    "-no-reboot", "-serial", f"file:{ser}", "-qmp", f"unix:{qmp},server=on,wait=off",
    "-drive", f"if=none,id=store,file={disk},format=qcow2",
    "-append", "console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog no_timer_check "
               f"vsk={K}"])

# QEMU's own answer for which host thread is which vCPU.
deadline = time.time() + 20
while time.time() < deadline and not qmp.exists():
    time.sleep(0.05)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(str(qmp))
f = s.makefile("rw")
f.readline()
f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
f.write(json.dumps({"execute": "query-cpus-fast"}) + "\n"); f.flush()
while True:
    reply = json.loads(f.readline())
    if "return" in reply:
        break
tid = {c["cpu-index"]: c["thread-id"] for c in reply["return"]}
print(f"tag={tag} arm={arm} qpid={qemu.pid} vcpu0_tid={tid[0]} vcpu1_tid={tid[1]} "
      f"hogs={hogs_n} period={period}s k={K}")

hogs = [subprocess.Popen(["taskset", "-c", BUSY_CPU, "sh", "-c", "while :; do :; done"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(hogs_n)]


def place(fast_cpu_index):
    """Put the favoured vCPU thread on the free host CPU and the other on the busy one."""
    for idx in (0, 1):
        cpu = FREE_CPU if idx == fast_cpu_index else BUSY_CPU
        subprocess.run(["taskset", "-pc", cpu, str(tid[idx])],
                       capture_output=True)


def ticks(t):
    try:
        with open(f"/proc/{qemu.pid}/task/{t}/stat") as fh:
            g = fh.read().rsplit(") ", 1)[1].split()
        return int(g[11]) + int(g[12])
    except (OSError, IndexError):
        return 0


started = time.monotonic()
while time.monotonic() - started < 300:
    if "ADVANCE-START" in ser.read_text(errors="replace"):
        break
    if qemu.poll() is not None:
        break
    time.sleep(0.02)

favoured = 0 if arm in ("E", "G") else 1
place(favoured)
t_begin, s0, s1 = time.monotonic(), ticks(tid[0]), ticks(tid[1])
print(f"bias applied at ADVANCE-START: favouring vcpu{favoured} "
      f"(free cpu {FREE_CPU}, contended cpu {BUSY_CPU} with {hogs_n} busy loops)")

flips = 0
last = t_begin
while time.monotonic() - t_begin < 300:
    text = ser.read_text(errors="replace")
    if "\nADVANCE R" in text or text.startswith("ADVANCE R"):
        break
    if qemu.poll() is not None:
        break
    if arm == "G" and time.monotonic() - last >= period:
        favoured ^= 1
        place(favoured)
        flips += 1
        last = time.monotonic()
    time.sleep(0.02)
w, e0, e1 = time.monotonic() - t_begin, ticks(tid[0]), ticks(tid[1])
print(f"HOSTCPU window={w:.2f}s v0_ticks={e0-s0} v1_ticks={e1-s1} flips={flips}")

for h in hogs:
    h.kill()
qemu.send_signal(signal.SIGTERM)
try:
    qemu.wait(timeout=10)
except subprocess.TimeoutExpired:
    qemu.kill()
for line in ser.read_text(errors="replace").splitlines():
    if line.startswith(("ADVANCE", "VS-GUEST-READY")):
        print(line)
