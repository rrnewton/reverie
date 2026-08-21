#!/usr/bin/env python3
"""Boot a plain QEMU Linux guest, pause it at the QEMU level, read its state,
resume it, and record which kernel watchdog complains.

No hermit in this path on purpose: the variable under test is the guest's TIME
SOURCE, and hermit virtualizes time for QEMU, which would confound it.  The
arms here are the time sources QEMU itself offers.

The workload arrives on a disk, not over the console, because that is what this
initramfs expects (see its /init).
"""

import json
import os
from pathlib import Path
import re
import select
import shutil
import socket
import subprocess
import tempfile
import time

QEMU = "/usr/bin/qemu-system-x86_64"
ASSETS = Path("/home/newton/work/dev-hermit/ignored/qemu-linux")
KERNEL = ASSETS / "bzImage"
INITRD = ASSETS / "initramfs.cpio.gz"

COMMAND_BEGIN = b"__HERMIT_COMMAND_BEGIN__"


class Qmp:
    def __init__(self, path, deadline):
        self.sock = _connect_unix(path, deadline)
        self.stream = self.sock.makefile("rwb", buffering=0)
        if "QMP" not in self._read():
            raise RuntimeError("no QMP greeting")
        self.execute("qmp_capabilities")

    def _read(self):
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("QMP disconnected")
            try:
                return json.loads(line.decode())
            except json.JSONDecodeError:
                continue

    def execute(self, command, arguments=None):
        request = {"execute": command}
        if arguments:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode() + b"\n")
        while True:
            message = self._read()
            if "event" in message:
                continue
            if "error" in message:
                raise RuntimeError("QMP %s failed: %s" % (command, message["error"]))
            if "return" in message:
                return message["return"]

    def status(self):
        return str(self.execute("query-status").get("status", ""))

    def close(self):
        try:
            self.stream.close()
        finally:
            self.sock.close()


def _connect_unix(path, deadline):
    last = None
    while time.monotonic() < deadline:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(5.0)
        try:
            s.connect(str(path))
            return s
        except OSError as error:
            last = error
            s.close()
            time.sleep(0.05)
    raise TimeoutError("socket never became ready: %s (%s)" % (path, last))


class Serial:
    def __init__(self, path, transcript, deadline):
        self.sock = _connect_unix(path, deadline)
        self.sock.setblocking(False)
        self.buffer = b""
        self.transcript = open(str(transcript), "wb")

    def pump(self, seconds):
        end = time.monotonic() + seconds
        while True:
            remaining = end - time.monotonic()
            if remaining <= 0:
                return
            ready, _, _ = select.select([self.sock], [], [], min(0.2, remaining))
            if not ready:
                continue
            try:
                chunk = self.sock.recv(1 << 16)
            except BlockingIOError:
                continue
            if not chunk:
                return
            self.buffer += chunk
            self.transcript.write(chunk)
            self.transcript.flush()

    def drain(self, quiet_for=0.5, limit=20.0):
        """Read everything the guest has already written and is sitting in the
        socket, and stop once nothing new arrives for `quiet_for`.

        Call this AFTER stopping the guest and BEFORE marking the boundary of
        a pause. Without it the backlog produced just before the pause is
        indistinguishable from output produced after the resume, so a
        "time across the pause" measurement silently reports an ordinary
        sampling interval from inside the backlog instead.
        """
        deadline = time.monotonic() + limit
        last_change = time.monotonic()
        size = len(self.buffer)
        while time.monotonic() < deadline:
            self.pump(0.1)
            if len(self.buffer) != size:
                size = len(self.buffer)
                last_change = time.monotonic()
            elif time.monotonic() - last_change >= quiet_for:
                return size
        return size

    def wait_for(self, marker, timeout):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if marker in self.buffer:
                return True
            self.pump(0.2)
        return marker in self.buffer

    def text(self):
        return self.buffer.decode("utf-8", "replace")

    def close(self):
        try:
            self.sock.close()
        finally:
            self.transcript.close()


def make_command_image(path, command):
    """The initramfs reads one 512-byte sector and runs the first line."""
    payload = command.encode()
    if len(payload) > 500:
        raise ValueError("command does not fit in one sector: %d bytes" % len(payload))
    size = 1 << 20
    with open(str(path), "wb") as image:
        image.write(payload + b"\n")
        image.write(b"\0" * (size - len(payload) - 1))


# Time-source arms.  The accelerator and the clock flags are the whole
# difference between them; everything else about the guest is identical.
ARMS = {
    # Real time, hardware virtualization.  The guest reads kvm-clock, a
    # paravirtual clock derived from the host's, so a host-side pause is
    # visible to it -- and KVM has a channel to TELL the guest it was paused.
    "kvm": ["-accel", "kvm", "-cpu", "host"],
    # Real time, software emulation.  Without -icount the guest's clock tracks
    # host wall time, and there is no paravirtual channel at all.
    "tcg": ["-accel", "tcg", "-cpu", "max"],
    # Virtual time.  -icount makes the guest clock a function of instructions
    # retired; -rtc clock=vm points the RTC at that same virtual clock.  This
    # is the configuration the hermit demos run QEMU in.
    "icount": [
        "-accel", "tcg", "-cpu", "max",
        "-icount", "shift=3,sleep=off",
        "-rtc", "base=2022-01-01T00:00:00,clock=vm",
    ],
}


class Guest:
    def __init__(self, arm, run_dir, command, memory="1024M", append_extra="",
                 cpus=4, numa_nodes=2):
        self.arm = arm
        self.cpus = cpus
        self.numa_nodes = numa_nodes
        self.run_dir = Path(run_dir).resolve()
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.command = command
        self.memory = memory
        self.append_extra = append_extra
        self.process = None
        self.qmp = None
        self.serial = None
        self._socket_dir = None

    def start(self, timeout=240.0):
        # AF_UNIX paths are capped at 108 bytes and this tree's run
        # directories exceed it, so QEMU refuses to bind. Keep the sockets in
        # a short private directory; the logs stay in the run directory.
        self._socket_dir = Path(tempfile.mkdtemp(prefix="pq.", dir="/tmp"))
        qmp_path = self._socket_dir / "q"
        serial_path = self._socket_dir / "s"
        command_image = self.run_dir / "command.img"
        make_command_image(command_image, self.command)
        append = "console=ttyS0 reboot=t"
        if self.append_extra:
            append += " " + self.append_extra
        argv = [
            QEMU,
            "-machine", "q35",
            "-smp", "cpus=%d,sockets=%d,cores=%d,threads=1" % (
                self.cpus, self.numa_nodes, self.cpus // self.numa_nodes),
            "-m", self.memory,
            "-display", "none",
            "-monitor", "none",
            "-no-reboot",
            "-serial", "unix:%s,server=on,wait=off" % serial_path,
            "-qmp", "unix:%s,server=on,wait=off" % qmp_path,
            "-drive", "if=none,id=cmd,file=%s,format=raw,readonly=on" % command_image,
            "-device", "virtio-blk-pci,drive=cmd",
            "-kernel", str(KERNEL),
            "-initrd", str(INITRD),
            "-append", append,
        ] + ARMS[self.arm] + self._numa_args()
        (self.run_dir / "command.txt").write_text(" ".join(argv) + "\n")
        with (self.run_dir / "qemu.stderr").open("wb") as errlog:
            self.process = subprocess.Popen(
                argv, cwd=str(self.run_dir), stdout=subprocess.DEVNULL,
                stderr=errlog, start_new_session=True,
            )
        deadline = time.monotonic() + timeout
        # Fail fast: without this a QEMU that dies on a bad flag is
        # indistinguishable from a slow boot until the whole timeout expires.
        time.sleep(0.3)
        if self.process.poll() is not None:
            raise RuntimeError("QEMU exited immediately (status %d): %s" % (
                self.process.returncode,
                (self.run_dir / "qemu.stderr").read_text().strip()[:400]))
        self.qmp = Qmp(qmp_path, deadline)
        self.serial = Serial(serial_path, self.run_dir / "serial.log", deadline)
        if not self.serial.wait_for(COMMAND_BEGIN, timeout):
            raise TimeoutError("guest never picked up its command disk")
        return self

    def _numa_args(self):
        """Split the vCPUs and RAM evenly across `numa_nodes` guest NUMA nodes."""
        if self.numa_nodes <= 1:
            return []
        per_node = self.cpus // self.numa_nodes
        megabytes = int(self.memory.rstrip("M"))
        # q35 rejects `-numa node,mem=` outright ("not supported by this
        # machine type; use -numa node,memdev instead"), so each node gets its
        # own memory-backend-ram object.
        args = []
        for node in range(self.numa_nodes):
            first = node * per_node
            args += [
                "-object", "memory-backend-ram,id=ram%d,size=%dM" % (
                    node, megabytes // self.numa_nodes),
                "-numa", "node,nodeid=%d,cpus=%d-%d,memdev=ram%d" % (
                    node, first, first + per_node - 1, node),
            ]
        return args

    def pause(self):
        self.qmp.execute("stop")
        status = self.qmp.status()
        if status != "paused":
            raise RuntimeError("guest did not pause (status %s)" % status)
        return status

    def resume(self):
        self.qmp.execute("cont")
        status = self.qmp.status()
        if status != "running":
            raise RuntimeError("guest did not resume (status %s)" % status)
        return status

    def close(self):
        for closer in (self.serial, self.qmp):
            if closer is not None:
                try:
                    closer.close()
                except Exception:
                    pass
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
        if self._socket_dir is not None:
            shutil.rmtree(self._socket_dir, ignore_errors=True)


# Each mechanism named by the string it actually prints.  "It broke" is not a
# finding; which of these spoke up is.
COMPLAINTS = [
    ("soft-lockup detector",
     re.compile(r"BUG: soft lockup - CPU#\d+ stuck for \d+s")),
    ("hard-lockup detector",
     re.compile(r"watchdog: Watchdog detected hard LOCKUP|NMI watchdog: Watchdog detected hard LOCKUP")),
    ("RCU stall detector",
     re.compile(r"INFO: rcu_\w+ (?:self-)?detected (?:expedited )?stall|INFO: rcu_\w+ detected (?:expedited )?stalls")),
    ("hung-task detector",
     re.compile(r"INFO: task .{1,40}:\d+ blocked for more than \d+ seconds")),
    ("workqueue watchdog",
     re.compile(r"BUG: workqueue lockup")),
    ("clocksource watchdog",
     re.compile(r"clocksource: .{0,80}Marking clocksource '[^']+' as unstable|clocksource: timekeeping watchdog on CPU\d+")),
    ("hrtimer overrun report",
     re.compile(r"hrtimer: interrupt took \d+ ns")),
    ("kernel panic",
     re.compile(r"Kernel panic - not syncing")),
]


def complaints_in(text):
    """[(mechanism, the line it printed)] for every watchdog that spoke up."""
    found = []
    for name, pattern in COMPLAINTS:
        for match in pattern.finditer(text):
            start = text.rfind("\n", 0, match.start()) + 1
            found.append((name, text[start:].split("\n", 1)[0].strip()))
    return found
