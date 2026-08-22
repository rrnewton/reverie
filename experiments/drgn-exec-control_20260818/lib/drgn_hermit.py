#!/usr/bin/env python3
"""Read a paused Hermit/QEMU Linux guest through drgn without ptrace attach.

The helper restores the phase-5 QEMU snapshot with its CPUs paused.  Hermit's
exact tracer thread group is then stopped while QEMU is in ptrace-stop, and
guest physical reads are served from QEMU's RAM mmap through
``/proc/<qemu-pid>/mem``.  Only :meth:`advance` resumes the tracer and guest.
"""

from contextlib import contextmanager
from dataclasses import dataclass
import json
import lzma
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time
from typing import Iterator, Optional, Tuple


XZ_MAGIC = b"\xfd7zXZ\x00"
ELF_MAGIC = b"\x7fELF"
VMCOREINFO_MARKER = b"OSRELEASE="


@dataclass(frozen=True)
class GuestConfig:
    root: Path
    hermit: Path
    qemu: Path
    kernel: Path
    initrd: Path
    vmlinux: Path
    snapshot_disk: Path
    snapshot_name: str
    artifact_dir: Path
    qemu_bios: Optional[Path] = None
    qemu_library_path: Optional[Path] = None
    timeout: float = 240.0
    ram_bytes: int = 512 << 20
    # Execution control.  When gdbserver_port is set the guest runs under
    # `hermit run --gdbserver`, a gdb client attaches before the guest starts,
    # and the guest is stopped mid-execution at the `stop_at_writev`-th guest
    # console byte.  Zero disables the mid-execution stop.
    gdbserver_port: Optional[int] = None
    gdb_binary: str = "/usr/bin/gdb"
    stop_at_writev: int = 0
    # Demo 7's scheduler flags, overridable so the gdbserver deadlock can be
    # bisected against them.
    scheduler_flags: Tuple[str, ...] = (
        "--strict",
        "--no-rcb-time",
        "--target-timeslice", "100000",
        "--max-timeslice", "disabled",
    )


@dataclass(frozen=True)
class ObservationMetrics:
    physical_reads: int
    physical_bytes: int
    qemu_state: str
    tracer_state: str
    serial_bytes_delta: int


class QmpClient:
    """Small synchronous QMP client tolerant of interleaved events."""

    def __init__(self, connection: socket.socket) -> None:
        self.connection = connection
        self.stream = connection.makefile("rwb", buffering=0)
        greeting = self._read_message()
        if "QMP" not in greeting:
            raise RuntimeError("QMP greeting was not received")
        self.execute("qmp_capabilities")

    @classmethod
    def connect(
        cls, path: Path, process: subprocess.Popen, timeout: float
    ) -> "QmpClient":
        deadline = time.monotonic() + timeout
        last_error = None
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(
                    "Hermit exited before QMP connected (status {})".format(
                        process.returncode
                    )
                )
            connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            connection.settimeout(min(5.0, timeout))
            try:
                connection.connect(str(path))
                return cls(connection)
            # BlockingIOError (EAGAIN) shows up on the execution-control path:
            # QEMU runs far slower under the gdbserver, so the socket exists
            # before QEMU is accepting on it.
            except (
                FileNotFoundError,
                ConnectionRefusedError,
                BlockingIOError,
                socket.timeout,
            ) as error:
                last_error = error
                connection.close()
                time.sleep(0.05)
        raise TimeoutError("QMP socket did not become ready: {} ({})".format(path, last_error))

    def _read_message(self):
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("QMP disconnected")
            try:
                return json.loads(line.decode("utf-8"))
            except json.JSONDecodeError:
                continue

    def execute(self, command: str, arguments=None):
        request = {"execute": command}
        if arguments:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        while True:
            response = self._read_message()
            if "event" in response:
                continue
            if "error" in response:
                raise RuntimeError("QMP {} failed: {}".format(command, response["error"]))
            if "return" in response:
                return response["return"]

    def status(self) -> str:
        result = self.execute("query-status")
        return str(result.get("status", ""))

    def close(self) -> None:
        try:
            self.stream.close()
        finally:
            self.connection.close()


def _atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".vmlinux.", dir=str(path.parent))
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def ensure_vmlinux(kernel: Path, vmlinux: Path) -> Path:
    """Extract the ELF-with-BTF payload from the fixed XZ bzImage if needed."""
    if vmlinux.is_file():
        with vmlinux.open("rb") as source:
            if source.read(4) == ELF_MAGIC:
                return vmlinux
        raise RuntimeError("cached vmlinux is not an ELF file: {}".format(vmlinux))

    compressed = kernel.read_bytes()
    offset = compressed.find(XZ_MAGIC)
    if offset < 0:
        raise RuntimeError(
            "kernel has no XZ-compressed vmlinux; set DEMO07_VMLINUX to matching debug info"
        )
    try:
        extracted = lzma.decompress(compressed[offset:])
    except lzma.LZMAError as error:
        raise RuntimeError("could not extract vmlinux from {}: {}".format(kernel, error))
    if not extracted.startswith(ELF_MAGIC):
        raise RuntimeError("extracted kernel payload is not ELF")
    _atomic_write(vmlinux, extracted)
    return vmlinux


def _elf_build_id(path: Path) -> str:
    try:
        result = subprocess.run(
            ["readelf", "-n", str(path)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError:
        raise RuntimeError("readelf is required to verify the kernel BuildID")
    except subprocess.CalledProcessError as error:
        raise RuntimeError("readelf failed for {}: {}".format(path, error.stderr.strip()))
    match = re.search(r"Build ID:\s*([0-9a-fA-F]+)", result.stdout)
    if match is None:
        raise RuntimeError("vmlinux has no GNU BuildID: {}".format(path))
    return match.group(1).lower()


def _register_vmcoreinfo_symbols(program, vmcoreinfo: bytes, drgn) -> None:
    """Expose runtime kernel symbols recorded in the guest's VMCOREINFO."""
    symbols = tuple(
        drgn.Symbol(
            match.group(1).decode("ascii"),
            int(match.group(2), 16),
            1,
            drgn.SymbolBinding.GLOBAL,
            drgn.SymbolKind.UNKNOWN,
        )
        for match in re.finditer(
            rb"(?m)^SYMBOL\(([^)]+)\)=([0-9a-fA-F]+)$", vmcoreinfo
        )
    )
    if not symbols:
        raise RuntimeError("guest VMCOREINFO contains no SYMBOL entries")

    def find_symbols(_program, name, address, one):
        matches = [
            symbol
            for symbol in symbols
            if (name is None or symbol.name == name)
            and (
                address is None
                or symbol.address <= address < symbol.address + symbol.size
            )
        ]
        return matches[:1] if one else matches

    program.register_symbol_finder(
        "guest-vmcoreinfo", find_symbols, enable_index=0
    )


def _btf_dwarf_cache(vmlinux: Path, artifact_dir: Path) -> Path:
    """Convert the public kernel BTF to DWARF for drgn builds without BTF."""
    build_id = _elf_build_id(vmlinux)
    cache_dir = artifact_dir / "type-cache"
    cache_dir.mkdir(parents=True, exist_ok=True)
    output = cache_dir / "vmlinux-types-{}.o".format(build_id)
    if output.is_file():
        with output.open("rb") as cached:
            if cached.read(4) == ELF_MAGIC:
                return output
        raise RuntimeError("cached drgn type object is not ELF: {}".format(output))

    bpftool = shutil.which("bpftool")
    compiler = shutil.which("gcc")
    if bpftool is None or compiler is None:
        raise RuntimeError(
            "bpftool and gcc are required to bridge kernel BTF into public drgn"
        )

    descriptor, source_name = tempfile.mkstemp(
        prefix=".vmlinux-types.", suffix=".c", dir=str(cache_dir)
    )
    os.close(descriptor)
    source = Path(source_name)
    temporary_output = source.with_suffix(".o")
    try:
        with source.open("wb") as generated:
            dumped = subprocess.run(
                [bpftool, "btf", "dump", "file", str(vmlinux), "format", "c"],
                stdout=generated,
                stderr=subprocess.PIPE,
            )
        if dumped.returncode != 0:
            raise RuntimeError(
                "bpftool could not decode kernel BTF: {}".format(
                    dumped.stderr.decode("utf-8", "replace").strip()
                )
            )
        with source.open("ab") as generated:
            generated.write(b"\nstruct task_struct demo07_init_task_type;\n")
        compiled = subprocess.run(
            [
                compiler,
                "-w",
                "-g",
                "-gdwarf-4",
                "-fno-eliminate-unused-debug-types",
                "-c",
                str(source),
                "-o",
                str(temporary_output),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if compiled.returncode != 0:
            raise RuntimeError(
                "gcc could not compile BTF-derived types: {}".format(
                    compiled.stderr.decode("utf-8", "replace").strip()
                )
            )
        os.chmod(temporary_output, 0o644)
        os.replace(temporary_output, output)
        return output
    finally:
        for temporary in (source, temporary_output):
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def _load_btf_dwarf(program, vmlinux: Path, artifact_dir: Path) -> None:
    debug_object = _btf_dwarf_cache(vmlinux, artifact_dir)
    module = program.extra_module(str(debug_object), create=True)
    # An extra module needs a nonempty range even though only its types are used.
    module.address_range = (0, 1)
    module.try_file(str(debug_object), force=True)
    program.load_module_debug_info(module)


def _vmcoreinfo_symbol_addresses(vmcoreinfo: bytes) -> dict[str, int]:
    return {
        match.group(1).decode("ascii"): int(match.group(2), 16)
        for match in re.finditer(
            rb"(?m)^SYMBOL\(([^)]+)\)=([0-9a-fA-F]+)$", vmcoreinfo
        )
    }


def _kallsyms_address(program, vmcoreinfo: bytes, wanted: str) -> int:
    """Resolve one symbol from the stopped guest's compressed kallsyms table."""
    addresses = _vmcoreinfo_symbol_addresses(vmcoreinfo)
    required = (
        "kallsyms_names",
        "kallsyms_num_syms",
        "kallsyms_token_table",
        "kallsyms_token_index",
        "kallsyms_offsets",
        "kallsyms_relative_base",
    )
    missing = [name for name in required if name not in addresses]
    if missing:
        raise RuntimeError(
            "guest VMCOREINFO lacks kallsyms metadata: {}".format(", ".join(missing))
        )

    number = struct.unpack(
        "<I", program.read(addresses["kallsyms_num_syms"], 4)
    )[0]
    if not 0 < number <= 4_000_000:
        raise RuntimeError("invalid guest kallsyms count: {}".format(number))

    names_size = addresses["kallsyms_token_table"] - addresses["kallsyms_names"]
    token_table_size = (
        addresses["kallsyms_token_index"] - addresses["kallsyms_token_table"]
    )
    if not 0 < names_size <= 64 << 20 or not 0 < token_table_size <= 1 << 20:
        raise RuntimeError("invalid guest kallsyms table layout")

    names = program.read(addresses["kallsyms_names"], names_size)
    token_table = program.read(addresses["kallsyms_token_table"], token_table_size)
    token_index = struct.unpack(
        "<256H", program.read(addresses["kallsyms_token_index"], 512)
    )
    offsets = struct.unpack(
        "<{}i".format(number),
        program.read(addresses["kallsyms_offsets"], number * 4),
    )
    relative_base = struct.unpack(
        "<Q", program.read(addresses["kallsyms_relative_base"], 8)
    )[0]

    position = 0
    for index in range(number):
        if position >= len(names):
            raise RuntimeError("guest kallsyms names table ended early")
        length = names[position]
        position += 1
        if length & 0x80:
            if position >= len(names):
                raise RuntimeError("guest kallsyms name length is truncated")
            length = (length & 0x7F) | (names[position] << 7)
            position += 1
        if position + length > len(names):
            raise RuntimeError("guest kallsyms name is truncated")

        expanded = bytearray()
        for token in names[position : position + length]:
            start = token_index[token]
            end = token_table.find(b"\0", start)
            if end < 0:
                raise RuntimeError("guest kallsyms token table is truncated")
            expanded.extend(token_table[start:end])
        position += length
        # The first expanded character is the kallsyms type code.
        if expanded[1:].decode("ascii", "replace") == wanted:
            offset = offsets[index]
            return (
                relative_base + offset
                if offset >= 0
                else relative_base - 1 - offset
            )
    raise RuntimeError("guest kallsyms has no {} symbol".format(wanted))


def _register_init_task_object(program, address: int, drgn) -> None:
    task_type = program.type("struct task_struct")

    def find_object(_program, name, _flags, _filename):
        if name == "init_task":
            return drgn.Object(program, task_type, address=address)
        return None

    program.register_object_finder(
        "guest-init-task", find_object, enable_index=0
    )


def _proc_status_value(pid: int, key: str) -> str:
    with open("/proc/{}/status".format(pid)) as status:
        for line in status:
            if line.startswith(key + ":"):
                return line.split()[1]
    raise RuntimeError("no {} in /proc/{}/status".format(key, pid))


def _proc_state(pid: int) -> str:
    return _proc_status_value(pid, "State")


def _find_qemu(process_group: int) -> Optional[int]:
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        try:
            with open("/proc/{}/stat".format(pid)) as stat_file:
                fields = stat_file.read().rsplit(") ", 1)[1].split()
            if int(fields[2]) != process_group:
                continue
            with open("/proc/{}/comm".format(pid)) as comm_file:
                comm = comm_file.read().strip()
            if comm.startswith("qemu-system") or comm == "qemu-kvm":
                return pid
        except (OSError, IndexError, ValueError):
            continue
    return None


def _wait_for_qemu(process: subprocess.Popen, process_group: int, timeout: float) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(
                "Hermit exited before QEMU appeared (status {})".format(process.returncode)
            )
        qemu_pid = _find_qemu(process_group)
        if qemu_pid is not None:
            return qemu_pid
        time.sleep(0.05)
    raise TimeoutError("QEMU child not found in Hermit's process group")


def _open_serial_pipe(
    base: Path, process: subprocess.Popen, timeout: float
) -> Tuple[int, int]:
    """Open a QEMU `-serial pipe:` FIFO pair (``<base>.in``/``<base>.out``).

    A connected unix-socket serial chardev keeps a host-timing poll-ready fd in
    QEMU's main loop and starves the -icount vCPU under `hermit --no-rcb-time`
    (the demo-5 boot bug). A pipe's input FIFO is poll-ready only while a command
    is queued, so QEMU's main loop blocks in poll() between commands. QEMU opens
    both ends O_RDWR at chardev init, so these opens do not block once QEMU is up
    (guaranteed by the preceding QMP connect). The read end is non-blocking so
    the advance loop can poll for guest liveness, matching the old socket's
    per-recv timeout.
    """
    read_fd = os.open(str(base) + ".out", os.O_RDONLY | os.O_NONBLOCK)
    deadline = time.monotonic() + timeout
    while True:
        try:
            write_fd = os.open(str(base) + ".in", os.O_WRONLY | os.O_NONBLOCK)
            return read_fd, write_fd
        except OSError as error:
            if process.poll() is not None:
                os.close(read_fd)
                raise RuntimeError(
                    "Hermit exited before serial pipe opened (status {})".format(
                        process.returncode
                    )
                )
            if time.monotonic() >= deadline:
                os.close(read_fd)
                raise TimeoutError(
                    "serial input pipe did not open: {}.in ({})".format(base, error)
                )
            time.sleep(0.05)


GDB_SENTINEL = "<<<HERMIT_EXEC_CONTROL_DONE>>>"


class GdbClient:
    """A long-lived gdb client attached to Hermit's gdbserver.

    Hermit's gdbstub accepts exactly one connection (server.rs) and its async
    interrupt handler is an empty match arm (session.rs), so gdb attaches once,
    before the guest starts, and every later stop has to come from a breakpoint
    installed during that first attach.
    """

    def __init__(self, gdb: str, binary: Path, port: int, log: Path) -> None:
        self.log = log.open("w")
        self.port = port
        self.process = subprocess.Popen(
            [gdb, "-q", "-nx", str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )

    def send(self, command: str) -> None:
        self.log.write("(gdb) {}\n".format(command))
        self.log.flush()
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def command(self, command: str, timeout: float = 120.0) -> str:
        self.send(command)
        self.send("echo \\n{}\\n".format(GDB_SENTINEL))
        deadline = time.monotonic() + timeout
        text = ""
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("gdb exited during {!r}:\n{}".format(command, text))
            chunk = os.read(self.process.stdout.fileno(), 65536).decode(
                "utf-8", "replace"
            )
            if not chunk:
                raise RuntimeError("gdb closed output during {!r}".format(command))
            self.log.write(chunk)
            self.log.flush()
            text += chunk
            if GDB_SENTINEL in text:
                return text
        raise TimeoutError("gdb command {!r} timed out:\n{}".format(command, text))

    def close(self) -> None:
        try:
            self.process.stdin.write("\nquit\n")
            self.process.stdin.flush()
            self.process.wait(timeout=10)
        except Exception:
            self.process.kill()
        finally:
            self.log.close()


def _wait_for_tcp_port(port: int, process: subprocess.Popen, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    target = ":{:04X} ".format(port)
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(
                "Hermit exited before the gdbserver port opened (status {})".format(
                    process.returncode
                )
            )
        try:
            with open("/proc/net/tcp") as handle:
                if target in handle.read():
                    return
        except OSError:
            pass
        time.sleep(0.05)
    raise TimeoutError("Hermit gdbserver port {} never opened".format(port))


def _tracer_tgid_of(qemu_pid: int) -> int:
    """Hermit's tracer thread group, without stopping it."""
    tracer_tid = int(_proc_status_value(qemu_pid, "TracerPid"))
    if tracer_tid == 0:
        raise RuntimeError("QEMU is not ptrace-traced by Hermit")
    return int(_proc_status_value(tracer_tid, "Tgid"))


def _freeze_exact_tracer(qemu_pid: int, timeout: float = 20.0) -> Tuple[int, int]:
    tracer_tid = int(_proc_status_value(qemu_pid, "TracerPid"))
    if tracer_tid == 0:
        raise RuntimeError("QEMU is not ptrace-traced by Hermit")
    tracer_tgid = int(_proc_status_value(tracer_tid, "Tgid"))
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _proc_state(qemu_pid) != "t":
            time.sleep(0.002)
            continue
        os.kill(tracer_tgid, signal.SIGSTOP)
        for _ in range(1000):
            if _proc_state(tracer_tgid) == "T":
                break
            time.sleep(0.001)
        if _proc_state(qemu_pid) == "t" and _proc_state(tracer_tgid) == "T":
            return tracer_tid, tracer_tgid
        os.kill(tracer_tgid, signal.SIGCONT)
    raise TimeoutError("could not freeze Hermit's exact tracer at a QEMU trace-stop")


def _ram_region(qemu_pid: int, wanted: int) -> Tuple[int, int]:
    candidates = []
    with open("/proc/{}/maps".format(qemu_pid)) as maps:
        for line in maps:
            fields = line.split()
            if "r" not in fields[1] or "w" not in fields[1]:
                continue
            first, last = (int(value, 16) for value in fields[0].split("-"))
            size = last - first
            if wanted // 2 <= size <= wanted * 2:
                candidates.append((abs(size - wanted), first, last))
    if not candidates:
        raise RuntimeError("could not identify the {} MiB QEMU RAM mmap".format(wanted >> 20))
    _, first, last = min(candidates)
    return first, last


def _scan_vmcoreinfo(
    descriptor: int, first: int, last: int, chunk: int = 8 << 20
) -> Tuple[int, bytes]:
    offset = first
    tail = b""
    while offset < last:
        count = min(chunk, last - offset)
        try:
            data = os.pread(descriptor, count, offset)
        except OSError:
            offset += 4096
            tail = b""
            continue
        if not data:
            offset += 4096
            tail = b""
            continue
        combined = tail + data
        search_from = 0
        while True:
            index = combined.find(VMCOREINFO_MARKER, search_from)
            if index < 0:
                break
            address = offset - len(tail) + index
            candidate = os.pread(descriptor, 65536, address).split(b"\0", 1)[0]
            if (
                re.match(rb"OSRELEASE=[0-9]", candidate)
                and b"\nPAGESIZE=" in candidate
                and b"\nSYMBOL(_stext)=" in candidate
            ):
                return address, candidate
            search_from = index + len(VMCOREINFO_MARKER)
        tail = data[-len(VMCOREINFO_MARKER) :]
        offset += len(data)
    raise RuntimeError("VMCOREINFO was not found in guest RAM")


class HermitGuestProgram:
    def __init__(self, config: GuestConfig) -> None:
        self.config = config
        self._process = None  # type: Optional[subprocess.Popen]
        self._process_group = None  # type: Optional[int]
        self._qemu_pid = None  # type: Optional[int]
        self._tracer_tgid = None  # type: Optional[int]
        self._memory = None  # type: Optional[int]
        self._qmp = None  # type: Optional[QmpClient]
        self._serial_read_fd = None  # type: Optional[int]
        self._serial_write_fd = None  # type: Optional[int]
        self._ram_first = 0
        self._ram_size = 0
        self._vmcoreinfo = b""
        self._vmlinux = None  # type: Optional[Path]
        self._frozen = False
        self._serial_bytes = 0
        self._reads = 0
        self._bytes = 0
        self._init_task_address = None  # type: Optional[int]
        self.metrics = []  # type: list[ObservationMetrics]
        self.run_dir = None  # type: Optional[Path]
        self.serial_log = None  # type: Optional[Path]
        self._gdb = None  # type: Optional[GdbClient]
        self.stop_flag = None  # type: Optional[Path]
        self.resume_flag = None  # type: Optional[Path]
        self._transcript = bytearray()

    def start(self) -> "HermitGuestProgram":
        for path in (
            self.config.hermit,
            self.config.qemu,
            self.config.kernel,
            self.config.initrd,
            self.config.snapshot_disk,
        ):
            if not path.is_file():
                raise FileNotFoundError(str(path))
        self._vmlinux = ensure_vmlinux(self.config.kernel, self.config.vmlinux)
        self.config.artifact_dir.mkdir(parents=True, exist_ok=True)
        self.run_dir = Path(tempfile.mkdtemp(prefix="run.", dir=str(self.config.artifact_dir)))
        self.serial_log = self.run_dir / "serial.log"
        hermit_log = self.run_dir / "hermit.log"
        qmp_socket = self.run_dir / "qmp.sock"
        # Bidirectional serial over a `-serial pipe:` FIFO pair, not a unix
        # socket: a socket chardev's poll fd starves the -icount vCPU under
        # `hermit --no-rcb-time` (the demo-5 boot bug). QEMU opens (does not
        # create) the FIFOs, so make them before launch.
        serial_pipe = self.run_dir / "serial"
        for suffix in (".in", ".out"):
            os.mkfifo(str(serial_pipe) + suffix)
        working_snapshot = self.run_dir / "snapshot.qcow2"
        shutil.copyfile(str(self.config.snapshot_disk), str(working_snapshot))

        qemu_command = [
            str(self.config.qemu),
            "-machine", "q35",
            "-accel", "tcg",
            "-cpu", "max",
            "-smp", "1",
            "-m", "512M",
            "-display", "none",
            "-monitor", "none",
            "-serial", "pipe:{}".format(serial_pipe),
            "-qmp", "unix:{},server=on,wait=off".format(qmp_socket),
            "-drive", "if=none,id=hermit-snapshot-store,file={},format=qcow2".format(
                working_snapshot
            ),
            "-loadvm", self.config.snapshot_name,
            "-S",
            "-icount", "shift=0,sleep=off",
            "-rtc", "base=2022-01-01T00:00:00,clock=vm",
            "-kernel", str(self.config.kernel),
            "-initrd", str(self.config.initrd),
            "-append", "console=ttyS0 reboot=t",
        ]
        if self.config.qemu_bios is not None:
            qemu_command[1:1] = ["-L", str(self.config.qemu_bios)]
        debugger = []
        if self.config.gdbserver_port is not None:
            debugger = ["--gdbserver", "--gdbserver-port",
                        str(self.config.gdbserver_port)]
        command = [
            str(self.config.hermit),
            "run",
        ] + debugger + list(self.config.scheduler_flags) + [
            "--",
        ] + qemu_command
        environment = os.environ.copy()
        environment["LC_ALL"] = "C"
        environment["TZ"] = "UTC"
        if self.config.qemu_library_path is not None:
            old_path = environment.get("LD_LIBRARY_PATH", "")
            environment["LD_LIBRARY_PATH"] = str(self.config.qemu_library_path) + (
                ":" + old_path if old_path else ""
            )

        with hermit_log.open("wb") as output:
            self._process = subprocess.Popen(
                command,
                cwd=str(self.config.root),
                env=environment,
                start_new_session=True,
                stdout=output,
                stderr=subprocess.STDOUT,
            )
        self._process_group = os.getpgid(self._process.pid)
        if self.config.gdbserver_port is not None:
            self._attach_debugger()
        self._qmp = QmpClient.connect(qmp_socket, self._process, self.config.timeout)
        self._serial_read_fd, self._serial_write_fd = _open_serial_pipe(
            serial_pipe, self._process, self.config.timeout
        )
        initial_status = self._qmp.status()
        if initial_status not in ("paused", "prelaunch"):
            self._qmp.execute("stop")
            if self._qmp.status() != "paused":
                raise RuntimeError("QEMU did not start with guest CPUs paused")

        self._qemu_pid = _wait_for_qemu(
            self._process, self._process_group, self.config.timeout
        )
        if self.config.gdbserver_port is None:
            _, self._tracer_tgid = _freeze_exact_tracer(self._qemu_pid)
        else:
            # SIGSTOPing the tracer thread group would also freeze the thread
            # serving the gdbserver socket.  Under execution control the guest
            # is held by QEMU's own paused vCPUs here and by a Hermit
            # breakpoint stop later, so the tracer must stay runnable.
            self._tracer_tgid = _tracer_tgid_of(self._qemu_pid)
        self._frozen = True
        first, last = _ram_region(self._qemu_pid, self.config.ram_bytes)
        self._ram_first = first
        self._ram_size = last - first
        self._memory = os.open("/proc/{}/mem".format(self._qemu_pid), os.O_RDONLY)
        _, self._vmcoreinfo = _scan_vmcoreinfo(self._memory, first, last)
        build_match = re.search(rb"(?m)^BUILD-ID=([0-9a-fA-F]+)$", self._vmcoreinfo)
        if build_match is None:
            raise RuntimeError("guest VMCOREINFO has no BuildID")
        guest_build_id = build_match.group(1).decode().lower()
        debug_build_id = _elf_build_id(self._vmlinux)
        if guest_build_id != debug_build_id:
            raise RuntimeError(
                "kernel/vmlinux BuildID mismatch: guest {} debug {}".format(
                    guest_build_id, debug_build_id
                )
            )
        return self

    def _attach_debugger(self) -> None:
        """Attach gdb and arm the mid-execution breakpoint.

        Hermit does not start the guest until a gdb client connects, so this
        runs before the QMP handshake.  The breakpoint has to be installed now:
        Hermit's gdbstub cannot be interrupted later and will not accept a
        second connection.

        The stop site is `writev`.  In this QEMU configuration writev carries
        guest console output and nothing else, one call per byte, so the
        breakpoint means "stop as the guest emits its next console byte".
        `write` would have been unusable at roughly 45k calls a second.
        """
        port = self.config.gdbserver_port
        assert port is not None and self.run_dir is not None
        _wait_for_tcp_port(port, self._process, 120.0)
        self.stop_flag = self.run_dir / "STOPPED"
        self.resume_flag = self.run_dir / "RESUME"
        client = GdbClient(
            self.config.gdb_binary, self.config.qemu, port, self.run_dir / "gdb.log"
        )
        self._gdb = client
        client.command("set confirm off")
        client.command("set pagination off")
        client.command("set debuginfod enabled off")
        # Resolve libraries from the host filesystem, not over the RSP link.
        client.command("set sysroot /")
        # libc is not mapped yet at the initial _start stop.
        client.command("set breakpoint pending on")
        client.command("target remote :{}".format(port), timeout=180.0)

        if self.config.stop_at_writev > 0:
            # gdb owns the counting and the handshake, because this process
            # cannot talk to gdb while the guest is running.  At the chosen
            # console byte gdb parks in `shell` until the harness has finished
            # its drgn read, which keeps QEMU in its Hermit ptrace-stop.
            client.command("set $console_bytes = 0")
            client.command("break writev")
            client.send("commands")
            client.send("silent")
            client.send("set $console_bytes = $console_bytes + 1")
            client.send("if $console_bytes == {}".format(self.config.stop_at_writev))
            client.send(
                "  shell touch {}; while [ ! -e {} ]; do sleep 0.05; done".format(
                    self.stop_flag, self.resume_flag
                )
            )
            client.send("end")
            client.send("continue")
            client.command("end")
        client.send("continue")

    @contextmanager
    def paused_mid_execution(self, dwell: float = 0.0):
        """Stop the running guest wherever it is, read it, then resume.

        This is Demo 7's own pause primitive -- SIGSTOP of Hermit's exact tracer
        thread group while QEMU sits in a ptrace-stop -- applied at an arbitrary
        moment during guest execution instead of only at a QMP boundary. The
        stopped tracer is the only process that can let QEMU run again, so the
        guest executes nothing for as long as we hold it.

        Unlike a breakpoint, where the stop lands is wall-clock dependent. What
        it costs the guest should still be nothing, because under QEMU
        `-icount` the guest's clock is derived from instructions retired, not
        from host time. That is the claim `dwell` exists to test.
        """
        if self._qemu_pid is None:
            raise RuntimeError("guest was not started")
        serial_before = self._serial_bytes
        _, self._tracer_tgid = _freeze_exact_tracer(self._qemu_pid)
        self._frozen = True
        try:
            qemu_state = _proc_state(self._qemu_pid)
            tracer_state = _proc_state(self._tracer_tgid)
            if qemu_state != "t" or tracer_state != "T":
                raise RuntimeError(
                    "guest did not come to rest: qemu={} tracer={}".format(
                        qemu_state, tracer_state
                    )
                )
            yield self._program()
            if dwell > 0:
                time.sleep(dwell)
            # Re-check after the read and the dwell: the guest must not have
            # moved at any point while we held it.
            qemu_state = _proc_state(self._qemu_pid)
            tracer_state = _proc_state(self._tracer_tgid)
            serial_delta = self._serial_bytes - serial_before
            self.metrics.append(
                ObservationMetrics(
                    physical_reads=self._reads,
                    physical_bytes=self._bytes,
                    qemu_state=qemu_state,
                    tracer_state=tracer_state,
                    serial_bytes_delta=serial_delta,
                )
            )
            if qemu_state != "t" or tracer_state != "T" or serial_delta != 0:
                raise RuntimeError(
                    "guest advanced while paused: qemu={} tracer={} serial_delta={}".format(
                        qemu_state, tracer_state, serial_delta
                    )
                )
        finally:
            os.kill(self._tracer_tgid, signal.SIGCONT)
            self._frozen = False

    def wait_for_execution_stop(self, timeout: Optional[float] = None) -> float:
        """Block until Hermit's breakpoint has stopped QEMU mid-execution."""
        if self.stop_flag is None:
            raise RuntimeError("no mid-execution breakpoint was armed")
        limit = self.config.timeout if timeout is None else timeout
        started = time.monotonic()
        deadline = started + limit
        while time.monotonic() < deadline:
            if self._process is not None and self._process.poll() is not None:
                raise RuntimeError("Hermit exited before the breakpoint was reached")
            if self.stop_flag.exists():
                return time.monotonic() - started
            time.sleep(0.02)
        raise TimeoutError("Hermit breakpoint stop was not reached")

    def release_execution_stop(self) -> None:
        """Let gdb resume the guest from the breakpoint stop."""
        if self.resume_flag is None:
            raise RuntimeError("no mid-execution breakpoint was armed")
        self.resume_flag.touch()

    @contextmanager
    def paused_observation(self):
        """A drgn read taken while QEMU's vCPUs are paused via QMP.

        Used for the reads either side of the advance. The invariant here is
        QMP `paused`, not a ptrace stop: the vCPUs execute nothing, so guest
        RAM is quiescent even though QEMU's own process may be running its
        main loop. Asserting a ptrace stop instead makes this read a race.
        """
        if self._qemu_pid is None or self._qmp is None:
            raise RuntimeError("guest was not started")
        serial_before = self._serial_bytes
        # `-S` leaves QEMU in `prelaunch` until the machine is first started,
        # and in `paused` after any later QMP stop. Both mean the vCPUs are
        # executing nothing, which is the invariant this read needs.
        stopped = ("paused", "prelaunch")
        status = self._qmp.status()
        if status not in stopped:
            raise RuntimeError("guest vCPUs are not paused: {}".format(status))
        self._frozen = True
        program = self._program()
        yield program
        status = self._qmp.status()
        serial_delta = self._serial_bytes - serial_before
        self.metrics.append(
            ObservationMetrics(
                physical_reads=self._reads,
                physical_bytes=self._bytes,
                qemu_state=_proc_state(self._qemu_pid),
                tracer_state=_proc_state(self._tracer_tgid),
                serial_bytes_delta=serial_delta,
            )
        )
        if status not in stopped or serial_delta != 0:
            raise RuntimeError(
                "guest advanced during read: status={} serial_delta={}".format(
                    status, serial_delta
                )
            )

    @contextmanager
    def stopped_observation(self):
        """A drgn read taken at a Hermit breakpoint stop.

        Demo 7's read requires Hermit's tracer thread group to be SIGSTOPped.
        That is impossible here -- the tracer serves the gdbserver.  The
        invariant that actually matters is unchanged and still checked: QEMU
        sits in a ptrace-stop, so it executes no instruction, and the guest
        console transcript does not grow across the read.
        """
        if self._qemu_pid is None:
            raise RuntimeError("guest was not started")
        serial_before = self._serial_bytes
        state_before = _proc_state(self._qemu_pid)
        if state_before != "t":
            raise RuntimeError(
                "QEMU is not in a ptrace-stop at the breakpoint: {}".format(
                    state_before
                )
            )
        self._frozen = True
        program = self._program()
        yield program
        qemu_state = _proc_state(self._qemu_pid)
        serial_delta = self._serial_bytes - serial_before
        self.metrics.append(
            ObservationMetrics(
                physical_reads=self._reads,
                physical_bytes=self._bytes,
                qemu_state=qemu_state,
                tracer_state=_proc_state(self._tracer_tgid),
                serial_bytes_delta=serial_delta,
            )
        )
        if qemu_state != "t" or serial_delta != 0:
            raise RuntimeError(
                "guest advanced during breakpoint read: qemu={} serial_delta={}".format(
                    qemu_state, serial_delta
                )
            )

    def _program(self):
        if not self._frozen or self._memory is None or self._vmlinux is None:
            raise RuntimeError("guest must be frozen before a drgn observation")
        self._reads = 0
        self._bytes = 0

        import drgn

        def read_physical(address, count, offset, physical):
            if not physical:
                raise ValueError("guest memory callback received a virtual read")
            data = os.pread(self._memory, count, self._ram_first + offset)
            if len(data) != count:
                raise OSError(
                    "short guest RAM read at {:#x}: {}/{}".format(
                        address, len(data), count
                    )
                )
            self._reads += 1
            self._bytes += count
            return data

        program = drgn.Program(drgn.Platform(drgn.Architecture.X86_64))
        program.add_memory_segment(0, self._ram_size, read_physical, physical=True)
        program.set_linux_kernel_custom(self._vmcoreinfo, True)
        _register_vmcoreinfo_symbols(program, self._vmcoreinfo, drgn)
        if "btf" not in program.registered_type_finders():
            # Public drgn 0.2.0 does not read BTF, while Meta's build does.
            # Convert the pinned public BTF to cached DWARF types on that path.
            _load_btf_dwarf(program, self._vmlinux, self.config.artifact_dir)
        else:
            try:
                program.load_debug_info([str(self._vmlinux)], main=True)
            except drgn.MissingDebugInfoError:
                _load_btf_dwarf(program, self._vmlinux, self.config.artifact_dir)
        if self._init_task_address is None:
            self._init_task_address = _kallsyms_address(
                program, self._vmcoreinfo, "init_task"
            )
        _register_init_task_object(program, self._init_task_address, drgn)
        return program

    @contextmanager
    def observation(self):
        if self._qemu_pid is None or self._tracer_tgid is None:
            raise RuntimeError("guest was not started")
        serial_before = self._serial_bytes
        program = self._program()
        yield program
        qemu_state = _proc_state(self._qemu_pid)
        tracer_state = _proc_state(self._tracer_tgid)
        serial_delta = self._serial_bytes - serial_before
        current = ObservationMetrics(
            physical_reads=self._reads,
            physical_bytes=self._bytes,
            qemu_state=qemu_state,
            tracer_state=tracer_state,
            serial_bytes_delta=serial_delta,
        )
        self.metrics.append(current)
        if qemu_state != "t" or tracer_state != "T" or serial_delta != 0:
            raise RuntimeError(
                "guest advanced during read: qemu={} tracer={} serial_delta={}".format(
                    qemu_state, tracer_state, serial_delta
                )
            )

    def _pump_serial(self) -> int:
        """Drain whatever the guest has emitted so far into the transcript.

        Split out of _wait_for_serial so a mid-execution pause can observe the
        guest's progress without consuming bytes the marker wait still needs.
        The accumulated transcript lives on the instance, not in a local.
        """
        if (
            self._serial_read_fd is None
            or self.serial_log is None
            or self._process is None
        ):
            raise RuntimeError("serial transport is unavailable")
        if self._process.poll() is not None:
            raise RuntimeError(
                "Hermit exited during deterministic advance (status {})".format(
                    self._process.returncode
                )
            )
        try:
            chunk = os.read(self._serial_read_fd, 65536)
        except BlockingIOError:
            # No guest output right now; QEMU still holds the pipe's write end
            # open (O_RDWR), so this is a wait, not EOF.
            return 0
        if not chunk:
            raise RuntimeError("guest serial disconnected during advance")
        self._transcript.extend(chunk)
        self._serial_bytes += len(chunk)
        with self.serial_log.open("ab") as output:
            output.write(chunk)
        return len(chunk)

    def wait_for_serial_bytes(self, count: int, timeout: Optional[float] = None) -> int:
        """Wait until the guest has emitted at least `count` bytes this advance."""
        limit = self.config.timeout if timeout is None else timeout
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            if len(self._transcript) >= count:
                return len(self._transcript)
            if self._pump_serial() == 0:
                time.sleep(0.005)
        raise TimeoutError(
            "guest emitted {} of {} serial bytes".format(len(self._transcript), count)
        )

    def _wait_for_serial(self, marker: bytes) -> None:
        deadline = time.monotonic() + self.config.timeout
        while time.monotonic() < deadline:
            if marker in self._transcript:
                return
            if self._pump_serial() == 0:
                time.sleep(0.02)
        raise TimeoutError("guest advance marker was not seen")

    def advance(self, command: str, marker: bytes) -> None:
        """Run one fixed guest command interval, then freeze at its marker."""
        if not self._frozen or self._serial_write_fd is None or self._qmp is None:
            raise RuntimeError("guest is not ready for deterministic advance")
        if self._tracer_tgid is None or self._qemu_pid is None:
            raise RuntimeError("traced processes are unavailable")
        if b"\n" in marker or "\n" in command or "\r" in command:
            raise ValueError("advance command and marker must each be one line")

        self.begin_advance(command)
        self.finish_advance(marker)

    def begin_advance(self, command: str) -> None:
        """Queue the fixed guest command and let the guest run."""
        if not self._frozen or self._serial_write_fd is None or self._qmp is None:
            raise RuntimeError("guest is not ready for deterministic advance")
        if self._tracer_tgid is None or self._qemu_pid is None:
            raise RuntimeError("traced processes are unavailable")
        if "\n" in command or "\r" in command:
            raise ValueError("advance command must be one line")
        # Queue the deterministic input while the tracee and tracer are frozen.
        # Once the tracer resumes, QEMU can accept it but the vCPU remains paused
        # until the explicit QMP cont below.
        os.write(self._serial_write_fd, command.encode("utf-8") + b"\n")
        if self.config.gdbserver_port is None:
            os.kill(self._tracer_tgid, signal.SIGCONT)
        self._frozen = False
        self._qmp.execute("cont")

    def finish_advance(self, marker: bytes) -> None:
        """Wait for the guest's completion marker and park the guest again."""
        if self._qmp is None or self._qemu_pid is None:
            raise RuntimeError("guest is not running an advance")
        if b"\n" in marker:
            raise ValueError("advance marker must be one line")
        self._wait_for_serial(marker)
        self._qmp.execute("stop")
        if self._qmp.status() != "paused":
            raise RuntimeError("QEMU did not pause after deterministic advance")
        if self.config.gdbserver_port is None:
            _, self._tracer_tgid = _freeze_exact_tracer(self._qemu_pid)
        self._frozen = True

    def close(self) -> None:
        if self._gdb is not None:
            # Never leave gdb parked in the breakpoint handshake.
            if self.resume_flag is not None:
                try:
                    self.resume_flag.touch()
                except OSError:
                    pass
            try:
                self._gdb.close()
            except Exception:
                pass
            self._gdb = None
        if self._memory is not None:
            os.close(self._memory)
            self._memory = None
        if self._qmp is not None:
            try:
                self._qmp.close()
            except OSError:
                pass
            self._qmp = None
        for attribute in ("_serial_read_fd", "_serial_write_fd"):
            descriptor = getattr(self, attribute)
            if descriptor is not None:
                try:
                    os.close(descriptor)
                except OSError:
                    pass
                setattr(self, attribute, None)
        if self._process_group is not None:
            try:
                os.killpg(self._process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
        if self._process is not None:
            try:
                self._process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass


@contextmanager
def program_from_hermit(config: GuestConfig) -> Iterator[HermitGuestProgram]:
    """Yield a restored snapshot guest, initially frozen for observation."""
    guest = HermitGuestProgram(config)
    try:
        yield guest.start()
    finally:
        guest.close()
