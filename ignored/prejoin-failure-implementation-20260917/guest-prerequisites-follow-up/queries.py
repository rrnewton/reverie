#!/usr/bin/python3
"""Bounded read-only compiler/ELF discovery. Never link or execute a guest."""
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parent
ENV = json.loads((ROOT / "environment.json").read_text())
QUERY_ROOT = ROOT / "queries"


def limits():
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_AS, (1024**3, 1024**3))
    resource.setrlimit(resource.RLIMIT_FSIZE, (1024**2, 1024**2))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def query(name, argv, expected=(0,)):
    directory = QUERY_ROOT / name
    directory.mkdir(parents=True, exist_ok=False)
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.monotonic()
    with (directory / "stdout").open("xb") as stdout, (directory / "stderr").open("xb") as stderr:
        process = subprocess.Popen(
            argv, cwd=ROOT, env=ENV, stdin=subprocess.DEVNULL,
            stdout=stdout, stderr=stderr, start_new_session=True, preexec_fn=limits,
        )
        timed_out = False
        try:
            status = process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            status = process.wait(timeout=5)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    streams = {}
    for stream in ["stdout", "stderr"]:
        path = directory / stream
        data = path.read_bytes()
        streams[stream] = {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    record = {
        "name": name, "argv": argv, "cwd": str(ROOT), "pid": process.pid,
        "exit_status": status, "timed_out": timed_out,
        "wall_seconds": time.monotonic() - start,
        "cpu_seconds": after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
        "bounds": {"cpu_seconds_per_process": 5, "wall_seconds": 15, "address_space_bytes": 1024**3,
                   "file_bytes_per_stream": 1024**2, "core_bytes": 0},
        "environment_sha256": hashlib.sha256((ROOT / "environment.json").read_bytes()).hexdigest(),
        "streams": streams,
    }
    (directory / "result.json").write_text(json.dumps(record, indent=2) + "\n")
    if timed_out or status not in expected:
        raise RuntimeError(f"read-only query {name} failed with status {status}")
    return (directory / "stdout").read_text(), (directory / "stderr").read_text()


def main():
    query("cc-original-empty-input-selection", ["/usr/bin/cc", "-x", "c", "-fsyntax-only", "-"])
    for name, flags in [
        ("version", ["-v"]), ("machine", ["-dumpmachine"]), ("fullversion", ["-dumpfullversion"]),
        ("search-dirs", ["-print-search-dirs"]), ("sysroot", ["-print-sysroot"]),
        ("specs", ["-dumpspecs"]),
    ]:
        query("cc-" + name, ["/usr/bin/cc", *flags])
    for program in ["cc1", "collect2", "ld", "as", "lto-wrapper"]:
        query("cc-program-" + program, ["/usr/bin/cc", "-print-prog-name=" + program])
    for filename in ["specs", "crt1.o", "Scrt1.o", "crti.o", "crtn.o", "crtbegin.o", "crtbeginS.o",
                     "crtend.o", "crtendS.o", "libc.so", "libc_nonshared.a", "libpthread.so", "libpthread.a",
                     "libgcc.a", "libgcc_s.so", "libgcc_s.so.1", "liblto_plugin.so"]:
        query("cc-file-" + filename, ["/usr/bin/cc", "-print-file-name=" + filename])
    query("ld-cache", ["/usr/sbin/ldconfig", "-p"])
    for fixture in json.loads((ROOT / "fixtures.json").read_text()):
        flags = fixture["flags"]
        source = fixture["copy"]
        # -### prints subprocess/link commands only. The named output must remain absent.
        output = ROOT / ("NOT-BUILT-" + fixture["name"])
        query(fixture["name"] + "-driver-dry-run", ["/usr/bin/cc", "-###", *flags, source, "-o", str(output)])
        assert not output.exists()
        # Dependency-only preprocessing emits the actual header closure to stdout.
        query(fixture["name"] + "-header-dependencies", ["/usr/bin/cc", *flags, "-M", source])


if __name__ == "__main__":
    main()
