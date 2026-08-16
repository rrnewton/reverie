#!/usr/bin/env python3
"""Qualify unworked ptrace+replay candidate cells: BOTH ARMS, under Hermit.

Read-only on the product: fixture sources come from a git blob at a pinned SHA,
mutants are compiled into this gitignored scratch dir, no manifest is touched
and nothing is enabled.

Two measurements per binary, kept separate on purpose:

  OBSERVABLE  `hermit run --backend ptrace --strict` on the portable profile.
              This passes the guest's own stdout AND stderr through, so it is
              where the bite is proven.  NOT `.text`.

  GATE        the exact replay-mode command emitted by
              scripts/manifest-to-commands.rs:269-271.  Only the exit status is
              read, because that is all the replay verdict reads.

Mutation anchors are asserted UNIQUE (count == 1), never first-match.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

D = "/home/newton/work/dev-hermit/ignored/qualify-nonpriv-20260816"
REPO = "/home/newton/work/dev-hermit/hermit"
H = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
SHA = "302a1a9c0fde564db0292d1ea5cabc91343e79bc"
CC = ["cc", "-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror"]

CELLS = [
    "backend-parity-c/cachestat-refusal",
    "backend-parity-c/faccessat2-flags",
    "backend-parity-c/kcmp-refusal",
    "backend-parity-c/openat2-refusal",
    "backend-parity-c/pidfd-open-self",
    "c-programs/acct-refusal-probe",
    "c-programs/dbt-execveat-unsupported",
    "c-programs/kcmp-eperm",
    "c-programs/keyctl-passthrough",
    "c-programs/process-vm-readv-refusal-probe",
    "c-programs/process-vm-writev-refusal-probe",
    "c-programs/ptrace-eperm",
]

HLOG = re.compile(r"^\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+(ERROR|WARN|INFO|DEBUG|TRACE)\s")
SYSCALL_SITE = re.compile(r"syscall\(\s*(SYS_[A-Za-z0-9_]+)")
ERRNO_ASSERT = re.compile(r"errno\s*==\s*(E[A-Z0-9]+)")


def sh(*a, **kw):
    return subprocess.run(a, capture_output=True, text=True, **kw)


def blob(path):
    r = sh("git", "-C", REPO, "show", f"{SHA}:{path}")
    assert r.returncode == 0, f"BLOB READ FAILED {path}"
    assert r.stdout, f"BLOB EMPTY {path}"
    return r.stdout


def guest_only(text):
    """Strip hermit's own log lines; what remains is the guest's own output."""
    return "".join(l + "\n" for l in text.split("\n") if l and not HLOG.match(l))


def build(src_text, name, cflags):
    sp = f"{D}/src/{name}.c"
    open(sp, "w").write(src_text)
    out = f"{D}/bin/{name}"
    r = sh(*(CC + list(cflags) + [sp, "-o", out]))
    if r.returncode != 0:
        return None, r.stderr[-300:]
    return out, None


def cell_env(root):
    for sub in ("home", "xdg-config", "fixtures"):
        os.makedirs(f"{root}/{sub}", exist_ok=True)
    os.makedirs("/tmp/hermit-e2e", exist_ok=True)
    e = dict(os.environ)
    e.update({"LC_ALL": "C", "TZ": "UTC", "HOME": f"{root}/home",
              "XDG_CONFIG_HOME": f"{root}/xdg-config",
              "E2E_TMPDIR": "/tmp/hermit-e2e",
              "E2E_FIXTURE_DIR": f"{root}/fixtures"})
    return e


def observe(binpath, timeout):
    """THE OBSERVABLE. Guest stdout+stderr come through here."""
    root = tempfile.mkdtemp(dir=f"{D}/work")
    try:
        p = subprocess.run(
            [H, "--log=error", "run", "--backend", "ptrace", "--strict",
             "--no-virtualize-cpuid", "--max-timeslice=disabled", "--", binpath],
            capture_output=True, text=True, timeout=timeout + 30, env=cell_env(root))
        return {"rc": p.returncode, "stdout": guest_only(p.stdout),
                "stderr": guest_only(p.stderr)}
    except subprocess.TimeoutExpired:
        return {"rc": "TIMEOUT", "stdout": "", "stderr": ""}
    finally:
        shutil.rmtree(root, ignore_errors=True)


def gate_replay(binpath, timeout):
    """THE GATE. Exactly manifest-to-commands.rs:269-271. Exit status only."""
    root = tempfile.mkdtemp(dir=f"{D}/work")
    try:
        p = subprocess.run(
            [H, "--log=info", "--backend", "ptrace", "record", "start", "--strict",
             "--verify", "--data-dir", f"{root}/recording",
             "--record-timeout", str(timeout), "--", binpath],
            capture_output=True, text=True, timeout=timeout + 60, env=cell_env(root))
        return {"rc": p.returncode}
    except subprocess.TimeoutExpired:
        return {"rc": "TIMEOUT"}
    finally:
        shutil.rmtree(root, ignore_errors=True)


def mutations(src):
    """Two INDEPENDENT families. Anchors asserted unique by the caller."""
    out = []
    # M1 behaviour: issue a different, permitted syscall at the call site.
    m = SYSCALL_SITE.search(src)
    if m:
        name = m.group(1)
        anchor = m.group(0)                       # "syscall(SYS_foo"
        if src.count(anchor) == 1:
            sub = "SYS_getppid" if name != "SYS_getppid" else "SYS_getpid"
            out.append(("M1-behaviour", anchor, anchor.replace(name, sub), "behaviour",
                        f"{name} -> {sub}: the guest issues a permitted syscall, so the "
                        f"contracted refusal is never exercised"))
    # M2 oracle: demand a different errno; the SAME call is still issued.
    m = ERRNO_ASSERT.search(src)
    if m:
        want = m.group(1)
        anchor = m.group(0)
        if src.count(anchor) == 1:
            sub = "EPERM" if want != "EPERM" else "ENOSYS"
            out.append(("M2-oracle", anchor, f"errno == {sub}", "oracle",
                        f"errno == {want} -> {sub}: the same call is issued and really "
                        f"refused; only the demanded errno changes"))
    return out


def run_cell(cell, spec):
    tag = cell.replace("/", "_")
    src = blob(spec["program"])
    timeout = spec["timeout"]
    rec = {"cell": cell, "mode": "replay", "backend": "ptrace",
           "lane": spec["lane"], "program": spec["program"], "blob_sha": SHA,
           "timeout_seconds": timeout, "mutants": []}
    acc, err = build(src, f"{tag}.accept", spec["cflags"])
    if acc is None:
        rec["verdict"] = "NOT-DETERMINABLE"
        rec["reason"] = f"accept build failed: {err}"
        return rec
    rec["accept"] = {"observable": observe(acc, timeout),
                     "gate_replay": gate_replay(acc, timeout)}
    muts = mutations(src)
    if not muts:
        rec["verdict"] = "NOT-DETERMINABLE"
        rec["reason"] = "no mutation anchor was unique in this source"
        return rec
    for label, anchor, repl, family, note in muts:
        n = src.count(anchor)
        assert n == 1, f"{cell} {label}: ANCHOR NOT UNIQUE ({n}): {anchor!r}"
        mb, merr = build(src.replace(anchor, repl), f"{tag}.{label}", spec["cflags"])
        if mb is None:
            rec["mutants"].append({"label": label, "family": family, "note": note,
                                   "anchor": anchor, "replacement": repl,
                                   "build_error": merr})
            continue
        rec["mutants"].append({"label": label, "family": family, "note": note,
                               "anchor": anchor, "replacement": repl,
                               "observable": observe(mb, timeout),
                               "gate_replay": gate_replay(mb, timeout)})
    return rec


def verdict(rec):
    """QUALIFIED requires BOTH arms on the named backend, and a mutant proven to
    bite the observable before its gate result is read."""
    if "verdict" in rec:
        return rec["verdict"], rec["reason"]
    a = rec["accept"]
    if a["observable"]["rc"] == "TIMEOUT" or a["gate_replay"]["rc"] == "TIMEOUT":
        return "NOT-DETERMINABLE", "accept arm timed out"
    if a["gate_replay"]["rc"] != 0:
        return "ACCEPT-ARM-FAILS", (
            f"accept arm does not pass the replay gate (rc={a['gate_replay']['rc']}); "
            f"the cell cannot be qualified until it passes clean")
    biting = []
    for m in rec["mutants"]:
        if "build_error" in m:
            continue
        ao = (a["observable"]["rc"], a["observable"]["stdout"], a["observable"]["stderr"])
        mo = (m["observable"]["rc"], m["observable"]["stdout"], m["observable"]["stderr"])
        m["bites_observable"] = ao != mo
        m["gates"] = m["gate_replay"]["rc"] not in (0, "TIMEOUT")
        if m["bites_observable"]:
            biting.append(m)
    if not biting:
        return "NOT-DETERMINABLE", "no mutation moved the observable; instrument, not fixture"
    if all(m["gates"] for m in biting):
        return "QUALIFIED-PTRACE", (
            f"accept arm passes the replay gate and {len(biting)} biting mutant(s) "
            f"across {len({m['family'] for m in biting})} independent famil(y/ies) "
            f"drive it off 0")
    if any(m["gates"] for m in biting):
        return "QUALIFIED-PTRACE-PARTIAL", (
            "at least one biting mutant moves the exit status and at least one does not")
    return "NOT-EXIT-GATED", (
        "every biting mutant left the replay exit status at 0")


if __name__ == "__main__":
    for sub in ("src", "bin", "work"):
        os.makedirs(f"{D}/{sub}", exist_ok=True)
    progs = json.load(open("/tmp/pop.json"))["progs"]
    only = sys.argv[1:] or CELLS
    # BUILD PREFLIGHT: surface an anchor or -Werror defect before any measurement.
    for cell in only:
        s = blob(progs[cell]["program"])
        t = cell.replace("/", "_")
        b, e = build(s, f"{t}.accept", progs[cell]["cflags"])
        assert b, f"PREFLIGHT accept build failed for {cell}: {e}"
        for label, anchor, repl, _f, _n in mutations(s):
            assert s.count(anchor) == 1, f"PREFLIGHT {cell} {label}: anchor not unique"
    print(f"preflight: {len(only)} accept binaries build, every anchor unique\n", flush=True)
    out = []
    for cell in only:
        r = run_cell(cell, progs[cell])
        v, why = verdict(r)
        r["verdict"], r["reason"] = v, why
        out.append(r)
        a = r.get("accept", {})
        print(f"{v:26s} {cell}")
        if a:
            print(f"    accept  obs_rc={a['observable']['rc']} out={a['observable']['stdout']!r} "
                  f"replay_rc={a['gate_replay']['rc']}")
        for m in r["mutants"]:
            if "build_error" in m:
                print(f"    {m['label']:13s} BUILD FAILED: {m['build_error'][:70]}")
                continue
            print(f"    {m['label']:13s} bite={m.get('bites_observable')} "
                  f"obs_rc={m['observable']['rc']} replay_rc={m['gate_replay']['rc']} "
                  f"err={m['observable']['stderr'].strip()[:80]!r}")
        sys.stdout.flush()
    json.dump(out, open(f"{D}/results.json", "w"), indent=1)
    print(f"\nwrote {D}/results.json")
