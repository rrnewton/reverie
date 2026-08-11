#!/usr/bin/env python3
"""Render the compat scorecard and the ranked red-cell target list."""

from __future__ import annotations

import argparse
import csv
import re
from collections import Counter, defaultdict

BACKEND_ORDER = ["ptrace", "kvm", "dbt", "sabre", "liteinst", "e9patch"]
# Owner backend priority (PROJECT_VISION + coordinator brief): ptrace is the
# REFERENCE, so a ptrace red is the most serious of all; then the flagship KVM,
# then DBT, then SaBRe/LiteInst.
BACKEND_WEIGHT = {"ptrace": 260, "kvm": 150, "dbt": 90, "sabre": 55,
                  "liteinst": 55, "e9patch": 30}


def norm_signature(row: dict) -> str:
    """Collapse a red into a root-cause-shaped signature so cells that one fix
    would clear are counted together."""
    tail = row.get("stderr_tail", "")
    for pat, label in [
        (r"-524 ENOTSUPP", "ENOTSUPP(-524) refusal"),
        (r"unexpected CPUID identity", "CPUID not virtualized"),
        (r"[Uu]nsupported syscall[^\\]*", "unsupported syscall"),
        (r"Operation is not supported", "operation not supported"),
        (r"exited in error", "first run errored"),
        (r"logs? differ|DETLOG|log-diff|divergen", "INFO/DETLOG divergence"),
        (r"stdout differ|stdout mismatch", "stdout mismatch"),
        (r"timed out|timeout", "timeout"),
        (r"panicked at", "hermit panic"),
        (r"Segmentation fault|SIGSEGV|signal 11", "guest SIGSEGV"),
    ]:
        if re.search(pat, tail):
            return label
    return row.get("outcome", "unknown")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--csv", required=True)
    ap.add_argument("--native", default="")
    ap.add_argument("--allow-both", default="")
    ap.add_argument("--top", type=int, default=12)
    args = ap.parse_args()

    rows = list(csv.DictReader(open(args.csv)))
    native = {}
    if args.native:
        for n in csv.DictReader(open(args.native)):
            native[n["cell_id"]] = n
    allow = {}
    if args.allow_both:
        for a in csv.DictReader(open(args.allow_both)):
            allow[(a["backend"], a["cell_id"])] = a
    for r in rows:
        r["native_rc"] = native.get(r["cell_id"], {}).get("native_rc", "")
        a = allow.get((r["backend"], r["cell_id"]), {})
        r["allow_both_class"] = a.get("allow_both_class", "")
        r["allow_both_logs"] = a.get("allow_both_logs", "")
        r["allow_both_tail"] = a.get("allow_both_stderr_tail", "")
        r["allow_both_cmd"] = a.get("allow_both_cmd", "")
        # Did hermit CHANGE the guest's outcome relative to running it bare?
        # native_rc uses Popen.wait() convention (negative == killed by signal).
        ge, gs = a.get("allow_both_guest_exit", ""), a.get("allow_both_guest_signal", "")
        hermit_status = ""
        if gs not in ("", "None", None):
            hermit_status = str(-int(gs))
        elif ge not in ("", "None", None):
            hermit_status = str(int(ge))
        r["hermit_status"] = hermit_status
        r["status_matches_native"] = (
            "yes" if hermit_status and hermit_status == r["native_rc"]
            else ("no" if hermit_status and r["native_rc"] else "?"))
    backends = [b for b in BACKEND_ORDER if any(r["backend"] == b for r in rows)]
    sha = rows[0]["hermit_sha"] if rows else "?"
    rsha = rows[0]["reverie_sha"] if rows else "?"

    print(f"# Compat scorecard — hermit {sha}")
    print(f"# reverie pin {rsha}")
    print(f"# artifact: {args.csv}")
    print()

    # ---- per-backend table -------------------------------------------------
    print("## Per-backend verdicts (denominator = 235 canonical cells/backend)")
    print()
    hdr = (f"| {'backend':9s} | {'PASS':>4s} | {'  L2':>4s} | {'  L1':>4s} | {'FAIL':>4s} | "
           f"{'NO-RES':>6s} | {'denom':>5s} | PASS/measurable |")
    print(hdr)
    print("|" + "-" * 11 + "|" + ("-" * 6 + "|") * 5 + "-" * 7 + "|" + "-" * 17 + "|")
    for b in backends:
        rs = [r for r in rows if r["backend"] == b]
        p = [r for r in rs if r["verdict"] == "PASS"]
        l2 = [r for r in p if r["assurance"] == "L2-canonical"]
        l1 = [r for r in p if r["assurance"] != "L2-canonical"]
        f = [r for r in rs if r["verdict"] == "FAIL"]
        n = [r for r in rs if r["verdict"] == "NO-RESULT"]
        meas = len(p) + len(f)
        print(f"| {b:9s} | {len(p):4d} | {len(l2):4d} | {len(l1):4d} | {len(f):4d} | "
              f"{len(n):6d} | {len(rs):5d} | {len(p)}/{meas} |")
    print()
    print("PASS = the determinism check that ran returned a positive verdict.")
    print("  L2 = --verify-strict canonical BitwiseInfoV1 INFO parity "
          "(verify-json bitwise_parity:true, nonzero compared_log_messages).")
    print("  L1 = a structurally weaker channel this backend cannot exceed today:")
    print("       kvm -> output-only (compare_logs:false, bitwise_parity:false)")
    print("       dbt -> exit-status + stdout + Detcore summary + guest-memory hash")
    print("              (--verify-json is always verdict:no_result on the DBT path)")
    print("FAIL = the cell did not pass. NO-RESULT = not measurable (source absent, "
          "timeout, backend unstaged, no verdict).")
    print()
    print("## AXIS 2 — how many FAILs are determinism failures at all?")
    print()
    print("`hermit run --verify` defaults to `--verify-allow success`: when run 1 does")
    print("not exit successfully hermit SKIPS run 2 and writes verdict:no_result, so no")
    print("comparison ever happens.  Every FAIL was therefore re-measured with")
    print("`--verify-allow both`.  A cell that then verifies is deterministic; only its")
    print("exit-status success criterion failed.  This does NOT change the PASS column.")
    print()
    print(f"| {'backend':9s} | {'FAIL':>4s} | {'det+same-as-native':>18s} | "
          f"{'det+outcome-changed':>19s} | {'genuine det red':>15s} |")
    print("|-----------|------|--------------------|---------------------|-----------------|")
    axis2 = {}
    for b in backends:
        fr = [r for r in rows if r["backend"] == b and r["verdict"] == "FAIL"]
        det = [r for r in fr if r["allow_both_class"].startswith("determinism-only-green")]
        same = [r for r in det if r["status_matches_native"] == "yes"]
        chg = [r for r in det if r["status_matches_native"] != "yes"]
        axis2[b] = (len(fr), len(same), len(chg), len(fr) - len(det))
        print(f"| {b:9s} | {len(fr):4d} | {len(same):18d} | {len(chg):19d} | "
              f"{len(fr)-len(det):15d} |")
    print()
    print("det+same-as-native  = deterministic under hermit AND the guest's exit status")
    print("                      is identical to running it bare. Not a hermit defect;")
    print("                      the cell just has a non-zero success contract.")
    print("det+outcome-changed = the two hermit runs agreed bitwise, but hermit changed")
    print("                      the guest's OUTCOME vs bare metal. Needs a human look")
    print("                      even though determinism held.")
    print("genuine det red     = no positive verdict even with the allowance widened.")
    print()

    # ---- no-result accounting ---------------------------------------------
    print("## NO-RESULT accounting (never counted red, never dropped)")
    print()
    nr = Counter()
    for r in rows:
        if r["verdict"] == "NO-RESULT":
            nr[(r["backend"], r["outcome"])] += 1
    for (b, o), c in sorted(nr.items()):
        print(f"  {b:9s} {o:22s} {c:4d}")
    print()
    print("  'absent'  = 30 performance/* sources that are NOT on hermit main at this SHA")
    print("              (they exist only on unmerged branches). Same 30 for every backend.")
    print("  'timeout' = the cell timed out in the parallel pass AND AGAIN on a serial")
    print("              re-run with the box otherwise quiet. That is a CONFIRMED HANG,")
    print("              not load flake. Reported as NO-RESULT because no determinism")
    print("              verdict exists, but it is an actionable defect -- see below.")
    print()
    hangs = [r for r in rows if r["outcome"] == "timeout"]
    if hangs:
        print("## Confirmed hangs (timed out in parallel AND on a quiet serial re-run)")
        print()
        byb = defaultdict(list)
        for r in hangs:
            byb[r["backend"]].append(r)
        for b in backends:
            if b in byb:
                print(f"  {b} ({len(byb[b])}): "
                      + ", ".join(sorted(r["cell_id"] for r in byb[b])))
        print()

    # ---- red-cell tier distribution ---------------------------------------
    print("## Red tier distribution (canonical vs stripped comparison)")
    print()
    for b in backends:
        t = Counter(r["tier"] for r in rows if r["backend"] == b and r["verdict"] == "FAIL")
        if t:
            print(f"  {b:9s} " + "  ".join(f"{k}={v}" for k, v in sorted(t.items())))
    print()
    print("  red-stripped        = diverges/fails even under the weak stripped "
          "--verify: a hard red.")
    print("  red-canonical-only  = only the bitwise --verify-strict comparison "
          "catches it: a subtler red.")
    print()

    # ---- failure clusters --------------------------------------------------
    print("## Failure clusters (one root cause may clear many cells)")
    print()
    clusters = defaultdict(list)

    # ---- ranked target list ------------------------------------------------
    passers = defaultdict(list)
    for r in rows:
        if r["verdict"] == "PASS":
            passers[r["cell_id"]].append(r["backend"])

    def red_class(r, b):
        if r["verdict"] == "NO-RESULT" and r["outcome"] == "timeout":
            return "confirmed-hang"
        # AXIS 1 first: if the cell is deterministic once --verify-allow is
        # widened, the "red" is the exit-status success criterion, NOT a
        # determinism failure.  Never charge that to the backend.
        if r.get("allow_both_class", "").startswith("determinism-only-green"):
            # Deterministic -- but did hermit CHANGE the guest's outcome?  If the
            # exit status under hermit equals the bare-metal status, the guest
            # behaves identically with and without hermit and the only thing that
            # failed is the exit-0 success criterion: not a red.  If it DIFFERS,
            # hermit altered the observable result and that needs a human look
            # even though the two hermit runs agreed with each other.
            return ("exit-contract-only" if r.get("status_matches_native") == "yes"
                    else "deterministic-but-outcome-differs-from-native")
        if [x for x in passers.get(r["cell_id"], []) if x != b]:
            return "backend-gap"          # some other backend runs it => isolatable
        if r.get("native_rc") == "0":
            return "hermit-gap"           # guest clean natively, red on every backend
        return "test-defect-suspect"      # also fails/hangs without hermit

    for r in rows:
        r["red_class"] = red_class(r, r["backend"]) if r["verdict"] == "FAIL" else ""
        if r["verdict"] == "FAIL":
            clusters[(r["backend"], norm_signature(r), r["red_class"])].append(r)
        elif r["outcome"] == "timeout":
            # A confirmed hang produces no verdict, so it is NO-RESULT in the
            # table -- but it is an actionable defect and must appear in the
            # target list, not vanish between the two columns.
            r["red_class"] = "confirmed-hang"
            clusters[(r["backend"], "CONFIRMED HANG (no verdict)",
                      "confirmed-hang")].append(r)
    for (b, sig, rc), rs in sorted(clusters.items(), key=lambda kv: -len(kv[1])):
        print(f"  {b:9s} {len(rs):4d} cells  {rc:20s} {sig}")
    print()

    scored = []
    for (b, sig, _rc), rs in clusters.items():
        # A cluster that other backends already pass is a backend-specific gap:
        # the guest itself is known-good under hermit, which makes it far more
        # tractable.  A cluster whose guests also fail NATIVELY is very likely a
        # test defect and must not be charged to the backend.
        others = sum(1 for r in rs if red_class(r, b) == "backend-gap")
        hermit_gap = sum(1 for r in rs if red_class(r, b) == "hermit-gap")
        suspect = sum(1 for r in rs if red_class(r, b) == "test-defect-suspect")
        cluster = len(rs)
        try:
            fastest = min(int(r["duration_ms"] or 0) for r in rs)
        except ValueError:
            fastest = 0
        contract = sum(1 for r in rs if red_class(r, b) == "exit-contract-only")
        if contract == cluster:  # every cell deterministic AND matching native
            continue                              # not a determinism red at all
        hangs = sum(1 for r in rs if red_class(r, b) == "confirmed-hang")
        score = (BACKEND_WEIGHT.get(b, 10)
                 + 12 * min(cluster, 8)
                 + 10 * min(others, 8)
                 + 10 * min(hermit_gap, 8)
                 + 40 * (hangs > 0)               # a hang blocks the cell outright
                 - 25 * (suspect == cluster)      # whole cluster is guest-side
                 + (10 if fastest and fastest < 5000 else 0))
        rs_sorted = sorted(rs, key=lambda r: (red_class(r, b) != "backend-gap",
                                              int(r["duration_ms"] or 0)))
        scored.append((score, b, sig, cluster, others, rs_sorted))
    scored.sort(key=lambda t: -t[0])

    print("## RANKED RED-CELL TARGET LIST")
    print()
    print("Rank key: owner backend priority (ptrace reference > kvm flagship > dbt >")
    print("sabre/liteinst) + cluster size (cells one fix clears) + how many of those")
    print("cells already PASS on another backend (guest known-good => backend-specific")
    print("gap) + fast repro.")
    print()
    for i, (score, b, sig, cluster, others, rs) in enumerate(scored[:args.top], 1):
        lead = rs[0]
        print(f"### {i}. [{b}] {sig}  — {cluster} cell(s), {others} of them green "
              f"elsewhere (score {score})")
        print(f"    lead cell     : {lead['cell_id']}  ({lead['lane']} lane, "
              f"{lead['duration_ms']}ms)")
        print(f"    verdict/tier  : {lead['outcome']} / {lead['tier']} / "
              f"{red_class(lead, b)}")
        print(f"    guest native  : rc={lead.get('native_rc','?')}   "
              f"green on: {','.join(x for x in passers.get(lead['cell_id'], []) if x != b) or 'no other backend'}")
        print(f"    observed      : {lead['stderr_tail'][-300:].strip()}")
        print(f"    repro         : {lead['repro_cmd']}")
        if cluster > 1:
            print(f"    other cells   : "
                  + ", ".join(f"{r['cell_id']}[{red_class(r, b)[:11]}]" for r in rs[1:9])
                  + (" ..." if cluster > 9 else ""))
        print()

    # ---- full red inventory ------------------------------------------------
    print("## Full red inventory (cell, backend, outcome, tier)")
    print()
    for b in backends:
        rs = [r for r in rows if r["backend"] == b and r["verdict"] == "FAIL"]
        if not rs:
            continue
        print(f"### {b} — {len(rs)} red")
        for r in sorted(rs, key=lambda r: r["cell_id"]):
            green_elsewhere = [x for x in passers.get(r["cell_id"], []) if x != b]
            print(f"  {r['cell_id']:50s} {r['outcome']:20s} {r['tier']:18s} "
                  f"{red_class(r, b):19s} nat={r.get('native_rc','?'):>4s} "
                  f"green-on: {','.join(green_elsewhere) or 'none'}")
        print()
    print("red_class: backend-gap = another backend PASSES this cell (isolatable);")
    print("           hermit-gap  = red on every backend but the guest exits 0 natively;")
    print("           test-defect-suspect = the guest also fails/hangs WITHOUT hermit,")
    print("                                 so the red is probably guest-side, not a backend gap.")


if __name__ == "__main__":
    main()
