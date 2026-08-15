#!/usr/bin/env python3
"""Screen disabled verify/kvm slots for whether the fixture can express failure
in the channel the verify verdict observes (exit status + stdout; stderr is NOT
observed -- observation = {status=true, stdout=true, stderr=false} on all 313).

Method: conditional-negation mutation of the fixture source (== <-> !=), then
compare (exit status, stdout) of baseline vs mutant, run natively.

Rationale for mutation over pattern-matching: a static grep undercounts, e.g.
`return ok == 4 ? 0 : 1;` has no `return [1-9]` at the start of a return.
We track separately whether the mutation ACTUALLY APPLIED, so that
"mutation never applied" is never confused with "applied, no observable change".
"""
import glob, os, re, subprocess, sys, tempfile, json, hashlib

REPO = "/home/newton/work/dev-hermit/worktrees/slot04/hermit"
OUT = "/home/newton/work/dev-hermit/ignored/kvm2-screen"

CFLAGS = ["-std=gnu11", "-O1", "-w", "-D_GNU_SOURCE", "-pthread"]

STR = re.compile(r'"(?:[^"\\]|\\.)*"')


def sites_of(src):
    """Offsets of == / != outside strings and preprocessor lines."""
    offs=[]; pos=0
    for line in src.split("\n"):
        s=line.lstrip()
        if not (s.startswith("#") or s.startswith("//")):
            holes=[]
            def stash(m):
                holes.append((m.start(),m.end())); return m.group(0)
            spans=[(m.start(),m.end()) for m in STR.finditer(line)]
            for m in re.finditer(r"==|!=", line):
                if not any(a<=m.start()<b for a,b in spans):
                    offs.append(pos+m.start())
        pos+=len(line)+1
    return offs


def mutate_at(src, off):
    """Flip the single ==/!= at absolute offset off."""
    cur=src[off:off+2]
    rep="!=" if cur=="==" else "=="
    return src[:off]+rep+src[off+2:]


def mutate(src):
    """LEGACY flip-all. Retained only to document the masking defect it caused."""
    out, n = [], 0
    for line in src.split("\n"):
        s = line.lstrip()
        if s.startswith("#") or s.startswith("//"):
            out.append(line)
            continue
        # protect string literals
        holes = []

        def stash(m):
            holes.append(m.group(0))
            return "\x00%d\x00" % (len(holes) - 1)

        p = STR.sub(stash, line)
        p, a = re.subn(r"==", "\x01", p)
        p, b = re.subn(r"!=", "==", p)
        p = p.replace("\x01", "!=")
        n += a + b
        for i, h in enumerate(holes):
            p = p.replace("\x00%d\x00" % i, h)
        out.append(p)
    return "\n".join(out), n


def build_run(src_text, tag, workdir):
    """Compile src_text and run it. Returns dict."""
    c = os.path.join(workdir, tag + ".c")
    b = os.path.join(workdir, tag)
    open(c, "w").write(src_text)
    cp = subprocess.run(["cc"] + CFLAGS + [c, "-o", b],
                        capture_output=True, timeout=120)
    if cp.returncode != 0:
        return {"compiled": False, "cc_err": cp.stderr.decode()[-300:]}
    env = dict(os.environ)
    env.setdefault("E2E_TMPDIR", workdir)
    env.setdefault("E2E_FIXTURE_DIR", workdir)
    env["LC_ALL"] = "C"
    env["TZ"] = "UTC"
    try:
        rp = subprocess.run([b], capture_output=True, timeout=10,
                            cwd=workdir, env=env, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return {"compiled": True, "timeout": True}
    return {"compiled": True, "timeout": False, "rc": rp.returncode,
            "out": hashlib.sha256(rp.stdout).hexdigest()[:16],
            "outlen": len(rp.stdout)}


def screen_one(path, max_sites=14):
    r = {"program": path}
    try:
        src = open(os.path.join(REPO, path), errors="replace").read()
    except OSError as e:
        return dict(r, bucket="NOT-DETERMINABLE", why="source unreadable: %s" % e)
    offs = sites_of(src)
    r["mutation_sites"] = len(offs)
    if not offs:
        return dict(r, bucket="NOT-DETERMINABLE",
                    why="conditional-negation mutation never applied (no ==/!= sites outside strings/preprocessor)")
    with tempfile.TemporaryDirectory(prefix="scr-") as wd:
        base = build_run(src, "base", wd)
        r["base"] = base
        if not base.get("compiled"):
            return dict(r, bucket="NOT-DETERMINABLE",
                        why="baseline does not compile standalone: " + base.get("cc_err", "")[:150])
        if base.get("timeout"):
            return dict(r, bucket="NOT-DETERMINABLE", why="baseline times out standalone (needs harness env)")
        if base.get("rc") != 0:
            return dict(r, bucket="NOT-DETERMINABLE",
                        why="baseline exits %s standalone (needs harness env); a mutation delta would not be attributable" % base.get("rc"))
        # one site at a time -- flipping all sites lets mutations cancel (measured: see controls)
        tried = 0
        applied_ok = 0
        for i, off in enumerate(offs[:max_sites]):
            m = build_run(mutate_at(src, off), "m%d" % i, wd)
            tried += 1
            if not m.get("compiled"):
                continue
            applied_ok += 1
            if m.get("timeout"):
                return dict(r, bucket="REFUSABLE", channel="timeout", sites_tried=tried,
                            why="a single-site mutant hangs where baseline exits 0 (observable via harness timeout)")
            if m.get("rc") != base.get("rc"):
                return dict(r, bucket="REFUSABLE", channel="status", sites_tried=tried,
                            why="single-site mutation changed exit status 0 -> %s" % m.get("rc"))
            if m.get("out") != base.get("out"):
                return dict(r, bucket="REFUSABLE", channel="stdout", sites_tried=tried,
                            why="single-site mutation left exit status 0 but changed observed stdout")
        if applied_ok == 0:
            return dict(r, bucket="NOT-DETERMINABLE", sites_tried=tried,
                        why="every single-site mutant failed to compile; mutation never actually applied")
        return dict(r, bucket="CANNOT-FAIL", sites_tried=tried, mutants_built=applied_ok,
                    why="%d single-site mutants compiled and ran; exit status AND observed stdout unchanged in every one" % applied_ok)


def main():
    rows = json.load(open(os.path.join(OUT, "population.json")))
    res = []
    todo = [r for r in rows if not r["cap"]]
    for i, r in enumerate(todo):
        prog = r["program"]
        if not prog:
            out = {"program": prog, "bucket": "NOT-DETERMINABLE",
                   "why": "manifest test has no program field; no fixture source to mutate"}
        elif not prog.endswith(".c"):
            out = {"program": prog, "bucket": "NOT-DETERMINABLE",
                   "why": "fixture is %s, outside this C-source mutation operator; not screened in this pass" % os.path.splitext(prog)[1]}
        else:
            out = screen_one(prog)
        out["id"] = r["id"]
        res.append(out)
        if i % 20 == 0:
            print("%d/%d" % (i, len(todo)), flush=True)
    json.dump(res, open(os.path.join(OUT, "results.json"), "w"), indent=1)
    from collections import Counter
    print(Counter(x["bucket"] for x in res))


if __name__ == "__main__":
    main()
