#!/usr/bin/env python3
"""Ask a running QEMU which accelerator it actually chose.

Configuration is not evidence: demos 5 and 6 pass no -accel at all, so QEMU
applies its built-in `kvm:tcg` preference list.  This launches the real command
shape and asks the running process via QMP.
"""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path("/home/newton/work/dev-hermit")
ASSETS = ROOT / "ignored/qemu-linux"
QEMU = os.environ.get("QEMU_BIN", "/usr/bin/qemu-system-x86_64")


def qmp_connect(path, process, timeout=90.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return None
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(5.0)
        try:
            s.connect(str(path))
            return s
        except (FileNotFoundError, ConnectionRefusedError, BlockingIOError,
                socket.timeout) as error:
            last = error
            s.close()
            time.sleep(0.05)
    raise TimeoutError("QMP never became ready: {}".format(last))


class Qmp:
    def __init__(self, sock):
        self.f = sock.makefile("rwb", buffering=0)
        self._read()
        self.execute("qmp_capabilities")

    def _read(self):
        while True:
            line = self.f.readline()
            if not line:
                raise RuntimeError("QMP closed")
            msg = json.loads(line)
            if "event" in msg:
                continue
            return msg

    def execute(self, command, arguments=None):
        payload = {"execute": command}
        if arguments:
            payload["arguments"] = arguments
        self.f.write((json.dumps(payload) + "\n").encode())
        return self._read()


def build(smp, icount, accel, serial, qmp_socket, disk, loadvm):
    command = [QEMU, "-machine", "q35", "-cpu", "max", "-smp", str(smp),
               "-m", "512M", "-display", "none", "-monitor", "none"]
    if accel:
        command[1:1] = ["-accel", accel]
    command += ["-serial", "file:{}".format(serial),
                "-qmp", "unix:{},server=on,wait=off".format(qmp_socket),
                "-drive", "if=none,id=s,file={},format=qcow2".format(disk)]
    if loadvm:
        command += ["-loadvm", loadvm]
    if icount:
        command += ["-icount", "shift=0,sleep=off"]
    command += ["-rtc", "base=2022-01-01T00:00:00,clock=vm",
                "-kernel", str(ASSETS / "bzImage"),
                "-initrd", str(ASSETS / "initramfs.cpio.gz"),
                "-append", "console=ttyS0 reboot=t"]
    return command


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--smp", type=int, default=1)
    p.add_argument("--icount", action="store_true")
    p.add_argument("--accel", default="")
    p.add_argument("--hermit", action="store_true")
    p.add_argument("--label", default="probe")
    p.add_argument("--settle", type=float, default=6.0)
    args = p.parse_args()

    work = Path(tempfile.mkdtemp(prefix="accel.", dir=str(ROOT / "ignored/exec-control")))
    disk = work / "disk.qcow2"
    subprocess.run(["cp", str(ASSETS / "hermit-boot.qcow2"), str(disk)], check=True)
    serial = work / "serial.txt"
    qmp_socket = work / "qmp.sock"

    command = build(args.smp, args.icount, args.accel, serial, qmp_socket, disk, None)
    if args.hermit:
        command = [str(ROOT / "hermit/target/release/hermit"), "run", "--strict",
                   "--no-rcb-time", "--target-timeslice", "100000",
                   "--max-timeslice", "disabled", "--"] + command

    log = (work / "qemu.log").open("wb")
    started = time.monotonic()
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                               start_new_session=True)
    result = {"label": args.label, "smp": args.smp, "icount": args.icount,
              "accel_flag": args.accel or "(none: QEMU default kvm:tcg)",
              "under_hermit": args.hermit, "workdir": str(work)}
    try:
        sock = qmp_connect(qmp_socket, process, 90.0)
        if sock is None:
            result["outcome"] = "QEMU EXITED before QMP"
            result["exit_code"] = process.returncode
            result["log"] = (work / "qemu.log").read_text()[-1500:]
            print(json.dumps(result, indent=2))
            return
        qmp = Qmp(sock)
        result["query_kvm"] = qmp.execute("query-kvm").get("return")
        # Which vCPU host threads exist, and how many?
        cpus = qmp.execute("query-cpus-fast").get("return", [])
        result["vcpu_count_reported"] = len(cpus)
        result["vcpu_thread_ids"] = [c.get("thread-id") for c in cpus]

        # Ground truth from the kernel: what threads does QEMU actually have?
        qemu_pid = None
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            try:
                with open("/proc/{}/stat".format(entry)) as fh:
                    fields = fh.read().rsplit(") ", 1)[1].split()
                if int(fields[2]) != os.getpgid(process.pid):
                    continue
                with open("/proc/{}/comm".format(entry)) as fh:
                    if fh.read().strip().startswith("qemu-system"):
                        qemu_pid = int(entry)
                        break
            except (OSError, IndexError, ValueError):
                continue
        result["qemu_pid"] = qemu_pid
        if qemu_pid:
            threads = {}
            for tid in os.listdir("/proc/{}/task".format(qemu_pid)):
                try:
                    with open("/proc/{}/task/{}/comm".format(qemu_pid, tid)) as fh:
                        threads[int(tid)] = fh.read().strip()
                except OSError:
                    pass
            result["qemu_threads"] = threads
            result["qemu_thread_count"] = len(threads)
            result["cpu_named_threads"] = sorted(
                n for n in threads.values() if "CPU" in n or "vcpu" in n.lower())
            # Did QEMU open /dev/kvm?
            kvm_fds = []
            fd_dir = "/proc/{}/fd".format(qemu_pid)
            try:
                for fd in os.listdir(fd_dir):
                    try:
                        target = os.readlink(os.path.join(fd_dir, fd))
                    except OSError:
                        continue
                    if "kvm" in target:
                        kvm_fds.append("{} -> {}".format(fd, target))
            except OSError:
                pass
            result["kvm_fds_open"] = kvm_fds

        time.sleep(args.settle)
        result["serial_bytes_after_{}s".format(args.settle)] = (
            serial.stat().st_size if serial.exists() else 0)
        result["wall_seconds"] = round(time.monotonic() - started, 1)
        result["outcome"] = "ok"
    finally:
        try:
            os.killpg(os.getpgid(process.pid), 9)
        except ProcessLookupError:
            pass
        log.close()
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
