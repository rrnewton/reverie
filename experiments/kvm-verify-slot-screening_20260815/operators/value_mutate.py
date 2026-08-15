#!/usr/bin/env python3
"""VALUE-mutation operator, companion to the check-mutation operator in screen.py.

Question it answers: if the backend returned a WRONG VALUE (not an error), would
this fixture's exit status move?  That is different from the question
screen.py answers ("does the outcome of some condition reach the exit status").

Mechanism: perturb by +1 an integer variable that reaches the fixture's stdout,
injected immediately AFTER the complete statement that writes it -- so that any
later check comparing it against an expected value WILL fire.  Injecting at the
printf instead would skip such a check and wrongly report "not value-gated".

Three-way outcome, kept strictly apart:
  VALUE-GATED      exit status moved under some value mutation
  NOT-VALUE-GATED  mutation demonstrably took effect (stdout changed) yet exit
                   status did not move
  NOT-DETERMINABLE mutation never applied / did not compile / baseline unusable
"""
import os, re, subprocess, tempfile, hashlib

REPO = "/home/newton/work/dev-hermit/worktrees/slot04/hermit"
CFLAGS = ["-std=gnu11", "-O1", "-w", "-D_GNU_SOURCE", "-pthread"]
CTRL = ("if", "for", "while", "switch", "else", "do")
INTISH = re.compile(r"\b(int|long|size_t|ssize_t|uint\d+_t|int\d+_t|unsigned|short)\b")


def observable_vars(src):
    """Identifiers passed to printf/puts/fputs(...,stdout) -- i.e. reaching stdout."""
    out = []
    for m in re.finditer(r"\b(printf|puts)\s*\(", src):
        i = m.end() - 1
        depth, j = 0, i
        while j < len(src):
            if src[j] == "(":
                depth += 1
            elif src[j] == ")":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        args = src[i + 1:j]
        args = re.sub(r'"(?:[^"\\]|\\.)*"', "", args)          # drop format string
        args = re.sub(r"\(\s*[A-Za-z_][\w \t*]*\)", "", args)   # drop casts
        for tok in re.findall(r"[A-Za-z_]\w*", args):
            if tok not in out and not re.match(r"^(unsigned|long|int|char|size_t|void)$", tok):
                out.append(tok)
    return out


def write_sites(src, var):
    """Line indices where var is written: assignment, compound assignment, or &var."""
    lines = src.split("\n")
    hits = []
    pat_assign = re.compile(r"\b%s\s*(=[^=]|\+=|-=|\*=|/=|\|=|&=)" % re.escape(var))
    pat_addr = re.compile(r"&\s*%s\b" % re.escape(var))
    for i, ln in enumerate(lines):
        s = ln.strip()
        if s.startswith("#") or s.startswith("//") or s.startswith("*"):
            continue
        # strip string literals: `printf("counter=%llu", ...)` is NOT a write to counter
        ln = re.sub(r'"(?:[^"\\]|\\.)*"', '""', ln)
        if pat_assign.search(ln) or pat_addr.search(ln):
            hits.append(i)
    return hits


def stmt_end(lines, i):
    """Index of the last line of the complete statement starting at line i."""
    txt = ""
    j = i
    while j < len(lines) and j < i + 40:
        txt += lines[j] + "\n"
        head = txt.strip()
        depth = txt.count("(") - txt.count(")")
        if depth <= 0:
            kw = head.split("(")[0].strip()
            if kw in CTRL:
                # consume the controlled body too
                after = txt[txt.rfind(")") + 1:]
                if "{" in after or (j + 1 < len(lines) and lines[j + 1].strip().startswith("{")):
                    k = j if "{" in after else j + 1
                    bal = 0
                    while k < len(lines):
                        bal += lines[k].count("{") - lines[k].count("}")
                        if bal <= 0 and "{" in "".join(lines[j:k + 1]):
                            return k
                        k += 1
                    return j
                if after.rstrip().endswith(";"):
                    return j
                k = j + 1
                while k < len(lines) and k < j + 10:
                    if lines[k].rstrip().endswith(";"):
                        return k
                    k += 1
                return j
            if ";" in txt:
                return j
        j += 1
    return i


def indent_of(line):
    return line[:len(line) - len(line.lstrip())]


def build_run(text, tag, wd):
    c, b = os.path.join(wd, tag + ".c"), os.path.join(wd, tag)
    open(c, "w").write(text)
    cp = subprocess.run(["cc"] + CFLAGS + [c, "-o", b], capture_output=True, timeout=120)
    if cp.returncode != 0:
        return {"ok": False, "err": cp.stderr.decode()[-200:]}
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    env.setdefault("E2E_TMPDIR", wd)
    env.setdefault("E2E_FIXTURE_DIR", wd)
    try:
        r = subprocess.run([b], capture_output=True, timeout=10, cwd=wd, env=env,
                           stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return {"ok": True, "timeout": True}
    return {"ok": True, "timeout": False, "rc": r.returncode,
            "sha": hashlib.sha256(r.stdout).hexdigest()[:12],
            "txt": r.stdout.decode(errors="replace")[:200]}


def screen_value(path, max_mutants=10):
    r = {"program": path}
    full = path if os.path.isabs(path) else os.path.join(REPO, path)
    try:
        src = open(full, errors="replace").read()
    except OSError as e:
        return dict(r, verdict="NOT-DETERMINABLE", why="unreadable: %s" % e)
    lines = src.split("\n")
    cands = observable_vars(src)
    plans = []
    for v in cands:
        # only perturb things that look integer-typed somewhere
        decl = [l for l in lines if re.search(r"\b%s\b" % re.escape(v), l) and INTISH.search(l)]
        if not decl:
            continue
        # EXCLUDE pointers: `struct foo *state` -- `state += 1` is pointer arithmetic,
        # which corrupts memory (SIGSEGV) or deadlocks. That is the mutation breaking the
        # fixture, not a value check firing. Measured on lock_free/thread_stress/
        # mmap_fork_shared/producer_consumer, all false VALUE-GATED before this filter.
        if any(re.search(r"\*\s*%s\b" % re.escape(v), l) for l in lines):
            continue
        # EXCLUDE array subscripts: `state->retries[args->id]` -- +1 writes out of bounds.
        if any(re.search(r"\[\s*[^\]]*\b%s\b[^\]]*\]" % re.escape(v), l) for l in lines):
            continue
        for i in write_sites(src, v):
            e = stmt_end(lines, i)
            plans.append((v, e))
    # dedupe, keep last-write and first-write per var
    seen, ordered = set(), []
    byvar = {}
    for v, e in plans:
        byvar.setdefault(v, []).append(e)
    for v, es in byvar.items():
        for e in sorted(set(es)):   # try EVERY write site, not just first/last
            if (v, e) not in seen:
                seen.add((v, e)); ordered.append((v, e))
    r["candidate_vars"] = cands
    r["mutant_plans"] = len(ordered)
    if not ordered:
        return dict(r, verdict="NOT-DETERMINABLE",
                    why="no integer variable reaching stdout with an identifiable write site; value mutation never applied")
    with tempfile.TemporaryDirectory(prefix="vm-") as wd:
        base = build_run(src, "base", wd)
        if not base.get("ok"):
            return dict(r, verdict="NOT-DETERMINABLE", why="baseline will not compile standalone")
        if base.get("timeout"):
            return dict(r, verdict="NOT-DETERMINABLE", why="baseline times out standalone")
        if base.get("rc") != 0:
            return dict(r, verdict="NOT-DETERMINABLE",
                        why="baseline exits %s standalone (needs harness env)" % base.get("rc"))
        r["base_rc"] = base["rc"]; r["base_txt"] = base["txt"].strip()[:120]
        applied = 0
        for n, (v, e) in enumerate(ordered[:max_mutants]):
            ml = list(lines)
            ml.insert(e + 1, "%s%s += 1;" % (indent_of(lines[e]), v))
            m = build_run("\n".join(ml), "m%d" % n, wd)
            if not m.get("ok"):
                continue
            if m.get("timeout"):
                return dict(r, verdict="VALUE-GATED", var=v, channel="timeout",
                            why="value mutation on `%s` made the fixture hang" % v)
            if m.get("rc") != base["rc"]:
                return dict(r, verdict="VALUE-GATED", var=v, channel="status",
                            why="value mutation on `%s` moved exit status %s -> %s"
                                % (v, base["rc"], m["rc"]),
                            mut_txt=m["txt"].strip()[:120])
            if m.get("sha") != base.get("sha"):
                applied += 1
                r.setdefault("effective", []).append(
                    {"var": v, "base": base["txt"].strip()[:90], "mut": m["txt"].strip()[:90]})
        if applied == 0:
            return dict(r, verdict="NOT-DETERMINABLE",
                        why="every value mutant compiled but none changed stdout; the mutation did not take effect, so 'no exit change' proves nothing")
        return dict(r, verdict="NOT-VALUE-GATED", effective_mutants=applied,
                    why="%d value mutant(s) demonstrably changed the observed stdout, yet exit status stayed %s"
                        % (applied, base["rc"]))
