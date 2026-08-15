#!/usr/bin/env python3
"""Second refuse-arm attempt on cells the check-mutation arm could not fail.

Different mechanism: perturb by +1 an integer reaching the fixture's stdout,
injected right after its write, then run that violating copy under kvm
--strict --verify. A cell whose refuse arm is negative under BOTH operators is
much better evidence of "cannot be made to fail" than one operator alone; a cell
that refuses here was a false negative of the first arm.
"""
import json, os, sys, shutil, tempfile
sys.path.insert(0, "/home/newton/work/dev-hermit/ignored/kvm2-screen")
sys.path.insert(0, "/home/newton/work/dev-hermit/ignored/kvm2-qual")
from value_mutate import observable_vars, write_sites, stmt_end, indent_of, INTISH
import re
from qualify import cc, kvm, STAGE, REPO

def value_mutants(src):
    lines = src.split("\n"); out = []
    for v in observable_vars(src):
        if not any(re.search(r"\b%s\b" % re.escape(v), l) and INTISH.search(l) for l in lines):
            continue
        if any(re.search(r"\*\s*%s\b" % re.escape(v), l) for l in lines):      # pointers
            continue
        if any(re.search(r"\[\s*[^\]]*\b%s\b[^\]]*\]" % re.escape(v), l) for l in lines):  # indices
            continue
        for i in sorted(set(stmt_end(lines, w) for w in write_sites(src, v))):
            ml = list(lines); ml.insert(i + 1, "%s%s += 1;" % (indent_of(lines[i]), v))
            out.append((v, "\n".join(ml)))
    return out

res = []
for c in json.load(open(sys.argv[1])):
    r = dict(c)
    p = os.path.join(REPO, c["program"]); src = open(p, errors="replace").read()
    muts = value_mutants(src)
    r["value_mutants"] = len(muts)
    if not muts:
        r.update(refuse2="NOT-MEASURED", why="no integer reaching stdout with a usable write site; value mutation never applied")
    else:
        os.makedirs(STAGE, exist_ok=True); wd = tempfile.mkdtemp(prefix="r2-", dir=STAGE)
        try:
            built = 0; r["refuse2"] = "NO-REFUSAL"
            r["why"] = "value mutants ran under kvm and none moved the verdict"
            for i, (v, text) in enumerate(muts[:8]):
                b = os.path.join(wd, "v%d" % i)
                if not cc(text, b): continue
                built += 1
                out = kvm(b, wd, timeout=90)
                if out["timeout"] or out["rc"] != 0:
                    r.update(refuse2="REFUSES", var=v, rc=out["rc"], timeout=out["timeout"],
                             why="value mutation on `%s` made the cell fail under kvm (rc=%s timeout=%s)" % (v, out["rc"], out["timeout"]))
                    break
            r["mutants_built"] = built
            if built == 0:
                r.update(refuse2="NOT-MEASURED", why="no value mutant compiled; mutation never applied")
        finally:
            shutil.rmtree(wd, ignore_errors=True)
    res.append(r); print("%-46s %s" % (c["id"][:44], r["refuse2"]), flush=True)
    json.dump(res, open(sys.argv[2], "w"), indent=1)
