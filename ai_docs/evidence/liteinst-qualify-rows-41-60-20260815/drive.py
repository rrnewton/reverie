#!/usr/bin/env python3
"""Qualify liteinst/verify cells: accept arm, bite proof, refuse arm, ptrace control."""
import re, subprocess, sys, shutil
from pathlib import Path

HERMIT = Path("/home/newton/work/dev-hermit/hermit/target/release/hermit")
SRC    = Path("/home/newton/work/dev-hermit/hermit")
WORK   = Path("/home/newton/work/dev-hermit/ignored/liteinst-qualify-20260815/cells")
OUT    = Path("/home/newton/work/dev-hermit/ignored/liteinst-qualify-20260815/results.tsv")
WORK.mkdir(parents=True, exist_ok=True)

def sh(argv, cwd=None, timeout=240):
    try:
        p = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "TIMEOUT"

def text_section(binary: Path) -> str:
    rc, out, _ = sh(["objdump", "-d", "--section=.text", str(binary)])
    # strip addresses so only instruction content is compared
    return "\n".join(re.sub(r"^\s*[0-9a-f]+:", "", l) for l in out.splitlines()) if rc == 0 else ""

MANIFEST = SRC / "tests/e2e/manifests/c-programs.toml"

def cell_cflags(test_id: str) -> list[str]:
    """Per-cell build.cflags. Omitting them makes a buildable cell look unbuildable:
    two rows here carry -pthread, without which they do not link."""
    body = MANIFEST.read_text()
    start = body.find(f'id = "{test_id}"')
    if start < 0:
        return []
    nxt = body.find("[[test]]", start)
    block = body[start: nxt if nxt > 0 else len(body)]
    m = re.search(r"cflags\s*=\s*\[(.*?)\]", block, re.S)
    return re.findall(r'"([^"]+)"', m.group(1)) if m else []

def prints_usage(text: str) -> bool:
    """A usage line at the start of a line -- not a substring match, which would
    hit `getrusage` and misfile a real cell as an invocation error."""
    return any(l.strip().lower().startswith("usage:") for l in text.splitlines())

def cell_cmd(backend, guest, report):
    return [str(HERMIT), "--log=info", "run", "--backend", backend, "--strict",
            "--verify", "--verify-json", str(report), "--", str(guest)]

rows = [l.rstrip("\n").split("\t") for l in open("/tmp/myslice.tsv") if l.strip()]
with open(OUT, "a") as log:
    for idx, row in enumerate(rows, start=41):
        backend, mode, cell, fixture, family, evidence = row[:6]
        d = WORK / f"{idx:02d}"; shutil.rmtree(d, ignore_errors=True); d.mkdir(parents=True)
        src = SRC / fixture
        orig_c, mut_c = d / "orig.c", d / "mut.c"
        shutil.copy(src, orig_c)
        body = src.read_text()

        # BEHAVIOUR mutation: swap the first probed syscall for a supported one.
        m = next((x for x in re.finditer(r"syscall\(\s*(SYS_[A-Za-z0-9_]+)", body)
                  if x.group(1) != "SYS_getpid"), None)
        if not m:
            log.write(f"{idx}\t{cell}\tNOT-DETERMINABLE\tno mutation pattern matched\t-\t-\t-\t-\t-\n"); log.flush(); continue
        token = m.group(1)
        mut_c.write_text(body.replace(token, "SYS_getpid"))

        cflags = cell_cflags(cell)
        rc_o, _, e_o = sh(["gcc", "-O0", *cflags, "-o", str(d/"orig"), str(orig_c)])
        rc_m, _, e_m = sh(["gcc", "-O0", *cflags, "-o", str(d/"mut"),  str(mut_c)])
        if rc_o or rc_m:
            log.write(f"{idx}\t{cell}\tNOT-DETERMINABLE\tbuild failed orig={rc_o} mut={rc_m}\t{token}\t-\t-\t-\t-\n"); log.flush(); continue

        bit = "BIT" if text_section(d/"orig") != text_section(d/"mut") else "INERT"
        if bit == "INERT":
            log.write(f"{idx}\t{cell}\tNOT-DETERMINABLE\tmutation inert: .text identical\t{token}\t-\t-\t-\t-\n"); log.flush(); continue

        res, txt = {}, {}
        for arm, guest in (("accept", d/"orig"), ("refuse", d/"mut")):
            for be in ("liteinst", "ptrace"):
                rc, o, e = sh(cell_cmd(be, guest, d/f"v-{arm}-{be}.json"))
                res[(arm, be)] = rc
                txt[(arm, be)] = o + e
                (d/f"out-{arm}-{be}.txt").write_text(o + e)

        al, ap = res[("accept","liteinst")], res[("accept","ptrace")]
        rl, rp = res[("refuse","liteinst")], res[("refuse","ptrace")]
        # A fixture printing a usage line needs arguments the manifest records as
        # none. That is MY invocation being wrong, not a property of the cell.
        if prints_usage(txt[("accept","liteinst")]) or prints_usage(txt[("accept","ptrace")]):
            verdict, why = "NOT-DETERMINABLE", "fixture requires args; manifest declares none -- my invocation, not the cell"
        # A mutant killed by a signal is not the fixture expressing failure.
        elif rl >= 128 or rp >= 128:
            verdict, why = "NOT-DETERMINABLE", f"mutant died by signal liteinst={rl} ptrace={rp}"
        elif al == 0 and rl != 0:
            verdict, why = "QUALIFIED-LITEINST", "accept passes and behaviour mutant refused, both on liteinst"
        elif al != 0 and ap == 0 and rp != 0:
            verdict, why = ("QUALIFIED-PTRACE-ONLY",
                            f"both arms pass on the PTRACE reference; accept arm FAILS on liteinst (rc={al}). "
                            "NOT demonstrated to accept under liteinst and must not be enabled there on this evidence")
        elif al != 0 and ap != 0:
            verdict, why = "NOT-QUALIFIED", f"accept arm fails on BOTH backends (liteinst={al} ptrace={ap}); any refuse result is meaningless"
        else:
            verdict, why = "NOT-QUALIFIED", f"mutant not refused (liteinst rc={rl} ptrace rc={rp})"
        log.write(f"{idx}\t{cell}\t{verdict}\t{why}\t{token}\t{bit}\t{al}\t{rl}\t{ap}/{rp}\t{" ".join(cflags) or "-"}\n"); log.flush()
        print(f"{idx} {cell} {verdict} accept_li={al} refuse_li={rl} ptrace={ap}/{rp}", flush=True)
