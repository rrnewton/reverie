#!/usr/bin/env python3
"""Re-derive the six restored cells on the observable the verdict reads.

Read-only: fixture sources come from a git blob at a pinned SHA, mutants are
compiled into this gitignored scratch dir, no manifest or product file is
touched.

Two measurements per binary, kept separate on purpose:

  OBSERVABLE  `hermit run --backend=ptrace --strict` (portable profile).  This
              passes the guest's own stdout AND stderr through, so it is where
              the bite is proven.  Recorded as (rc, stdout bytes, stderr bytes)
              with hermit's own log lines stripped.

  GATE        the exact replay-mode command emitted by
              scripts/manifest-to-commands.rs:269-271, and separately the exact
              verify-mode command from :258-267.  Only the exit status is read.

Anchors are asserted UNIQUE in the source, not first-match.
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

D = "/home/newton/work/dev-hermit/ignored/observable-rederive-20260815"
REPO = "/home/newton/work/dev-hermit/hermit"
H = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
SHA = "302a1a9c0fde564db0292d1ea5cabc91343e79bc"
CC = ["cc", "-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror"]
TIMEOUT_S = 90  # timeout_seconds from the manifest rows

DUMPABLE = "prctl(PR_GET_DUMPABLE, 0, 0, 0, 0)"

# (label, unique anchor, replacement, family, what the mutation does)
CELLS = {
    "backend-parity-c/aio-refusal": {
        "program": "tests/backend-parity/fixtures/aio_refusal.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             "syscall(SYS_io_submit, (unsigned long)0x1234, 0L, (void *)0)",
             "syscall(SYS_getppid, (unsigned long)0x1234, 0L, (void *)0)",
             "behaviour",
             "check 3 issues getppid (which Hermit permits) instead of io_submit, "
             "so the contracted ENOSYS refusal is not exercised"),
            ("M2-oracle",
             "long result = syscall(SYS_io_setup, 128, &ctx);\n  if (result == -1 && errno == ENOSYS)",
             "long result = syscall(SYS_io_setup, 128, &ctx);\n  if (result == -1 && errno == EPERM)",
             "oracle",
             "check 1 still issues io_setup but demands EPERM, so the real ENOSYS refusal "
             "no longer satisfies the contract"),
        ],
    },
    "backend-parity-c/child-subreaper-refusal": {
        "program": "tests/backend-parity/fixtures/child_subreaper_refusal.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             "prctl(PR_GET_CHILD_SUBREAPER, &value, 0, 0, 0)",
             "prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) + (value & 0)",
             "behaviour",
             "check 2 issues PR_GET_DUMPABLE (permitted) instead of PR_GET_CHILD_SUBREAPER; "
             "`+ (value & 0)` is a no-op that keeps `value` live for -Werror"),
            ("M2-oracle",
             "int set_rc = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);\n  if (set_rc == -1 && errno == ENOSYS)",
             "int set_rc = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);\n  if (set_rc == -1 && errno == EPERM)",
             "oracle",
             "check 1 still issues PR_SET_CHILD_SUBREAPER but demands EPERM"),
        ],
    },
    "backend-parity-c/mce-kill-refusal": {
        "program": "tests/backend-parity/fixtures/mce_kill_policy.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             "prctl(PR_MCE_KILL_GET, 0, 0, 0, 0)",
             DUMPABLE,
             "behaviour",
             "check 2 issues PR_GET_DUMPABLE (permitted) instead of PR_MCE_KILL_GET"),
            ("M2-oracle",
             "PR_MCE_KILL_EARLY, 0, 0) == -1 &&\n      errno == ENOSYS",
             "PR_MCE_KILL_EARLY, 0, 0) == -1 &&\n      errno == EPERM",
             "oracle",
             "check 1 still issues PR_MCE_KILL/SET but demands EPERM"),
        ],
    },
    "backend-parity-c/name-to-handle-refusal": {
        "program": "tests/backend-parity/fixtures/name_to_handle.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             'name_to_handle_at(AT_FDCWD, ".", &storage.handle, &mount_id, 0)',
             'open(".", O_RDONLY)',
             "behaviour",
             "check 3 opens the directory (which succeeds) instead of calling "
             "name_to_handle_at, so no refusal is exercised"),
            ("M2-oracle",
             'name_to_handle_at(AT_FDCWD, "/", &storage.handle, &mount_id, 0);\n'
             "  if (result == -1 && errno == EOPNOTSUPP)",
             'name_to_handle_at(AT_FDCWD, "/", &storage.handle, &mount_id, 0);\n'
             "  if (result == -1 && errno == EPERM)",
             "oracle",
             "check 1 still calls name_to_handle_at but demands EPERM"),
        ],
    },
    "backend-parity-c/seccomp-refusal": {
        "program": "tests/backend-parity/fixtures/seccomp_refusal.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             "prctl(PR_GET_SECCOMP, 0, 0, 0, 0)",
             DUMPABLE,
             "behaviour",
             "check 1 issues PR_GET_DUMPABLE (permitted) instead of PR_GET_SECCOMP"),
            ("M2-oracle",
             "errno == EOPNOTSUPP",
             "errno == EPERM",
             "oracle",
             "check 2 still issues seccomp(2) but demands EPERM instead of EOPNOTSUPP"),
        ],
    },
    "backend-parity-c/sysv-ipc-refusal": {
        "program": "tests/backend-parity/fixtures/sysv_ipc_refusal.c",
        "cflags": [],
        "muts": [
            ("M1-behaviour",
             "int msg = msgget(IPC_PRIVATE, IPC_CREAT | 0600);",
             "int msg = 3;",
             "behaviour",
             "check 3 observes a successful-looking descriptor instead of the "
             "refused msgget return"),
            ("M2-oracle",
             "int sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);\n  if (sem == -1 && errno == ENOSYS)",
             "int sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);\n  if (sem == -1 && errno == EPERM)",
             "oracle",
             "check 1 still issues semget but demands EPERM"),
        ],
    },
    # NEGATIVE CONTROL: the known scaffolding-only cell. This must come back with
    # the OTHER verdict, or the instrument is stuck on "gated".
    "c-programs/clone": {
        "program": "tests/c/clone.c",
        "cflags": ["-D_GNU_SOURCE", "-pthread", "-Wno-error=unused-parameter",
                   "-Wno-error=unused-function", "-Wno-error=unused-variable",
                   "-Wno-error=attributes", "-Wno-error=unknown-pragmas",
                   "-Wno-error=sign-compare"],
        "muts": [
            ("M1-behaviour",
             '"Hello from child!"',
             '"CORRUPTED by mutation"',
             "behaviour",
             "the CLONE_VM child writes a different string through the shared "
             "address space"),
        ],
    },
}

HLOG = re.compile(r"^\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+(ERROR|WARN|INFO|DEBUG|TRACE)\s")


def blob(path):
    r = subprocess.run(["git", "-C", REPO, "show", f"{SHA}:{path}"],
                       capture_output=True, text=True)
    assert r.returncode == 0, f"BLOB READ FAILED {path}: {r.stderr[-200:]}"
    assert r.stdout, f"BLOB EMPTY {path}"
    return r.stdout


def guest_only(text):
    """Strip hermit's own log lines; what remains is the guest's own output."""
    return "".join(l + "\n" for l in text.split("\n")
                   if l and not HLOG.match(l))


def build(src_text, name, cflags):
    sp = f"{D}/src/{name}.c"
    open(sp, "w").write(src_text)
    out = f"{D}/bin/{name}"
    r = subprocess.run(CC + list(cflags) + [sp, "-o", out],
                       capture_output=True, text=True)
    assert r.returncode == 0, f"BUILD FAILED {name}: {r.stderr[-400:]}"
    return out


def cell_env(cellroot):
    """The harness's HERMIT_RUN_ENV (manifest-to-commands.rs:39)."""
    for sub in ("home", "xdg-config", "fixtures"):
        os.makedirs(f"{cellroot}/{sub}", exist_ok=True)
    os.makedirs("/tmp/hermit-e2e", exist_ok=True)
    e = dict(os.environ)
    e.update({"LC_ALL": "C", "TZ": "UTC", "HOME": f"{cellroot}/home",
              "XDG_CONFIG_HOME": f"{cellroot}/xdg-config",
              "E2E_TMPDIR": "/tmp/hermit-e2e",
              "E2E_FIXTURE_DIR": f"{cellroot}/fixtures"})
    return e


def observe(binpath):
    """THE OBSERVABLE. A single strict run under the portable profile; hermit
    passes the guest's stdout and stderr through."""
    cellroot = tempfile.mkdtemp(dir=f"{D}/work")
    try:
        p = subprocess.run(
            [H, "--log=error", "run", "--backend", "ptrace", "--strict",
             "--no-virtualize-cpuid", "--max-timeslice=disabled", "--", binpath],
            capture_output=True, text=True, timeout=TIMEOUT_S + 30,
            env=cell_env(cellroot))
        return {"rc": p.returncode,
                "stdout": guest_only(p.stdout),
                "stderr": guest_only(p.stderr)}
    except subprocess.TimeoutExpired:
        return {"rc": "TIMEOUT", "stdout": "", "stderr": ""}
    finally:
        shutil.rmtree(cellroot, ignore_errors=True)


def gate_replay(binpath):
    """THE GATE, replay mode. Exactly manifest-to-commands.rs:269-271."""
    cellroot = tempfile.mkdtemp(dir=f"{D}/work")
    try:
        p = subprocess.run(
            [H, "--log=info", "--backend", "ptrace", "record", "start", "--strict",
             "--verify", "--data-dir", f"{cellroot}/recording",
             "--record-timeout", str(TIMEOUT_S), "--", binpath],
            capture_output=True, text=True, timeout=TIMEOUT_S + 60,
            env=cell_env(cellroot))
        return {"rc": p.returncode,
                "guest_stdout": guest_only(p.stdout),
                "guest_stderr": guest_only(p.stderr)}
    except subprocess.TimeoutExpired:
        return {"rc": "TIMEOUT", "guest_stdout": "", "guest_stderr": ""}
    finally:
        shutil.rmtree(cellroot, ignore_errors=True)


def gate_verify(binpath):
    """THE GATE, verify mode -- the mode these six cells are actually enabled in.
    Exactly manifest-to-commands.rs:258-267, portable lane, no bitwise parity."""
    cellroot = tempfile.mkdtemp(dir=f"{D}/work")
    try:
        p = subprocess.run(
            [H, "--log=info", "run", "--backend", "ptrace", "--strict", "--verify",
             "--no-virtualize-cpuid", "--max-timeslice=disabled", "--", binpath],
            capture_output=True, text=True, timeout=TIMEOUT_S + 60,
            env=cell_env(cellroot))
        return {"rc": p.returncode}
    except subprocess.TimeoutExpired:
        return {"rc": "TIMEOUT"}
    finally:
        shutil.rmtree(cellroot, ignore_errors=True)


def native(binpath):
    p = subprocess.run([binpath], capture_output=True, text=True, timeout=60)
    return {"rc": p.returncode, "stdout": p.stdout, "stderr": p.stderr}


def run_cell(cell, spec):
    tag = cell.replace("/", "_")
    src = blob(spec["program"])
    acc_bin = build(src, f"{tag}.accept", spec["cflags"])
    rec = {"cell": cell, "program": spec["program"], "blob_sha": SHA,
           "src_bytes": len(src),
           "accept": {"observable": observe(acc_bin),
                      "gate_replay": gate_replay(acc_bin),
                      "gate_verify": gate_verify(acc_bin),
                      "native": native(acc_bin)},
           "mutants": []}
    for label, pat, rep, family, note in spec["muts"]:
        n = src.count(pat)
        assert n == 1, f"{cell} {label}: ANCHOR NOT UNIQUE (count={n}): {pat!r}"
        mut_src = src.replace(pat, rep)
        assert mut_src != src
        mut_bin = build(mut_src, f"{tag}.{label}", spec["cflags"])
        rec["mutants"].append({
            "label": label, "family": family, "note": note,
            "anchor": pat, "replacement": rep,
            "observable": observe(mut_bin),
            "gate_replay": gate_replay(mut_bin),
            "gate_verify": gate_verify(mut_bin),
        })
    return rec


def preflight(only):
    """Build every accept and mutant binary FIRST, so an anchor or -Werror
    defect in my own instrument surfaces before any measurement is taken."""
    for cell, spec in CELLS.items():
        if only and cell not in only:
            continue
        tag = cell.replace("/", "_")
        src = blob(spec["program"])
        build(src, f"{tag}.accept", spec["cflags"])
        for label, pat, rep, _f, _n in spec["muts"]:
            n = src.count(pat)
            assert n == 1, f"{cell} {label}: ANCHOR NOT UNIQUE (count={n}): {pat!r}"
            build(src.replace(pat, rep), f"{tag}.{label}", spec["cflags"])
    print("preflight: every anchor unique, every binary builds\n", flush=True)


if __name__ == "__main__":
    only = sys.argv[1:]
    preflight(only)
    out = []
    for cell, spec in CELLS.items():
        if only and cell not in only:
            continue
        print(f"== {cell}", flush=True)
        r = run_cell(cell, spec)
        a = r["accept"]
        print(f"   accept    obs_rc={a['observable']['rc']} "
              f"out={a['observable']['stdout']!r} "
              f"replay_rc={a['gate_replay']['rc']} verify_rc={a['gate_verify']['rc']} "
              f"native_rc={a['native']['rc']}", flush=True)
        for m in r["mutants"]:
            print(f"   {m['label']:13s} obs_rc={m['observable']['rc']} "
                  f"out={m['observable']['stdout']!r} "
                  f"err={m['observable']['stderr']!r} "
                  f"replay_rc={m['gate_replay']['rc']} "
                  f"verify_rc={m['gate_verify']['rc']}", flush=True)
        out.append(r)
    json.dump(out, open(f"{D}/results.json", "w"), indent=1)
    print(f"\nwrote {D}/results.json")
