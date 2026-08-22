#!/usr/bin/env python3
"""Boot/resume with the workload taken from a disk instead of the serial port.

  diskcmd.py boot                       -- boot with the placeholder image, savevm
  diskcmd.py resume <tag> "<command>"   -- resume with an image holding <command>

The command drive is attached at BOTH boot and resume with the same id and size,
because vmstate records the device model and a device absent at snapshot cannot
appear at resume. It is readonly=on so `savevm` does not refuse it: a writable raw
drive is not snapshottable and QEMU rejects the save.

Nothing is written into the guest over the serial port. The serial line is a
`-serial file:` transcript that QEMU only writes to.
"""
import json, os, shutil, socket, subprocess, sys, time
from pathlib import Path

D = Path(__file__).resolve().parent
KERNEL = "/home/newton/work/dev-hermit/hermit/target/qemu-busybox/bzImage"
QEMU = "/usr/bin/qemu-system-x86_64"
NAME = "disk-ready"


def snap_path(direct: int, hold: int = 0) -> Path:
    return D / ("snap-disk-d%d-h%d.qcow2" % (direct, hold))
HERMIT = "/home/newton/work/dev-hermit/hermit/target/release/hermit"


def cmd_image(path: Path, text: str, nonce: str = "") -> None:
    """Fixed 4096-byte raw image; the size must match between boot and resume.

    Sector 0 holds the command. Sector 1 holds a nonce the guest reads only after the
    poll loop: sector 1 is never read before the snapshot, so it cannot be in the
    guest's page cache. That separates "the guest cached sector 0" from "the device
    is serving old content".
    """
    b = text.encode() + b"\n"
    assert len(b) <= 512
    sec0 = b + b"\0" * (512 - len(b))
    n = (nonce or "no-nonce").encode() + b"\n"
    sec1 = n + b"\0" * (512 - len(n))
    path.write_bytes(sec0 + sec1 + b"\0" * (4096 - 1024))


def qemu_args(serial, qmp, disk, cmdimg, loadvm=None, initrd="initramfs-disk.cpio.gz",
              direct=1, hold=0):
    a = [QEMU, "-nodefaults", "-nic", "none", "-machine", "q35", "-cpu", "max",
         "-m", "512M", "-accel", "tcg,thread=multi", "-smp", "2",
         "-rtc", "base=utc,clock=vm", "-kernel", KERNEL, "-initrd", str(D / initrd),
         "-display", "none", "-monitor", "none", "-no-reboot",
         "-serial", "file:%s" % serial,
         "-qmp", "unix:%s,server=on,wait=off" % qmp,
         "-drive", "if=none,id=store,file=%s,format=qcow2" % disk,
         "-drive", "if=none,id=cmdsrc,file=%s,format=raw,readonly=on" % cmdimg,
         "-device", "virtio-blk-pci,drive=cmdsrc"]
    if loadvm:
        a += ["-loadvm", loadvm]
    a += ["-append", "console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog "
                     "no_timer_check vsdirect=%d vshold=%d" % (direct, hold)]
    return a


def qmp(sock, cmds, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline and not Path(sock).exists():
        time.sleep(0.05)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(str(sock))
    f = s.makefile("rw"); f.readline()
    f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
    out = []
    for c in cmds:
        f.write(json.dumps(c) + "\n"); f.flush()
        while True:
            r = json.loads(f.readline())
            if "return" in r or "error" in r:
                out.append(r); break
    s.close()
    return out


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


def do_boot(direct=1, hold=0):
    snap = snap_path(direct, hold)
    ser = D / ("disk-boot-%d%d.log" % (direct, hold)); ser.write_text("")
    sock = D / ("disk-boot-%d%d-qmp.sock" % (direct, hold)); sock.unlink(missing_ok=True)
    snap.unlink(missing_ok=True)
    subprocess.run(["qemu-img", "create", "-f", "qcow2", str(snap), "256M"],
                   check=True, capture_output=True)
    wait = D / "cmd-wait.img"; cmd_image(wait, "WAIT")
    p = subprocess.Popen(qemu_args(ser, sock, snap, wait, direct=direct, hold=hold),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if not wait_for(ser, "VS-READ-MODE", p, 180):
        p.kill(); print("BOOT FAILED"); print(ser.read_text()[-600:]); return 1
    time.sleep(2)
    r = qmp(sock, [{"execute": "stop"},
                   {"execute": "human-monitor-command",
                    "arguments": {"command-line": "savevm %s" % NAME}},
                   {"execute": "quit"}])
    p.wait(timeout=60)
    err = [m for m in r if "error" in m or (m.get("return") and str(m["return"]).strip())]
    print("BOOT ok; savevm %r -> %s" % (NAME, err if err else "ok"))
    for line in ser.read_text(errors="replace").splitlines():
        if line.startswith("VS-"):
            print("  " + line.strip())
    return 0


def do_resume(tag, command, use_hermit=False, extra="", timeout=600, direct=1, hold=0):
    ser = D / ("dres-%s.log" % tag); ser.write_text("")
    sock = D / ("dres-%s-qmp.sock" % tag); sock.unlink(missing_ok=True)
    disk = D / ("dres-%s.qcow2" % tag); disk.unlink(missing_ok=True)
    shutil.copyfile(snap_path(direct, hold), disk)
    img = D / ("cmd-%s.img" % tag); cmd_image(img, command, nonce="NONCE-" + tag)
    args = qemu_args(ser, sock, disk, img, loadvm=NAME, direct=direct, hold=hold)
    if use_hermit:
        args = [HERMIT, "--log", "error", "run"] + extra.split() + ["--"] + args
    t0 = time.time()
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL,
                         stderr=open(D / ("dres-%s.err" % tag), "wb"),
                         start_new_session=True)
    ok = wait_for(ser, "VS-CMD-DONE", p, timeout)
    wall = time.time() - t0
    try:
        os.killpg(os.getpgid(p.pid), 9)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        p.wait(timeout=15)
    except Exception:
        pass
    text = ser.read_text(errors="replace")
    print("RESUME tag=%s wall=%.1fs done=%s" % (tag, wall, ok))
    polls = [l for l in text.splitlines() if l.startswith("VS-POLL")]
    for line in text.splitlines():
        if (line.startswith("VS-") and not line.startswith("VS-POLL")) \
                or "HELLO" in line or "ADVANCE" in line:
            print("  " + line.strip())
    for line in polls[:2] + (["  ..."] if len(polls) > 4 else []) + polls[-2:]:
        print("  " + line.strip() if line.startswith("VS-") else line)
    return 0 if ok else 1


if __name__ == "__main__":
    dflag = 0 if "--buffered" in sys.argv else 1
    hflag = 1 if "--hold" in sys.argv else 0
    if sys.argv[1] == "boot":
        sys.exit(do_boot(dflag, hflag))
    sys.exit(do_resume(sys.argv[2], sys.argv[3], direct=dflag, hold=hflag,
                       use_hermit="--hermit" in sys.argv,
                       extra=(sys.argv[sys.argv.index("--hermit-extra") + 1]
                              if "--hermit-extra" in sys.argv else "")))
