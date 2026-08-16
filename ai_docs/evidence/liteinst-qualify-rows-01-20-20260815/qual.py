#!/usr/bin/env python3
"""Qualify liteinst/verify queue rows 1-20 at the FIXTURE level, under Hermit.

The liteinst backend is unavailable on this binary, so the cell-level arms cannot
be run. What IS measurable is the property the queue's own header defines as
"qualifiable": a semantic mutation changes the fixture's EXIT STATUS -- explicitly
a property of the program, with backend capability named as a separate gate.

Runs under Hermit (ptrace), never natively. That matters: a `*-refusal` fixture
FAILS natively (the syscall it expects to be refused succeeds), so a native
baseline never exercises the accept path. Eight of these twenty carry exactly that
in the queue's evidence column -- "baseline already exits 1 natively".

Three arms, because two cannot tell a working gate from a mutation that never applied:
  ACCEPT   unmutated fixture passes under Hermit
  APPLIED  the mutant differs from the base as a build artifact (source + behaviour)
  REFUSE   the mutant is caught (exit status moves) under Hermit
"""
import json, os, re, shutil, signal, subprocess, sys, tempfile

REPO = "/home/newton/work/dev-hermit/worktrees/slot04/hermit"
HERMIT = os.path.join(REPO, "target/release/hermit")
OUT = "/home/newton/work/dev-hermit/ignored/kvm2-q0120"
STAGE = os.path.join(OUT, "stage")          # MUST be outside host /tmp
CF = ["-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror"]


def run(cmd, cwd, timeout):
    """Own session; a timeout kills the whole group so grandchildren cannot wedge us."""
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    env.setdefault("E2E_TMPDIR", cwd)
    env.setdefault("E2E_FIXTURE_DIR", cwd)
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True)
    try:
        o, e = p.communicate(timeout=timeout)
        return {"rc": p.returncode, "timeout": False,
                "out": o.decode(errors="replace"), "err": e.decode(errors="replace")}
    except subprocess.TimeoutExpired:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(os.getpgid(p.pid), sig)
            except ProcessLookupError:
                break
            try:
                p.communicate(timeout=8)
                break
            except subprocess.TimeoutExpired:
                continue
        return {"rc": None, "timeout": True, "out": "", "err": ""}


def cc(text, path, extra=()):
    open(path + ".c", "w").write(text)
    r = subprocess.run(["cc"] + CF + list(extra) + [path + ".c", "-o", path], capture_output=True, timeout=240)
    return (r.returncode == 0, r.stderr.decode()[-200:])


def hermit(binary, cwd, backend="ptrace", timeout=150):
    cmd = [HERMIT, "--log=info", "run"]
    if backend != "ptrace":
        cmd.append(f"--backend={backend}")
    cmd += ["--strict", "--verify", "--no-virtualize-cpuid", "--max-timeslice=disabled",
            "--", binary]
    return run(cmd, cwd, timeout)


def mutate(src, family):
    """Return (mutated_text, description) or (None, reason)."""
    if family == "ok-init":
        new, n = re.subn(r"\bint ok = 0;", "int ok = 1;", src, count=1)
        if n:
            return new, "int ok = 0 -> 1"
    if family == "violations-static":
        new, n = re.subn(r"\bstatic int violations;", "static int violations = 1;", src, count=1)
        if n:
            return new, "static int violations -> initialised to 1"
        new, n = re.subn(r"\bint violations = 0;", "int violations = 1;", src, count=1)
        if n:
            return new, "int violations = 0 -> 1"
    if family == "bad-init":
        for pat, rep, d in ((r"\bint bad = 0;", "int bad = 1;", "int bad = 0 -> 1"),
                            (r"\bstatic int bad;", "static int bad = 1;", "static int bad -> 1")):
            new, n = re.subn(pat, rep, src, count=1)
            if n:
                return new, d
    if family == "guard-inversion":
        m = re.search(r"\) == (\d+)\) \? 1 : 0;", src)
        if m:
            return src.replace(m.group(0), ") != %s) ? 1 : 0;" % m.group(1), 1), \
                   "guard `== %s` inverted to `!= %s`" % (m.group(1), m.group(1))
    # last resort: no family pattern matched
    return None, "no %s pattern matched in this fixture" % family


def qualify(row):
    r = dict(row)
    src_path = os.path.join(REPO, row["fixture"])
    if not os.path.exists(src_path):
        return dict(r, verdict="NOT-DETERMINABLE", why="fixture missing from this checkout")
    src = open(src_path, errors="replace").read()
    mut, desc = mutate(src, row["family"])
    r["mutation"] = desc
    os.makedirs(STAGE, exist_ok=True)
    wd = tempfile.mkdtemp(prefix="q-", dir=STAGE)
    try:
        base = os.path.join(wd, "base")
        ok, err = cc(src, base, row.get("cflags", []))
        if not ok:
            return dict(r, verdict="NOT-DETERMINABLE", why="baseline will not compile: " + err[:120])
        a = hermit(base, wd)
        r["accept_rc"] = a["rc"]; r["accept_out"] = a["out"].strip()[:90]
        if a["timeout"]:
            return dict(r, verdict="NOT-DETERMINABLE", why="ACCEPT arm timed out under hermit")
        if a["rc"] != 0:
            gm = [l for l in a["err"].split("\n") if l.strip() and not l.startswith("20")
                  and "CPUID" not in l and not l.startswith("::")]
            return dict(r, verdict="NOT-QUALIFIED", arm="accept",
                        why="ACCEPT arm fails under hermit (rc=%s): %s"
                            % (a["rc"], (gm[-1][:100] if gm else "")))
        if mut is None:
            return dict(r, verdict="NOT-DETERMINABLE", arm="refuse",
                        why="ACCEPT passes but the mutation never applied: " + desc)
        m = os.path.join(wd, "mut")
        ok, err = cc(mut, m, row.get("cflags", []))
        if not ok:
            return dict(r, verdict="NOT-DETERMINABLE", arm="refuse",
                        why="mutant will not compile: " + err[:120])
        # APPLIED: the mutant must differ as an artifact before its verdict means anything
        same_bytes = open(base, "rb").read() == open(m, "rb").read()
        r["binaries_identical"] = same_bytes
        if same_bytes:
            return dict(r, verdict="NOT-DETERMINABLE", arm="refuse",
                        why="mutation compiled to a byte-identical binary; it did not bite")
        v = hermit(m, wd)
        r["refuse_rc"] = v["rc"]; r["refuse_timeout"] = v["timeout"]
        if v["timeout"]:
            return dict(r, verdict="REFUSE-TIMEOUT", arm="refuse",
                        why="mutant timed out; re-run once with a control before scoring")
        if v["rc"] != a["rc"]:
            gm = [l for l in v["err"].split("\n") if l.strip() and not l.startswith("20")
                  and "CPUID" not in l and not l.startswith("::")]
            return dict(r, verdict="QUALIFIED", arm="both",
                        why="ACCEPT rc=0; REFUSE rc=%s (%s)" % (v["rc"], desc),
                        refuse_msg=(gm[-1][:100] if gm else ""))
        return dict(r, verdict="NOT-EXIT-GATED", arm="refuse",
                    why="mutation applied and binaries differ, but exit status stayed %s" % a["rc"],
                    refuse_out=v["out"].strip()[:90])
    finally:
        shutil.rmtree(wd, ignore_errors=True)


if __name__ == "__main__":
    rows = json.load(open(sys.argv[1])); out = []
    for i, row in enumerate(rows):
        out.append(qualify(row))
        print("%2d/%d %-38s %s" % (i + 1, len(rows), row["cell"][:36], out[-1]["verdict"]), flush=True)
        json.dump(out, open(sys.argv[2], "w"), indent=1)
