#!/usr/bin/env python3
"""Count the interrupts QEMU actually delivers to the guest, native vs under hermit.

QEMU's `-d int` logs every interrupt and exception delivered to the guest CPU,
with its vector, and needs no rebuild -- it is the timer-path instrument that
had not been used. On x86 Linux the LAPIC tick is LOCAL_TIMER_VECTOR = 0xec.

Configuration is copied from experiments/cc-repro_20260818/cc_qemu.py so the
numbers are comparable with the earlier /proc/interrupts measurements. That
harness is left untouched; this writes only into its own directory.

  boot    build the snapshot once
  resume  -loadvm it, optionally under hermit, run a workload, count vectors
"""
import argparse, json, os, re, socket, subprocess, sys, time
from collections import Counter
from pathlib import Path

D = Path(__file__).resolve().parent
SRC = Path("/home/newton/work/dev-hermit/experiments/cc-repro_20260818")
KERNEL = Path("/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage")
HERMIT = Path(os.environ.get(
    "HERMIT_BIN", "/home/newton/work/dev-hermit/hermit/target/release/hermit"))
QEMU = "/usr/bin/qemu-system-x86_64"
READY = "CC-GUEST-READY"


def qmp(sock_path, commands, timeout=60):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    deadline = time.time() + timeout
    while True:
        try:
            s.connect(str(sock_path)); break
        except OSError:
            if time.time() > deadline:
                raise
            time.sleep(0.05)
    f = s.makefile("rwb")
    f.readline()
    out = []
    for cmd in [{"execute": "qmp_capabilities"}] + commands:
        f.write((json.dumps(cmd) + "\n").encode()); f.flush()
        while True:
            line = f.readline()
            if not line:
                break
            msg = json.loads(line)
            if "return" in msg or "error" in msg:
                out.append(msg); break
    s.close()
    return out


def qemu_args(serial_arg, qmp_sock, disk, initrd, loadvm=None, smp=2,
              mem="512M", append_extra="", dlog=None):
    """Identical to cc_qemu.qemu_args, plus the -d int interrupt log."""
    a = [QEMU, "-nodefaults", "-nic", "none", "-machine", "q35", "-cpu", "max",
         "-m", mem, "-accel", "tcg,thread=multi" if smp > 1 else "tcg,thread=single",
         "-smp", str(smp), "-rtc", "base=utc,clock=vm",
         "-kernel", str(KERNEL), "-initrd", str(initrd),
         "-display", "none", "-monitor", "none", "-no-reboot"]
    a += serial_arg
    a += ["-qmp", "unix:%s,server=on,wait=off" % qmp_sock,
          "-drive", "if=none,id=store,file=%s,format=qcow2" % disk]
    if loadvm:
        a += ["-loadvm", loadvm]
    if dlog:
        a += ["-d", "int", "-D", str(dlog)]
    a += ["-append", ("console=ttyS0 panic=-1 rdinit=/init " + append_extra).strip()]
    return a


def wait_for(path, marker, proc, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if marker in Path(path).read_text(errors="replace"):
                return True
        except OSError:
            pass
        if proc.poll() is not None:
            time.sleep(0.3)
            try:
                return marker in Path(path).read_text(errors="replace")
            except OSError:
                return False
        time.sleep(0.2)
    return False


# `-d int` lines look like:  "    99: v=ec e=0000 i=0 cpl=0 IP=..."
VEC = re.compile(r"^\s*\d+:\s+v=([0-9a-f]{2})\b")


def count_vectors(path):
    c = Counter()
    try:
        with open(path, errors="replace") as fh:
            for line in fh:
                m = VEC.match(line)
                if m:
                    c[m.group(1)] += 1
    except OSError:
        pass
    return c


def do_boot(args):
    serial = D / "boot-serial.log"
    qmp_sock = D / "qmp-boot.sock"
    for p in (serial, qmp_sock):
        if p.exists():
            p.unlink()
    cmd = qemu_args(["-serial", "file:%s" % serial], qmp_sock, args.disk,
                    args.initrd, smp=args.smp, append_extra=args.append_extra)
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ok = wait_for(serial, READY, proc, args.timeout)
    if not ok:
        proc.kill()
        print("boot: guest never reached %s" % READY)
        print(Path(serial).read_text(errors="replace")[-600:])
        return 1
    qmp(qmp_sock, [{"execute": "stop"},
                   {"execute": "human-monitor-command",
                    "arguments": {"command-line": "savevm %s" % args.name}},
                   {"execute": "quit"}])
    proc.wait(timeout=30)
    print("BOOT ok, snapshot=%s disk=%s" % (args.name, args.disk))
    return 0


def do_resume(args):
    tag = args.tag
    base = D / ("r-%s" % tag)
    out_log = D / ("r-%s.capture" % tag)
    dlog = D / ("r-%s.int" % tag)
    qmp_sock = D / ("qmp-%s.sock" % tag)
    for p in (out_log, dlog, qmp_sock):
        if p.exists():
            p.unlink()
    for suf in (".in", ".out"):
        p = Path(str(base) + suf)
        if p.exists():
            p.unlink()
        os.mkfifo(str(p))
    # Each run needs its OWN image: a qcow2 takes an exclusive write lock.
    run_disk = D / ("disk-%s.qcow2" % tag)
    subprocess.run(["cp", "--reflink=auto", args.disk, str(run_disk)], check=True)

    cmd = qemu_args(["-serial", "pipe:%s" % base], qmp_sock, run_disk,
                    args.initrd, loadvm=args.name, smp=args.smp,
                    append_extra=args.append_extra, dlog=dlog)
    if args.hermit:
        # `--strict` and `--no-sequentialize-threads` are mutually exclusive:
        # hermit refuses the combination outright ("the argument '--strict'
        # cannot be used with '--no-sequentialize-threads'"). So the profile
        # selects one or the other rather than layering flags on --strict.
        pre = [str(HERMIT), "--log", os.environ.get("HERMIT_LOG", "error"), "run"]
        if args.profile == "compat":
            pre += ["--no-sequentialize-threads", "--max-timeslice", "disabled"]
        elif args.profile == "novirt":
            # Sequentialized (hermit's default, so the handle is kept) but with
            # virtual time OFF. --strict CANNOT be combined with
            # --no-virtualize-time: strict fails closed on the first
            # gettimeofday --  "inbound syscall: gettimeofday(...) = ?" then
            # "Sandbox container exited unexpectedly", exit 1. So this is the
            # only way to change the clock while keeping sequentialization.
            pre += ["--no-virtualize-time", "--no-virtualize-metadata"]
        else:
            pre += ["--strict"]
        if args.chaos:
            pre += ["--chaos", "--seed", str(args.seed)]
        cmd = pre + args.hermit_extra.split() + ["--"] + cmd

    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                            stderr=open(D / ("r-%s.err" % tag), "wb"))
    reader = subprocess.Popen(["dd", "if=%s.out" % base, "of=%s" % out_log,
                               "bs=1", "status=none"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(args.settle)
    try:
        with open(str(base) + ".in", "w") as fin:
            fin.write(args.command + "\n"); fin.flush()
    except Exception as e:
        print("resume: could not write command: %s" % e)
    ok = wait_for(out_log, args.expect, proc, args.timeout)
    wall = time.time() - t0
    try:
        qmp(qmp_sock, [{"execute": "quit"}], timeout=10)
    except Exception:
        pass
    proc.kill(); reader.kill()
    time.sleep(0.5)

    text = Path(out_log).read_text(errors="replace") if out_log.exists() else ""
    vec = count_vectors(dlog)
    tick = vec.get("ec", 0)
    total = sum(vec.values())
    hit = [l for l in text.splitlines() if args.expect in l]
    top = ", ".join("v=%s:%d" % (v, n) for v, n in vec.most_common(6))
    print("RUN tag=%-14s wall=%6.1fs found=%-5s ticks(v=ec)=%-7d all_int=%-8d | %s"
          % (tag, wall, ok, tick, total, top))
    for l in hit:
        print("    %s" % l.strip())
    for l in text.splitlines():
        if "LOC:" in l or "uptime" in l:
            print("    %s" % l.strip())
    run_disk.unlink(missing_ok=True)
    return 0 if ok else 1


def main():
    p = argparse.ArgumentParser()
    p.add_argument("mode", choices=("boot", "resume"))
    p.add_argument("--disk", default=str(D / "snap.qcow2"))
    p.add_argument("--initrd", default=str(D / "initramfs-win.cpio.gz"))
    p.add_argument("--name", default="s0")
    p.add_argument("--smp", type=int, default=2)
    p.add_argument("--append-extra", default="tsc=nowatchdog no_timer_check")
    p.add_argument("--timeout", type=float, default=900)
    p.add_argument("--settle", type=float, default=1.5)
    p.add_argument("--tag", default="t")
    p.add_argument("--command", default="order")
    p.add_argument("--expect", default="ORDER")
    p.add_argument("--hermit", action="store_true")
    p.add_argument("--chaos", action="store_true")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--hermit-extra", default="")
    p.add_argument("--profile", choices=("strict", "compat", "novirt"), default="strict")
    a = p.parse_args()
    return do_boot(a) if a.mode == "boot" else do_resume(a)


if __name__ == "__main__":
    sys.exit(main())
