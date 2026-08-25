#!/usr/bin/env python3
"""Resume the snapshot under hermit and measure the SAME two things as before.

The detector (guest/advance.c), the two measured quantities (each vCPU host
thread's share of host CPU in the workload window, and each guest CPU's share of
the guest counters) and the 20 ms sampling are unchanged from the contention
experiment, so the numbers land in the same table.

Only the actuator changes: host contention is replaced by hermit's own scheduler
deciding which traced thread runs.

Usage: hermit_run.py <tag> [--k K] [--hermit-extra "..."] [--no-hermit] [--settle S]
"""
import datetime, json, os, signal, socket, subprocess, sys, time
from pathlib import Path

D = Path("/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820")
HERMIT = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
KERNEL = "/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage"
QEMU = "/usr/bin/qemu-system-x86_64"

tag = sys.argv[1]
args = sys.argv[2:]
def opt(name, default):
    return type(default)(args[args.index(name) + 1]) if name in args else default
K = opt("--k", 30_000_000)
extra = opt("--hermit-extra", "")
settle = opt("--settle", 25.0)
timeout = opt("--timeout", 900.0)
use_hermit = "--no-hermit" not in args

base = D / f"h-{tag}"
cap = D / f"h-{tag}.capture"
qmp_sock = D / f"h-{tag}-qmp.sock"
disk = D / f"h-{tag}.qcow2"
for p in (Path(str(base) + ".in"), Path(str(base) + ".out"), qmp_sock, cap, disk):
    p.unlink(missing_ok=True)
for suf in (".in", ".out"):
    os.mkfifo(str(base) + suf)
SNAP = D / ("snap-ts.qcow2" if ("--preempt" in args or "--pingpong" in args
                                or "--advance" in args)
            else "snap-or.qcow2" if opt("--orient", -1) >= 0 else "snap.qcow2")
subprocess.run(["cp", "--reflink=auto", str(SNAP), str(disk)], check=True)

qcmd = [QEMU, "-nodefaults", "-nic", "none", "-machine", "q35", "-cpu", "max",
        "-m", "512M", "-accel", "tcg,thread=multi", "-smp", "2",
        "-rtc", "base=utc,clock=vm", "-kernel", KERNEL,
        "-initrd", str(D / "initramfs-shell.cpio.gz"), "-display", "none",
        "-monitor", "none", "-no-reboot", "-serial", f"pipe:{base}",
        "-qmp", f"unix:{qmp_sock},server=on,wait=off",
        "-drive", f"if=none,id=store,file={disk},format=qcow2", "-loadvm", "cc-ready",
        "-append", "console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog no_timer_check"]
LOGLEVEL = opt("--hermit-log", "error")
LOGFILE = opt("--hermit-log-file", "")
pre = [HERMIT, "--log", LOGLEVEL]
if LOGFILE:
    pre += ["--log-file", LOGFILE]
# --strict rejects unsupported syscalls, and with time virtualization OFF the guest's
# gettimeofday becomes one: the pair fails with
#   ERROR detcore: [detcore, dtid 3] inbound syscall: gettimeofday(...) = ?
#   Error: Sandbox container exited unexpectedly
# so the two cannot be combined. The predeclared configuration uses the novirt flags
# with DEFAULT sequentialization, not --strict.
sub = ["run"] + ([] if "--no-strict" in args else ["--strict"])
cmd = (pre + sub + extra.split() + ["--"] + qcmd if use_hermit else qcmd)
print("actuator=%s extra=%r k=%d" % ("hermit" if use_hermit else "none", extra, K))

err = open(D / f"h-{tag}.err", "wb")
# Own process group, so teardown can SIGKILL the whole tree. SIGTERM to the hermit
# process alone leaks: the guest runs as a PID-namespace init and the kernel discards
# SIGTERM there, and `pkill -P` only reaches direct children. 61 of my own hermit and
# QEMU processes were left behind before this was fixed, which the admission gate
# would (correctly) reject every subsequent arm for.
proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=err, start_new_session=True)
reader = subprocess.Popen(["dd", f"if={base}.out", f"of={cap}", "bs=1", "status=none"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _teardown(pr, rd):
    try:
        os.killpg(os.getpgid(pr.pid), signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        pr.wait(timeout=15)
    except Exception:
        pass
    rd.kill()


def qemu_pid():
    """The QEMU process on the HOST. Under hermit it is a descendant, not proc itself."""
    out = subprocess.run(["pgrep", "-P", str(proc.pid)], capture_output=True, text=True)
    cands = [proc.pid] + [int(x) for x in out.stdout.split()]
    for _ in range(3):
        more = []
        for c in cands:
            r = subprocess.run(["pgrep", "-P", str(c)], capture_output=True, text=True)
            more += [int(x) for x in r.stdout.split()]
        cands += more
    for c in dict.fromkeys(cands):
        try:
            if "qemu-system" in Path(f"/proc/{c}/comm").read_text():
                return c
        except OSError:
            pass
    return None


def nspid_map(pid):
    """namespace tid -> host tid. Hermit puts the guest in a PID namespace, so the
    thread-ids QMP reports are namespace-local and cannot index /proc directly."""
    m = {}
    for t in Path(f"/proc/{pid}/task").iterdir():
        try:
            for line in (t / "status").read_text().splitlines():
                if line.startswith("NSpid:"):
                    f = line.split()
                    m[int(f[-1])] = int(t.name)
                    break
        except OSError:
            pass
    return m


qp = None
deadline = time.time() + 120
while time.time() < deadline and qp is None:
    qp = qemu_pid()
    time.sleep(0.2)
if qp is None:
    print("FATAL no qemu process"); proc.kill(); reader.kill(); sys.exit(1)

deadline = time.time() + 120
while time.time() < deadline and not qmp_sock.exists():
    time.sleep(0.1)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(str(qmp_sock))
f = s.makefile("rw"); f.readline()
f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
f.write(json.dumps({"execute": "query-cpus-fast"}) + "\n"); f.flush()
while True:
    reply = json.loads(f.readline())
    if "return" in reply:
        break
nsmap = nspid_map(qp)
tid = {c["cpu-index"]: nsmap.get(c["thread-id"], c["thread-id"]) for c in reply["return"]}
for _t in sorted(nsmap.values()):
    _st = Path(f"/proc/{qp}/task/{_t}/status").read_text()
    _ns = [l for l in _st.splitlines() if l.startswith("NSpid:")][0]
    print(f"  NSPID host_tid={_t} {_ns.strip()}")
print(f"tag={tag} hermit_pid={proc.pid} qemu_hostpid={qp} "
      f"vcpu0_tid={tid[0]} vcpu1_tid={tid[1]} (qmp said "
      f"{[c['thread-id'] for c in reply['return']]})")


def ticks(t):
    try:
        with open(f"/proc/{qp}/task/{t}/stat") as fh:
            g = fh.read().rsplit(") ", 1)[1].split()
        return int(g[11]) + int(g[12])
    except (OSError, IndexError):
        return 0


time.sleep(settle)
if "--advance" in args:
    # The spin detector: two processes pinned one per guest CPU, first to K reads the
    # other's counter. The advancement ratio is what the faithfulness test needs.
    a0, a1 = ticks(tid[0]), ticks(tid[1])
    t0 = time.monotonic()
    utc_start = None
    with open(str(base) + ".in", "w") as fin:
        fin.write(f"advance {K}\n"); fin.flush()
    while time.monotonic() - t0 < timeout:
        text = cap.read_text(errors="replace") if cap.exists() else ""
        if utc_start is None and "ADVANCE-START" in text:
            utc_start = datetime.datetime.now(datetime.timezone.utc).isoformat()
        # Break on a COMPLETE result line, not on the prefix. Breaking as soon as
        # "ADVANCE R" appeared tore the run down mid-line and the capture read
        # "ADVANCE R1 me=20" -- the counters, which are the whole measurement, were
        # cut off. Require the trailing field and a newline.
        if any(l.startswith("ADVANCE R") and "other_started=" in l
               for l in text.splitlines()[:-1] or []):
            print("WINDOW_UTC start=%s end=%s"
                  % (utc_start, datetime.datetime.now(datetime.timezone.utc).isoformat()))
            break
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    print("ADV-RUN wall=%.2fs" % (time.monotonic() - t0))
    for line in (cap.read_text(errors="replace") if cap.exists() else "").splitlines():
        if "ADVANCE" in line or "lockup" in line or "csd:" in line or "rcu: INFO" in line:
            print(line.strip())
    _teardown(proc, reader)
    sys.exit(0)
if "--pingpong" in args:
    a0, a1 = ticks(tid[0]), ticks(tid[1])
    t0 = time.monotonic()
    utc_start = None
    with open(str(base) + ".in", "w") as fin:
        fin.write("pingpong %d %s\n" % (K, opt("--placement", "split"))); fin.flush()
    while time.monotonic() - t0 < timeout:
        text = cap.read_text(errors="replace") if cap.exists() else ""
        if utc_start is None and "PP-START" in text:
            utc_start = datetime.datetime.now(datetime.timezone.utc).isoformat()
        if "PP-END" in text:
            print("WINDOW_UTC start=%s end=%s"
                  % (utc_start, datetime.datetime.now(datetime.timezone.utc).isoformat()))
            break
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    print("PP-RUN wall=%.2fs v0_ticks=%d v1_ticks=%d"
          % (time.monotonic() - t0, ticks(tid[0]) - a0, ticks(tid[1]) - a1))
    for line in (cap.read_text(errors="replace") if cap.exists() else "").splitlines():
        if "PP" in line or "lockup" in line or "csd:" in line or "rcu: INFO" in line:
            print(line.strip())
    _teardown(proc, reader)
    sys.exit(0)
if "--preempt" in args:
    # Fixed work per guest CPU; the observable is how often the guest kernel took
    # the CPU away from a process that still wanted it.
    a0, a1 = ticks(tid[0]), ticks(tid[1])
    t0 = time.monotonic()
    utc_start = None
    with open(str(base) + ".in", "w") as fin:
        fin.write("preempt %d %s\n" % (K, opt("--placement", "split"))); fin.flush()
    while time.monotonic() - t0 < timeout:
        text = cap.read_text(errors="replace") if cap.exists() else ""
        if utc_start is None and "PREEMPT-START" in text:
            utc_start = datetime.datetime.now(datetime.timezone.utc).isoformat()
        if "PREEMPT-END" in text:
            print("WINDOW_UTC start=%s end=%s"
                  % (utc_start, datetime.datetime.now(datetime.timezone.utc).isoformat()))
            break
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    print("PREEMPT-RUN wall=%.2fs v0_ticks=%d v1_ticks=%d"
          % (time.monotonic() - t0, ticks(tid[0]) - a0, ticks(tid[1]) - a1))
    for line in (cap.read_text(errors="replace") if cap.exists() else "").splitlines():
        if "PREEMPT" in line or "csd" in line or "soft lockup" in line or "rcu" in line:
            print(line.strip())
    _teardown(proc, reader); sys.exit(0)
if "--idle-only" in args:
    # No workload at all. Establishes how many vCPU turns the settle phase alone
    # contributes, which is the alternative explanation for a flat turns split.
    a0, a1 = ticks(tid[0]), ticks(tid[1])
    t0 = time.monotonic()
    with open(str(base) + ".in", "w") as fin:
        fin.write("echo IDLEMARK\n"); fin.flush()
    time.sleep(opt("--idle-secs", 40.0))
    print("IDLE wall=%.2fs v0_ticks=%d v1_ticks=%d"
          % (time.monotonic() - t0, ticks(tid[0]) - a0, ticks(tid[1]) - a1))
    _teardown(proc, reader); sys.exit(0)
ORIENT = opt("--orient", -1)
if ORIENT >= 0:
    a0, a1 = ticks(tid[0]), ticks(tid[1])
    t0 = time.monotonic()
    utc_start = None
    with open(str(base) + ".in", "w") as fin:
        fin.write(f"advance {K} {ORIENT}\n"); fin.flush()
    while time.monotonic() - t0 < timeout:
        text = cap.read_text(errors="replace") if cap.exists() else ""
        if utc_start is None and "ADVANCE-START" in text:
            utc_start = datetime.datetime.now(datetime.timezone.utc).isoformat()
        if "ADVANCE-ONLY-DONE" in text:
            print("WINDOW_UTC start=%s end=%s"
                  % (utc_start, datetime.datetime.now(datetime.timezone.utc).isoformat()))
            break
        if proc.poll() is not None:
            break
        time.sleep(0.05)
    print("ORIENT loaded_guest_cpu=%d wall=%.2fs v0_ticks=%d v1_ticks=%d"
          % (ORIENT, time.monotonic() - t0, ticks(tid[0]) - a0, ticks(tid[1]) - a1))
    for line in (cap.read_text(errors="replace") if cap.exists() else "").splitlines():
        if "ADVANCE" in line:
            print(line.strip())
    _teardown(proc, reader); sys.exit(0)
with open(str(base) + ".in", "w") as fin:
    fin.write(f"advance {K}\n"); fin.flush()

t_begin = None
started = time.monotonic()
s0 = s1 = 0
while time.monotonic() - started < timeout:
    text = cap.read_text(errors="replace") if cap.exists() else ""
    if t_begin is None and "ADVANCE-START" in text:
        t_begin, s0, s1 = time.monotonic(), ticks(tid[0]), ticks(tid[1])
        utc_start = datetime.datetime.now(datetime.timezone.utc).isoformat()
    if t_begin is not None and ("\nADVANCE R" in text or text.startswith("ADVANCE R")):
        print("WINDOW_UTC start=%s end=%s"
              % (utc_start, datetime.datetime.now(datetime.timezone.utc).isoformat()))
        break
    if proc.poll() is not None:
        break
    time.sleep(0.02)
if t_begin is None:
    print("WINDOW none (workload never announced itself)")
else:
    print("HOSTCPU window=%.2fs v0_ticks=%d v1_ticks=%d"
          % (time.monotonic() - t_begin, ticks(tid[0]) - s0, ticks(tid[1]) - s1))
for line in (cap.read_text(errors="replace") if cap.exists() else "").splitlines():
    if "ADVANCE" in line:
        print(line.strip())
_teardown(proc, reader)
