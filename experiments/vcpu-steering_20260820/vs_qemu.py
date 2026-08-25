#!/usr/bin/env python3
"""Boot / snapshot / resume a 2-vCPU MTTCG guest with no -icount.

Productionised copy of the demo 5/6 machinery (demos/lib/qemu_controller.py and
demos/lib/qemu-snapshot.sh). The demos are NOT modified: they hardcode -smp 1 and
-icount shift=0,sleep=off, and this experiment needs the opposite on both counts
because QEMU refuses MTTCG when icount is enabled, and chaos needs two host vCPU
threads to have anything to steer.

  boot   : run natively (fast), wait for the guest READY marker, savevm, quit.
  resume : -loadvm the snapshot, optionally under `hermit ... --chaos --seed N`,
           type a command into the guest shell over a serial pipe, capture output.
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

D = Path(__file__).resolve().parent
KERNEL = Path("/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage")
HERMIT = Path(os.environ.get("HERMIT_BIN",
    "/home/newton/work/dev-hermit/hermit/target/release/hermit"))
QEMU = "/usr/bin/qemu-system-x86_64"
READY = "VS-GUEST-READY"


def qmp(sock_path, commands, timeout=60):
    """Minimal QMP client: negotiate, then run each command in order."""
    deadline = time.time() + timeout
    while not Path(sock_path).exists():
        if time.time() > deadline:
            raise TimeoutError("qmp socket never appeared: %s" % sock_path)
        time.sleep(0.1)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(str(sock_path))
    f = s.makefile("rwb")
    f.readline()  # greeting
    out = []
    for cmd in [{"execute": "qmp_capabilities"}] + commands:
        f.write((json.dumps(cmd) + "\n").encode())
        f.flush()
        while True:
            line = f.readline()
            if not line:
                raise RuntimeError("qmp closed early")
            msg = json.loads(line)
            if "event" in msg:
                continue
            out.append(msg)
            break
    s.close()
    return out


def qemu_args(serial_arg, qmp_sock, disk, initrd, loadvm=None, smp=2, mem="512M", append_extra=""):
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
    a += ["-append", ("console=ttyS0 panic=-1 rdinit=/init " + append_extra).strip()]
    return a


def wait_for(path, marker, proc, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if marker in Path(path).read_text(errors="replace"):
                return True
        except FileNotFoundError:
            pass
        if proc.poll() is not None:
            return False
        time.sleep(0.1)
    return False


def do_boot(args):
    serial = D / "boot-serial.log"
    qmp_sock = D / "boot-qmp.sock"
    for p in (serial, qmp_sock):
        p.unlink(missing_ok=True)
    cmd = qemu_args(["-serial", "file:%s" % serial], qmp_sock, args.disk,
                    args.initrd, smp=args.smp, append_extra=args.append_extra)
    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if not wait_for(serial, READY, proc, args.timeout):
        proc.kill()
        print("BOOT FAILED: no %s within %ss" % (READY, args.timeout))
        print(Path(serial).read_text(errors="replace")[-800:])
        return 1
    boot_s = time.time() - t0
    time.sleep(2)  # let the shell settle before capturing state
    r = qmp(qmp_sock, [{"execute": "stop"},
                       {"execute": "human-monitor-command",
                        "arguments": {"command-line": "savevm %s" % args.name}},
                       {"execute": "quit"}])
    proc.wait(timeout=60)
    err = [m for m in r if "error" in m]
    print("BOOT ok in %.1fs; savevm '%s' %s" %
          (boot_s, args.name, "FAILED: %s" % err if err else "ok"))
    return 1 if err else 0


def do_resume(args):
    tag = args.tag
    serial_base = D / ("res-%s" % tag)
    qmp_sock = D / ("res-%s-qmp.sock" % tag)
    out_log = D / ("res-%s.capture" % tag)
    for suf in (".in", ".out"):
        p = Path(str(serial_base) + suf)
        p.unlink(missing_ok=True)
        os.mkfifo(p)
    qmp_sock.unlink(missing_ok=True)
    out_log.unlink(missing_ok=True)

    # A qcow2 takes an exclusive write lock, so concurrent resumes MUST NOT share
    # one image. Give each run a private copy; the snapshot is read-only to us.
    run_disk = D / ("disk-%s.qcow2" % tag)
    run_disk.unlink(missing_ok=True)
    subprocess.run(["cp", "--reflink=auto", str(args.disk), str(run_disk)], check=True)
    cmd = qemu_args(["-serial", "pipe:%s" % serial_base], qmp_sock, run_disk,
                    args.initrd, loadvm=args.name, smp=args.smp, append_extra=args.append_extra)
    if args.seed is not None:
        pre = [str(HERMIT), "--log", args.hermit_log]
        if args.hermit_log_file:
            pre += ["--log-file", args.hermit_log_file]
        cmd = pre + ["run", "--strict", "--chaos",
               "--seed", str(args.seed)] + args.hermit_extra.split() + ["--"] + cmd
    elif args.hermit:
        pre = [str(HERMIT), "--log", args.hermit_log]
        if args.hermit_log_file:
            pre += ["--log-file", args.hermit_log_file]
        cmd = pre + ["run", "--strict"] + args.hermit_extra.split() + ["--"] + cmd

    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                            stderr=open(D / ("res-%s.err" % tag), "wb"))
    # Drain the guest's serial output in the background.
    reader = subprocess.Popen(["dd", "if=%s.out" % serial_base, "of=%s" % out_log,
                               "bs=1", "status=none"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(args.settle)
    try:
        with open(str(serial_base) + ".in", "w") as fin:
            fin.write(args.command + "\n")
            fin.flush()
    except Exception as e:
        print("resume: could not write command: %s" % e)
    ok = wait_for(out_log, args.expect, proc, args.timeout)
    wall = time.time() - t0
    try:
        qmp(qmp_sock, [{"execute": "quit"}], timeout=10)
    except Exception:
        pass
    proc.kill()
    reader.kill()
    text = Path(out_log).read_text(errors="replace") if out_log.exists() else ""
    hit = [l for l in text.splitlines() if args.expect in l]
    print("RESUME tag=%s seed=%s wall=%.1fs found=%s | %s" %
          (tag, args.seed, wall, ok, hit[0] if hit else text.strip()[-160:]))
    return 0 if ok else 1


def main():
    p = argparse.ArgumentParser()
    p.add_argument("mode", choices=("boot", "resume"))
    p.add_argument("--disk", default=str(D / "snap.qcow2"))
    p.add_argument("--initrd", default=str(D / "initramfs-shell.cpio.gz"))
    p.add_argument("--name", default="cc-ready")
    p.add_argument("--smp", type=int, default=2)
    p.add_argument("--timeout", type=float, default=180)
    p.add_argument("--settle", type=float, default=3)
    p.add_argument("--seed", type=int)
    p.add_argument("--hermit", action="store_true")
    p.add_argument("--hermit-extra", default="")
    p.add_argument("--tag", default="a")
    p.add_argument("--command", default="racer")
    p.add_argument("--expect", default="RACER")
    p.add_argument("--append-extra", default="")
    p.add_argument("--hermit-log", default="error")
    p.add_argument("--hermit-log-file", default="")
    a = p.parse_args()
    sys.exit(do_boot(a) if a.mode == "boot" else do_resume(a))


if __name__ == "__main__":
    main()
