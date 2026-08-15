#!/usr/bin/env python3
"""Qualify a candidate verify/kvm cell by measuring BOTH directions.

A justification that no longer holds makes a cell a CANDIDATE, not a qualified
cell. Enabling needs both arms:

  ACCEPT  the real fixture passes  `--strict --verify` on kvm
  REFUSE  an inert VIOLATING copy of the same fixture FAILS the same command

The refuse arm uses a mutated copy of the fixture -- an inert fixture, never a
change to the manifest or to a real cell. A cell that cannot be made to fail
would book a green that can never go red, which is worse than leaving it off.

Fixes the defect found in direct.py: hermit's grandchildren keep the stderr pipe
open, so subprocess.run(timeout=) wedges. Runs each child in its own session and
kills the whole process group.
"""
import json, os, re, shutil, signal, subprocess, sys, tempfile

REPO = "/home/newton/work/dev-hermit/worktrees/slot04/hermit"
HERMIT = os.path.join(REPO, "target/release/hermit")
OUT = "/home/newton/work/dev-hermit/ignored/kvm2-qual"
STAGE = os.path.join(OUT, "stage")           # MUST be outside host /tmp
CFLAGS = ["-std=gnu11", "-O1", "-w", "-D_GNU_SOURCE", "-pthread"]
STR = re.compile(r'"(?:[^"\\]|\\.)*"')


def run(cmd, cwd, timeout):
    """Run in its own session; on timeout kill the whole group, not just the child."""
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    env.setdefault("E2E_TMPDIR", cwd)
    env.setdefault("E2E_FIXTURE_DIR", cwd)
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True)
    try:
        out, err = p.communicate(timeout=timeout)
        return {"rc": p.returncode, "timeout": False,
                "out": out.decode(errors="replace"), "err": err.decode(errors="replace")}
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(p.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            p.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(p.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
            p.communicate()
        return {"rc": None, "timeout": True, "out": "", "err": ""}


def sites(src):
    offs, pos = [], 0
    for line in src.split("\n"):
        s = line.lstrip()
        if not (s.startswith("#") or s.startswith("//")):
            spans = [(m.start(), m.end()) for m in STR.finditer(line)]
            for m in re.finditer(r"==|!=", line):
                if not any(a <= m.start() < b for a, b in spans):
                    offs.append(pos + m.start())
        pos += len(line) + 1
    return offs


def flip(src, off):
    return src[:off] + ("!=" if src[off:off + 2] == "==" else "==") + src[off + 2:]


def cc(text, path):
    open(path + ".c", "w").write(text)
    r = subprocess.run(["cc"] + CFLAGS + [path + ".c", "-o", path],
                       capture_output=True, timeout=180)
    return r.returncode == 0


def kvm(binary, cwd, timeout=120):
    return run([HERMIT, "--log=info", "run", "--backend=kvm", "--strict", "--verify",
                "--no-virtualize-cpuid", "--max-timeslice=disabled", "--", binary], cwd, timeout)


def qualify(cell):
    r = dict(cell)
    src_path = os.path.join(REPO, cell["program"])
    if not src_path.endswith(".c") or not os.path.exists(src_path):
        return dict(r, qualified=False, why="fixture is not a C source in this checkout")
    src = open(src_path, errors="replace").read()
    os.makedirs(STAGE, exist_ok=True)
    wd = tempfile.mkdtemp(prefix="q-", dir=STAGE)
    try:
        base = os.path.join(wd, "base")
        if not cc(src, base):
            return dict(r, qualified=False, why="fixture does not compile standalone")
        a = kvm(base, wd)
        r["accept_rc"] = a["rc"]; r["accept_timeout"] = a["timeout"]
        if a["timeout"] or a["rc"] != 0:
            return dict(r, qualified=False, arm="accept",
                        why="ACCEPT arm did not pass (rc=%s timeout=%s); not a candidate right now"
                            % (a["rc"], a["timeout"]))
        # refuse arm: first mutant that both compiles and changes the kvm verdict
        tried = 0
        for i, off in enumerate(sites(src)[:12]):
            m = os.path.join(wd, "m%d" % i)
            if not cc(flip(src, off), m):
                continue
            tried += 1
            v = kvm(m, wd, timeout=90)
            if v["timeout"]:
                return dict(r, qualified=True, arm="both", refuse_rc="timeout", mutants_tried=tried,
                            why="ACCEPT rc=0; REFUSE: a violating copy hangs, which the harness scores FAIL via its timeout branch")
            if v["rc"] != 0:
                tail = [l for l in v["err"].split("\n")
                        if l.strip() and not l.startswith("20") and "CPUID" not in l]
                return dict(r, qualified=True, arm="both", refuse_rc=v["rc"], mutants_tried=tried,
                            why="ACCEPT rc=0; REFUSE rc=%s on a violating copy" % v["rc"],
                            refuse_msg=(tail[-1][:120] if tail else ""))
        return dict(r, qualified=False, arm="refuse", mutants_tried=tried,
                    why="ACCEPT rc=0 but NO violating copy could make it fail (%d mutants); "
                        "enabling would book a green that cannot go red" % tried)
    finally:
        shutil.rmtree(wd, ignore_errors=True)


if __name__ == "__main__":
    cells = json.load(open(sys.argv[1]))
    out = []
    for i, c in enumerate(cells):
        out.append(qualify(c))
        print("%d/%d %-46s qualified=%s" % (i + 1, len(cells), c["id"], out[-1].get("qualified")),
              flush=True)
        json.dump(out, open(sys.argv[2], "w"), indent=1)   # write incrementally
