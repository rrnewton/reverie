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
def case(name, expect_state, expect_stale, runs, jobs_of=None, successor_of=None):
    st, stale, ev = uh.classify(runs, now=NOW, jobs_of=jobs_of, successor_of=successor_of)
    ok = (st, stale) == (expect_state, expect_stale)
    cases.append((ok, name, f"{st} stale={stale} ev={ev}"))

# --- the five states, each named exactly once ---------------------------------
case("no runs at all -> NEVER_RAN",              uh.NEVER_RAN,      False, [])
# *** THE CAUSE-LAYER CORRECTION. *** A reviewer found that the first version
# returned SUPERSEDED whenever jobs==0 OR the jobs query FAILED, while the reason
# text asserted "superseded by a newer push" -- reintroducing absence-as-verdict
# one layer down, in the CAUSE rather than the STATE. Zero jobs also admits a
# manual or pre-start cancellation; a failed query admits an executed cancel.
# Supersession is now asserted ONLY on visible successor evidence.
case("cancelled, ZERO jobs, NO successor -> NOT superseded",
     uh.CANCELLED_UNVERIFIED, False, [run("cancelled")], jobs0)
case("cancelled, ZERO jobs, successor VISIBLE -> SUPERSEDED",
     uh.SUPERSEDED, False, [run("cancelled")], jobs0, (lambda r: "run 999 created 2026-08-10T02:14:12Z"))
case("cancelled, jobs query FAILS -> UNVERIFIED, never a cause",
     uh.CANCELLED_UNVERIFIED, False, [run("cancelled")],
     (lambda r: (_ for _ in ()).throw(RuntimeError("api down"))))
case("cancelled WITH jobs -> EVALUATED_FAIL",    uh.EVALUATED_FAIL, False, [run("cancelled")], jobsN)
case("fresh pending -> PENDING, not stale",      uh.PENDING,        False, [run(None,"in_progress",10)])
case("old pending -> PENDING, STALE",            uh.PENDING,        True,  [run(None,"in_progress",5000)])
case("all success -> EVALUATED_PASS",            uh.EVALUATED_PASS, False, [run("success")])
case("a real failure -> EVALUATED_FAIL",         uh.EVALUATED_FAIL, False, [run("failure")])

# --- THE REGRESSION THIS EXISTS FOR ------------------------------------------
# A run with no verdict must NEVER read as the good state.
for label, r in (("queued", run(None,"queued",10)), ("in_progress", run(None,"in_progress",10))):
    st,_,_ = uh.classify([r], now=NOW)
    cases.append((st != uh.EVALUATED_PASS,
                  f"a {label} run must NOT read as EVALUATED_PASS (the old defect)", st))

# --- refusing to guess when job counts are unavailable ------------------------
case("cancelled with NO jobs resolver -> UNVERIFIED, never PASS",
     uh.CANCELLED_UNVERIFIED, False, [run("cancelled")], None)

# --- precedence: a real failure outranks a pending sibling --------------------
case("failure + pending -> EVALUATED_FAIL wins",
     uh.EVALUATED_FAIL, False, [run("failure"), run(None,"in_progress",10)])

# The reason text must not assert a cause it did not observe.
st, stale, ev = uh.classify([run("cancelled")], now=NOW, jobs_of=jobs0)
r_unver = uh.likely_reason({"mergeable": "MERGEABLE"}, st, stale, ev)
cases.append(("CAUSE IS NOT ESTABLISHED" in r_unver and "will not guess" in r_unver,
              "unverified cancel REASON says the cause is not established", r_unver[:60]))
cases.append(("superseded by a newer push under queue-depth-1" not in r_unver,
              "unverified cancel REASON does NOT assert supersession as fact", "ok"))
st2, stale2, ev2 = uh.classify([run("cancelled")], now=NOW, jobs_of=jobs0,
                               successor_of=(lambda r: "run 999"))
r_ver = uh.likely_reason({"mergeable": "MERGEABLE"}, st2, stale2, ev2)
cases.append(("OBSERVED here rather than inferred" in r_ver and "run 999" in r_ver,
              "VERIFIED supersession names its successor evidence", r_ver[:60]))

# --- owner directive awareness changes reporting, never the classifier -------
DIRECTIVE = "deactivate-gh-actions-on-hermit-main-and-dev-hermit-parent"
disabled_conflict = uh.likely_reason(
    {"mergeable": "CONFLICTING"}, uh.NEVER_RAN, False,
    actions_enabled=False, actions_disabled_directive=DIRECTIVE,
)
cases.append(("NEVER_RAN is EXPECTED" in disabled_conflict
              and "REMEDY: resolve the conflict" in disabled_conflict
              and "REMEDY: dispatch" not in disabled_conflict,
              "Actions off: NEVER_RAN is expected, while conflict keeps its remedy",
              disabled_conflict))
disabled_mergeable = uh.likely_reason(
    {"mergeable": "MERGEABLE"}, uh.NEVER_RAN, False,
    actions_enabled=False, actions_disabled_directive=DIRECTIVE,
)
cases.append(("NEVER_RAN is EXPECTED" in disabled_mergeable
              and "REMEDY:" not in disabled_mergeable,
              "Actions off: mergeable NEVER_RAN emits no contradictory remedy",
              disabled_mergeable))
enabled_mergeable = uh.likely_reason(
    {"mergeable": "MERGEABLE"}, uh.NEVER_RAN, False,
    actions_enabled=True,
)
cases.append(("cause is NOT established" in enabled_mergeable
              and "REMEDY: dispatch" in enabled_mergeable,
              "Actions on: mergeable NEVER_RAN remains actionable",
              enabled_mergeable))

old_gh_json = uh.gh_json
try:
    uh.gh_json = lambda args: {"enabled": False}
    disabled_policy = uh.actions_policy("rrnewton/hermit")
    cases.append((disabled_policy == (False, DIRECTIVE),
                  "live disabled permission binds to the standing directive",
                  str(disabled_policy)))
    disabled_without_directive = uh.actions_policy("rrnewton/reverie")
    cases.append((disabled_without_directive == (False, None),
                  "disabled permission without a named directive stays actionable",
                  str(disabled_without_directive)))

    uh.gh_json = lambda args: {}
    try:
        uh.actions_policy("rrnewton/hermit")
        permission_failed_closed = False
    except RuntimeError:
        permission_failed_closed = True
    cases.append((permission_failed_closed,
                  "missing live permission cannot be treated as Actions off",
                  str(permission_failed_closed)))
finally:
    uh.gh_json = old_gh_json

# --- live-reader brackets: absence requires TWO exact-head readers ------------
FULL = "348a8b686262f6cb4aa4dc70c6a5e4613eb90361"

def collect_with(fake):
    old = uh.gh_json
    uh.gh_json = fake
    try:
        return uh.collect_live("rrnewton/hermit", 1, actions_enabled=True)[0]
    finally:
        uh.gh_json = old

def pr_list(head=FULL):
    return [{"number": 2123, "headRefOid": head, "mergeable": "MERGEABLE",
             "isDraft": True, "title": "fixture"}]

def abbreviated_reader(args):
    target = args[-1]
    if args[:2] == ["pr", "list"]:
        return pr_list(FULL[:12])
    if target.endswith("/pulls/2123"):
        return {"head": {"sha": FULL}}
    if "actions/runs?head_sha=" in target:
        # Querying the displayed 12-hex prefix returns zero; canonicalising to
        # the full SHA finds the terminal failure.
        if f"head_sha={FULL}&" not in target:
            return {"workflow_runs": []}
        return {"workflow_runs": [{"id": 31469903248,
                                    "status": "completed",
                                    "conclusion": "failure"}]}
    raise AssertionError(args)

short_row = collect_with(abbreviated_reader)
cases.append((short_row["head"] == FULL and short_row["run_count"] == 1
              and short_row["state"] == uh.EVALUATED_FAIL,
              "abbreviated listed head is canonicalised before querying runs",
              str(short_row)))

def missed_run_list(args):
    target = args[-1]
    if args[:2] == ["pr", "list"]:
        return pr_list()
    if "actions/runs?head_sha=" in target:
        return {"workflow_runs": []}
    if "/check-runs?" in target:
        return {"check_runs": [{"id": 99, "status": "completed",
                                 "conclusion": "failure",
                                 "app": {"slug": "github-actions"}}]}
    raise AssertionError(args)

fallback_row = collect_with(missed_run_list)
cases.append((fallback_row["run_count"] == 0
              and fallback_row["check_run_count"] == 1
              and fallback_row["state"] == uh.EVALUATED_FAIL
              and fallback_row["evidence"].get("reader") == "exact-commit-check-runs",
              "run-list zero plus exact-commit failure is EVALUATED_FAIL",
              str(fallback_row)))

def missed_pending_run(args):
    target = args[-1]
    if args[:2] == ["pr", "list"]:
        return pr_list()
    if "actions/runs?head_sha=" in target:
        return {"workflow_runs": []}
    if "/check-runs?" in target:
        return {"check_runs": [{"id": 100, "status": "in_progress",
                                 "conclusion": None,
                                 "started_at": "2026-08-10T00:00:00Z",
                                 "app": {"slug": "github-actions"}}]}
    raise AssertionError(args)

pending_row = collect_with(missed_pending_run)
cases.append((pending_row["state"] == uh.PENDING and pending_row["stale"],
              "fallback pending check preserves stale-window enforcement",
              str(pending_row)))

def genuinely_never_ran(args):
    target = args[-1]
    if args[:2] == ["pr", "list"]:
        return pr_list()
    if "actions/runs?head_sha=" in target:
        return {"workflow_runs": []}
    if "/check-runs?" in target:
        return {"check_runs": []}
    raise AssertionError(args)

zero_row = collect_with(genuinely_never_ran)
cases.append((zero_row["run_count"] == 0 and zero_row["check_run_count"] == 0
              and zero_row["state"] == uh.NEVER_RAN,
              "both exact-head readers empty remains NEVER_RAN",
              str(zero_row)))

p = sum(1 for ok,_,_ in cases if ok); f = len(cases)-p
for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
print(f"assertions: {p} passed, {f} failed")
sys.exit(1 if f else 0)
