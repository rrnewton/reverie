#!/usr/bin/env python3
"""Bracket the FIVE states. Only one is good news, and two used to be misreported.

THE DEFECT THIS GUARDS, measured in this very file on 2026-08-10: the previous
three-state classifier had no PENDING, so a run that was still QUEUED or
IN PROGRESS had no failing conclusion, fell through, and returned
EVALUATED_PASS. The detector built to stop "absence reads as success" was
reporting an unfinished run as passed. It also bucketed every `cancelled` as
EVALUATED_FAIL, conflating supersession (a hole) with a real failure.

Supersession itself is CORRECT and is not what is being fixed -- cancelling an
obsolete run in favour of a newer one is right. Only the naming was wrong.
"""
import importlib.util, sys, time

spec = importlib.util.spec_from_file_location("uh", "ci-hub/health/unevaluated_head.py")
uh = importlib.util.module_from_spec(spec); spec.loader.exec_module(uh)

NOW = time.time()
def run(concl=None, status="completed", age=0):
    from datetime import datetime, timezone
    return {"status": status, "conclusion": concl, "id": 1,
            "created_at": datetime.fromtimestamp(NOW-age, timezone.utc).isoformat().replace("+00:00","Z")}

cases, jobs0, jobsN = [], (lambda r: 0), (lambda r: 36)
def case(name, expect_state, expect_stale, runs, jobs_of=None):
    st, stale = uh.classify(runs, now=NOW, jobs_of=jobs_of)
    ok = (st, stale) == (expect_state, expect_stale)
    cases.append((ok, name, f"{st} stale={stale}"))

# --- the five states, each named exactly once ---------------------------------
case("no runs at all -> NEVER_RAN",              uh.NEVER_RAN,      False, [])
case("cancelled with ZERO jobs -> SUPERSEDED",   uh.SUPERSEDED,     False, [run("cancelled")], jobs0)
case("cancelled WITH jobs -> EVALUATED_FAIL",    uh.EVALUATED_FAIL, False, [run("cancelled")], jobsN)
case("fresh pending -> PENDING, not stale",      uh.PENDING,        False, [run(None,"in_progress",10)])
case("old pending -> PENDING, STALE",            uh.PENDING,        True,  [run(None,"in_progress",5000)])
case("all success -> EVALUATED_PASS",            uh.EVALUATED_PASS, False, [run("success")])
case("a real failure -> EVALUATED_FAIL",         uh.EVALUATED_FAIL, False, [run("failure")])

# --- THE REGRESSION THIS EXISTS FOR ------------------------------------------
# A run with no verdict must NEVER read as the good state.
for label, r in (("queued", run(None,"queued",10)), ("in_progress", run(None,"in_progress",10))):
    st,_ = uh.classify([r], now=NOW)
    cases.append((st != uh.EVALUATED_PASS,
                  f"a {label} run must NOT read as EVALUATED_PASS (the old defect)", st))

# --- refusing to guess when job counts are unavailable ------------------------
case("cancelled with NO jobs resolver -> SUPERSEDED, never PASS",
     uh.SUPERSEDED, False, [run("cancelled")], None)

# --- precedence: a real failure outranks a pending sibling --------------------
case("failure + pending -> EVALUATED_FAIL wins",
     uh.EVALUATED_FAIL, False, [run("failure"), run(None,"in_progress",10)])

p = sum(1 for ok,_,_ in cases if ok); f = len(cases)-p
for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
print(f"assertions: {p} passed, {f} failed")
sys.exit(1 if f else 0)
