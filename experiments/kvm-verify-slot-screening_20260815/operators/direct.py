#!/usr/bin/env python3
"""Direct-run classifier for a disabled verify/kvm cell's justification.

Replicates the flags ci/test_harness.sh applies to a portable-lane verify cell
(--strict --verify plus the portable profile) without the harness's ~90s of
setup, so a sample can be scaled at this host load. Validated against harness
verdicts before use -- see agreement check.

Three-way outcome:
  DOES-NOT-HOLD    cell passes; the recorded justification no longer describes reality
  STILL-HOLDS      cell fails AND the guest's own message names the boundary the
                   justification cites (message-bearing failure)
  NOT-DETERMINABLE timeout, no message, or a message that does not correspond to the
                   stated boundary -- under load a bare failure is not attributable
"""
import json, os, re, shutil, subprocess, sys, tempfile

REPO = "/home/newton/work/dev-hermit/worktrees/slot04/hermit"
HERMIT = os.path.join(REPO, "target/release/hermit")
OUT = "/home/newton/work/dev-hermit/ignored/kvm2-survey"
CFLAGS = ["-std=gnu11", "-O1", "-w", "-D_GNU_SOURCE", "-pthread"]
NOISE = re.compile(r"^20\d\d-|ARCH_(GET|SET)_CPUID|^\s*$")

# tokens that let a guest message be matched against a justification
TOKENS = re.compile(r"[A-Z][A-Z0-9_]{3,}|\b(?:e|E)rrno\s*\d+|\b\d{2,3}\b")


def compile_fixture(program, wd):
    src = os.path.join(REPO, program)
    if not src.endswith(".c") or not os.path.exists(src):
        return None, "fixture is not a C source in this checkout"
    b = os.path.join(wd, "guest")
    cp = subprocess.run(["cc"] + CFLAGS + [src, "-o", b], capture_output=True, timeout=180)
    if cp.returncode != 0:
        return None, "fixture does not compile standalone: " + cp.stderr.decode()[-160:]
    return b, None


def run_kvm(binary, wd, timeout=150):
    cmd = [HERMIT, "--log=info", "run", "--backend=kvm", "--strict", "--verify",
           "--no-virtualize-cpuid", "--max-timeslice=disabled", "--", binary]
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    env.setdefault("E2E_TMPDIR", wd)
    env.setdefault("E2E_FIXTURE_DIR", wd)
    try:
        r = subprocess.run(cmd, capture_output=True, timeout=timeout, cwd=wd, env=env,
                           stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return {"rc": None, "timeout": True, "msg": ""}
    err = r.stderr.decode(errors="replace")
    msg = "\n".join(l for l in err.split("\n") if not NOISE.match(l))
    return {"rc": r.returncode, "timeout": False, "msg": msg[-900:]}


def classify(reason, res):
    if res["timeout"]:
        return "NOT-DETERMINABLE", "run timed out; a timeout carries no cause and under load is not attributable"
    if res["rc"] == 0:
        return "DOES-NOT-HOLD", "cell passes --strict --verify on kvm; the recorded justification no longer describes reality"
    # failing: is the message bearing, and does it name the cited boundary?
    m = res["msg"]
    if not m.strip():
        return "NOT-DETERMINABLE", "failed with no guest message; nothing names a boundary"
    jt = set(t for t in TOKENS.findall(reason) if not t.isdigit())
    mt = set(t for t in TOKENS.findall(m) if not t.isdigit())
    shared = jt & mt
    # errno 38 == ENOSYS: let the justification's ENOSYS match a numeric errno 38
    if "ENOSYS" in jt and re.search(r"errno\s*38\b", m):
        shared = shared | {"ENOSYS(errno 38)"}
    if shared:
        return "STILL-HOLDS", "message-bearing failure naming the cited boundary: %s | %s" % (
            sorted(shared)[:4], m.strip().splitlines()[-1][:110])
    return "NOT-DETERMINABLE", "failed with a message that does not name the cited boundary: %s" % (
        m.strip().splitlines()[-1][:110])


# Guests MUST be staged OUTSIDE host /tmp: hermit replaces guest /tmp with an
# isolated directory and refuses a program that lives under host /tmp. Measured --
# tempfile.TemporaryDirectory() put 12 of 15 agreement cells into a launch refusal.
STAGE = os.path.join(OUT, "stage")


def screen(cell):
    os.makedirs(STAGE, exist_ok=True)
    wd = tempfile.mkdtemp(prefix="sv-", dir=STAGE)
    try:
        b, err = compile_fixture(cell["program"], wd)
        if b is None:
            return dict(cell, verdict="NOT-DETERMINABLE", why=err)
        res = run_kvm(b, wd)
        if "under host /tmp" in res.get("msg", ""):
            return dict(cell, verdict="NOT-DETERMINABLE",
                        why="guest launch refused (staged under host /tmp) -- harness bug, not a cell result")
        v, why = classify(cell["reason"], res)
        return dict(cell, verdict=v, why=why, rc=res["rc"])
    finally:
        shutil.rmtree(wd, ignore_errors=True)


if __name__ == "__main__":
    cells = json.load(open(sys.argv[1]))
    out = []
    for i, c in enumerate(cells):
        out.append(screen(c))
        print("%d/%d %s -> %s" % (i + 1, len(cells), c["id"], out[-1]["verdict"]), flush=True)
    json.dump(out, open(sys.argv[2], "w"), indent=1)
