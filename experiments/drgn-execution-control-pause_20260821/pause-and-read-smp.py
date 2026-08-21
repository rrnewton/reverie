#!/usr/bin/env python3
"""The capability the owner asked for: pause a running Linux VM at a RANDOM
point, read real kernel state out of it with drgn, and unpause it -- all at
the QEMU level, with no drgn execution-control API involved.

The pause is QMP `stop`; the read is drgn over a physical-memory callback into
/proc/<qemu-pid>/mem; the unpause is QMP `cont`.  Reused wholesale from
demos/lib/drgn_hermit.py: vmlinux extraction, the RAM-mapping search, the
VMCOREINFO scan, the kallsyms lookup and the init_task registration.  That
file is imported, never modified.
"""

import argparse
import json
import os
from pathlib import Path
import random
import struct
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = Path("/home/newton/work/dev-hermit")
sys.path.insert(0, str(ROOT / "demos" / "lib"))
sys.path.insert(0, str(HERE))

import drgn  # noqa: E402
from drgn.helpers.linux.list import list_for_each_entry  # noqa: E402

import drgn_hermit as dh  # noqa: E402
from guest_smp import Guest, ASSETS, KERNEL, complaints_in  # noqa: E402

WORKLOAD = (
    "n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
    "for n in 1 2 3; do (sleep 4000)& done; "
    "echo HB_START; "
    "while :; do echo \"HB $(cut -d' ' -f1 /proc/uptime) $(date -u +%s)\"; sleep 1; done"
)

# include/linux/sched.h TASK_* bits, in the order /proc reports them.
_TASK_NOLOAD = 0x0400
_STATE_BITS = [(0x0001, "S"), (0x0002, "D"), (0x0004, "T"), (0x0008, "t"),
               (0x0010, "X"), (0x0020, "Z"), (0x0040, "P")]


def _state_char(state):
    if state == 0:
        return "R"
    # TASK_IDLE is TASK_UNINTERRUPTIBLE|TASK_NOLOAD.  Test it before plain D or
    # every idle kworker is misreported as uninterruptible.
    if state & _TASK_NOLOAD:
        return "I"
    for bit, char in _STATE_BITS:
        if state & bit:
            return char
    return "state:%#x" % state


def _ram_regions(qemu_pid, per_node, nodes):
    """Every host mapping that looks like one node's worth of guest RAM."""
    candidates = []
    with open("/proc/%d/maps" % qemu_pid) as maps:
        for line in maps:
            fields = line.split()
            if "r" not in fields[1] or "w" not in fields[1]:
                continue
            first, last = (int(v, 16) for v in fields[0].split("-"))
            if per_node <= last - first <= per_node + (2 << 20):
                candidates.append((first, last))
    if len(candidates) < nodes:
        raise RuntimeError("found %d guest RAM mapping(s) of ~%d MiB, wanted %d"
                           % (len(candidates), per_node >> 20, nodes))
    return candidates[:nodes] if nodes > 1 else candidates[:1]


def _orderings(regions):
    """Candidate node orderings, address order first, then reversed."""
    if len(regions) == 1:
        return [regions]
    ascending = sorted(regions)
    return [ascending, list(reversed(ascending))]


def _check_kernel_present(fd, node0_host_base, vmcoreinfo):
    """Confirm this mapping really is guest-physical node 0.

    Reads kallsyms_num_syms through the candidate mapping and requires a
    plausible symbol count. The wrong mapping returns 0.
    """
    addresses = dh._vmcoreinfo_symbol_addresses(vmcoreinfo)
    virtual = addresses.get("kallsyms_num_syms")
    if virtual is None:
        raise RuntimeError("VMCOREINFO has no kallsyms_num_syms symbol")
    physical = virtual - 0xFFFFFFFF80000000
    count = struct.unpack("<I", os.pread(fd, 4, node0_host_base + physical))[0]
    if not 1000 < count < 10_000_000:
        raise RuntimeError(
            "mapping at %#x is not guest node 0: kallsyms_num_syms reads %d"
            % (node0_host_base, count))
    return count


class Inspector:
    """drgn against a paused QEMU's guest RAM.

    A NUMA guest has one host mapping PER NODE, not one for the whole guest,
    so the single-region assumption in demos/lib/drgn_hermit.py does not hold
    here: with `-numa` each `memory-backend-ram` is its own mmap and guest
    physical memory is split across them. Find them all, and work out which
    host mapping backs which guest-physical range by testing rather than by
    trusting /proc/<pid>/maps order -- a wrong assignment resolves no symbols,
    so the check is self-validating.
    """

    def __init__(self, qemu_pid, vmlinux, artifact_dir, ram_bytes=512 << 20,
                 nodes=1):
        self.vmlinux = Path(vmlinux)
        self.artifact_dir = Path(artifact_dir)
        self.fd = os.open("/proc/%d/mem" % qemu_pid, os.O_RDONLY)
        per_node = ram_bytes // nodes
        regions = _ram_regions(qemu_pid, per_node, nodes)

        # Node 0 holds low memory and therefore the kernel image. Try each
        # candidate ordering and keep the one that actually reads back the
        # kernel.
        #
        # A VMCOREINFO-shaped blob is present in BOTH mappings on this host,
        # so "did VMCOREINFO parse" does not discriminate between them; the
        # first version of this check accepted the wrong mapping on that
        # basis. Verified on a live 2-node guest: QEMU backs guest-physical
        # node 0 with the HIGHER host mapping, so address order is exactly
        # the wrong guess. The test below reads kallsyms_num_syms out of the
        # candidate and requires a sane count, which the wrong ordering fails
        # by returning 0.
        debug_build = dh._elf_build_id(self.vmlinux)
        last_error = None
        for ordering in _orderings(regions):
            try:
                first, last = ordering[0]
                _, vmcoreinfo = dh._scan_vmcoreinfo(self.fd, first, last)
                guest_build = dh.re.search(
                    rb"(?m)^BUILD-ID=([0-9a-fA-F]+)$", vmcoreinfo)
                if guest_build is None:
                    raise RuntimeError("guest VMCOREINFO carries no BuildID")
                if guest_build.group(1).decode().lower() != debug_build:
                    raise RuntimeError(
                        "kernel/vmlinux BuildID mismatch: guest %s debug %s"
                        % (guest_build.group(1).decode(), debug_build))
                _check_kernel_present(self.fd, first, vmcoreinfo)
                self.regions = ordering
                self.vmcoreinfo = vmcoreinfo
                break
            except Exception as error:
                last_error = error
        else:
            raise RuntimeError("no ordering of %d guest RAM mapping(s) yielded "
                               "a usable VMCOREINFO: %s" % (len(regions), last_error))

        self.first = self.regions[0][0]
        self.size = sum(last - first for first, last in self.regions)
        self.init_task_address = None
        self.reads = 0
        self.bytes = 0

    def program(self):
        self.reads = 0
        self.bytes = 0

        def read_physical(address, count, offset, physical):
            if not physical:
                raise ValueError("guest memory callback received a virtual read")
            out = b""
            want = count
            position = offset
            for first, last in self.regions:
                span = last - first
                if position >= span:
                    position -= span
                    continue
                take = min(want, span - position)
                chunk = os.pread(self.fd, take, first + position)
                if len(chunk) != take:
                    raise OSError("short guest RAM read at %#x" % address)
                out += chunk
                want -= take
                position = 0
                if not want:
                    break
            if want:
                raise OSError("guest read at %#x ran past the mapped nodes" % address)
            self.reads += 1
            self.bytes += count
            return out

        program = drgn.Program(drgn.Platform(drgn.Architecture.X86_64))
        program.add_memory_segment(0, self.size, read_physical, physical=True)
        program.set_linux_kernel_custom(self.vmcoreinfo, True)
        dh._register_vmcoreinfo_symbols(program, self.vmcoreinfo, drgn)
        if "btf" not in program.registered_type_finders():
            dh._load_btf_dwarf(program, self.vmlinux, self.artifact_dir)
        else:
            try:
                program.load_debug_info([str(self.vmlinux)], main=True)
            except drgn.MissingDebugInfoError:
                dh._load_btf_dwarf(program, self.vmlinux, self.artifact_dir)
        if self.init_task_address is None:
            self.init_task_address = dh._kallsyms_address(
                program, self.vmcoreinfo, "init_task")
        dh._register_init_task_object(program, self.init_task_address, drgn)
        return program

    def task_list(self):
        program = self.program()
        init_task = program["init_task"]

        # Read __state directly rather than through task_state_to_char: the
        # public-BTF type set on this box has no `task_state_array`, and the
        # bit meanings are stable kernel ABI anyway.
        def row(task):
            state = task.member_("__state").value_()
            # thread_info.cpu is the vCPU the task last ran on.  On a 4-vCPU
            # guest this is the column that shows the read is seeing SMP
            # state rather than a single runqueue.
            try:
                cpu = int(task.thread_info.cpu)
            except Exception:
                cpu = -1
            return (task.pid.value_(),
                    task.comm.string_().decode("utf-8", "replace"),
                    _state_char(state), cpu)

        rows = [row(init_task)]
        rows.extend(row(task) for task in list_for_each_entry(
            "struct task_struct", init_task.tasks.address_of_(), "tasks"))
        return rows

    def close(self):
        os.close(self.fd)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arm", default="kvm")
    parser.add_argument("--pauses", type=int, default=5)
    parser.add_argument("--hold", type=float, default=2.0,
                        help="extra wall seconds to hold beyond the drgn read")
    parser.add_argument("--cpus", type=int, default=4)
    parser.add_argument("--numa", type=int, default=2)
    parser.add_argument("--seed", type=int, default=20260821)
    parser.add_argument("--out", default=str(HERE / "results" / "pause-and-read.json"))
    args = parser.parse_args()

    rng = random.Random(args.seed)
    run_dir = HERE / "runs" / "pause-and-read" / args.arm
    artifacts = HERE / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    vmlinux = dh.ensure_vmlinux(KERNEL, ASSETS / "vmlinux")

    guest = Guest(args.arm, run_dir, WORKLOAD, cpus=args.cpus, numa_nodes=args.numa)
    session = {"arm": args.arm, "vmlinux": str(vmlinux), "pauses": []}
    try:
        guest.start(timeout=240)
        if not guest.serial.wait_for(b"HB_START", 120):
            raise TimeoutError("workload never started")
        guest.serial.pump(3.0)
        inspector = Inspector(guest.process.pid, vmlinux, artifacts,
                              ram_bytes=1024 << 20, nodes=args.numa)
        session["vmcoreinfo_osrelease"] = dh.re.search(
            rb"OSRELEASE=(\S+)", inspector.vmcoreinfo).group(1).decode()

        for index in range(1, args.pauses + 1):
            # A random point, chosen by us, in the guest's execution.
            gap = rng.uniform(1.5, 6.0)
            guest.serial.pump(gap)

            entry = {"n": index, "ran_for_seconds": round(gap, 2)}
            before_beats = len(guest.serial.text().split("HB ")) - 1
            started = time.monotonic()
            entry["status_at_pause"] = guest.pause()
            entry["qemu_state_while_paused"] = _proc_state(guest.process.pid)
            bytes_at_pause = len(guest.serial.buffer)

            read_started = time.monotonic()
            rows = inspector.task_list()
            entry["read_seconds"] = round(time.monotonic() - read_started, 2)
            entry["physical_reads"] = inspector.reads
            entry["physical_bytes"] = inspector.bytes
            entry["task_count"] = len(rows)
            entry["tasks"] = ["%d %s %s cpu%d" % r for r in rows]

            guest.serial.pump(args.hold)
            entry["bytes_emitted_while_paused"] = len(guest.serial.buffer) - bytes_at_pause
            entry["held_wall_seconds"] = round(time.monotonic() - started, 2)
            entry["status_at_resume"] = guest.resume()
            guest.serial.pump(2.5)
            entry["heartbeats_before"] = before_beats
            entry["heartbeats_after"] = len(guest.serial.text().split("HB ")) - 1
            session["pauses"].append(entry)
            print("pause %d: held %.2fs  tasks=%d  reads=%d (%.1f MiB)  "
                  "console bytes while paused=%d" % (
                      index, entry["held_wall_seconds"], entry["task_count"],
                      entry["physical_reads"], entry["physical_bytes"] / (1 << 20),
                      entry["bytes_emitted_while_paused"]), flush=True)

        guest.serial.pump(5.0)
        text = guest.serial.text()
        session["complaints"] = [{"mechanism": m, "line": l}
                                 for m, l in complaints_in(text)]
        beats = [float(b.split()[0]) for b in text.split("HB ")[1:]
                 if b.split() and b.split()[0].replace(".", "").isdigit()]
        steps = [b - a for a, b in zip(beats, beats[1:]) if b > a]
        session["max_guest_uptime_step_seconds"] = round(max(steps), 3) if steps else None
        session["ok"] = True
        inspector.close()
    except Exception as error:
        session["ok"] = False
        session["error"] = "%s: %s" % (type(error).__name__, error)
        if guest.serial is not None:
            session["tail"] = guest.serial.text()[-1500:]
        raise
    finally:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(json.dumps(session, indent=2))
        guest.close()


def _proc_state(pid):
    with open("/proc/%d/stat" % pid) as handle:
        return handle.read().rsplit(")", 1)[1].split()[0]


main()
