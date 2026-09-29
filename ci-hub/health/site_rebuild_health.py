#!/usr/bin/env python3
"""Warn when the compatibility website's Actions rebuild fails, and keep it building.

WHY THIS EXISTS. Owner directive, 2026-09-28: the website build always happens,
and when the nightly Actions rebuild fails, tick-hub warns the coordinator.

The rebuild is hermit's `docs.yml`. It has a `schedule:` trigger (cron
"23 8 * * *") gated on `vars.COMPATIBILITY_NIGHTLY_ENABLED == 'true'`, and that
variable is already 'true'. The remaining gap is GitHub itself: scheduled runs
are best-effort, start hours late (the last five started between 13:14Z and
16:50Z against an 08:23Z cron), and GitHub is documented to drop them outright.
Nothing on our side noticed either a failed rebuild or a missing one.

WHAT IT MEASURES. The newest docs.yml runs, read from the Actions API. A run is
a REBUILD ATTEMPT when it is a scheduled run that was not skipped, or when one
of the rebuild-gated steps (the same `if:` the workflow uses) actually ran. A
plain manual docs dispatch republishes the site without reading the ledger and
is not counted.

    current   the newest completed rebuild attempt succeeded, and a successful
              rebuild finished within --stale-hours.
    failed    the newest completed rebuild attempt did not succeed.
    stale     no successful rebuild finished within --stale-hours, and the newest
              attempt did not fail (the schedule was dropped or never admitted).
    unknown   the Actions API could not be read. Exit 75, never "current".

WHAT IT DOES ABOUT IT. With --dispatch, a `failed` or `stale` site whose last
successful rebuild is older than --stale-hours is rebuilt:
`gh workflow run docs.yml --ref main -f rebuild_compatibility=true`. That is
the owner-authorized website refresh the workflow already offers; it runs no
Hermit tests. It is skipped while any docs.yml run is queued or in flight
(the workflow serializes publication), and at most one dispatch is made per
--min-dispatch-interval-hours, so a deterministic failure cannot turn into a
dispatch loop. A dispatch that GitHub refuses is itself a warning.

FIRES WHILE IT PERSISTS (site_gate_policy.py). With --gate the exit status is 1
on EVERY evaluation whose outcome is a warning: `failed:<run id>`,
`stale:<newest successful run id>`, either one suffixed `:dispatch-failed`
while GitHub refuses the rebuild dispatch. tick-hub evaluates it on EVERY
tick (cadence 0: every scheduled 30-minute tick, observed 1790-1811 s apart,
and every manual one), so an unfixed failure pages on every tick and each page
says since when, and no tick can skip it and enqueue kind=resolve while the
failure holds. This replaced a once-per-outcome-key policy under which a
failure that persisted warned only once. Returning to `current` is recorded
quietly. An unreadable Actions API exits 75 for the first two consecutive
evaluations and fires state `unknown` from the third. The state file is written atomically
under an exclusive lock, so two ticks cannot interleave a read and a write.

WHAT IT DOES NOT COVER. What the rebuild published is site_data_freshness.py's
question: a green rebuild that published old data reads `stale` there. That
gate also reads a stopped builder as `stale` on its own, once an unpublished
main run is more than its --rebuild-due-hours (36) old and more than 24 h
newer than the published data, including while this gate cannot read the
Actions API. A test in tests/test_site_data_freshness.py holds tick-hub.yaml
to configuring both gates at the same cadence.

    site_rebuild_health.py                              # report, exit 0
    site_rebuild_health.py --json                       # machine readable
    site_rebuild_health.py --gate --state S --dispatch  # the tick-hub form
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
import site_gate_policy as policy  # noqa: E402
from site_gate_policy import locked_state  # noqa: E402,F401  (tests use it)

QUIET = 0
FIRE = 1
UNKNOWN = 75  # tick-hub's NO_RESULT_EXIT: the gate ran and could not decide.

DEFAULT_REPO = "rrnewton/hermit"
WORKFLOW = "docs.yml"
SITE_URL = "https://rrnewton.github.io/hermit/compatibility/latest/"
# Both steps carry the workflow's rebuild condition
# `github.event_name == 'schedule' || inputs.rebuild_compatibility`. The access
# check runs first, so a rebuild that failed before reading the ledger is still
# an attempt.
REBUILD_GATED_STEPS = frozenset(
    {
        "Check nightly source access and pins",
        "Rebuild from the current published cell ledger",
    }
)
IN_FLIGHT = frozenset({"queued", "in_progress", "waiting", "pending", "requested"})
MAX_RUNS = 20
MAX_JOB_READS = 10

Runner = Callable[[list[str]], "tuple[int, str, str]"]


def _run(cmd: list[str], timeout: int = 120) -> tuple[int, str, str]:
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except (OSError, subprocess.SubprocessError) as exc:  # fail closed
        return 1, "", str(exc)


def _gh(args: list[str]) -> list[str]:
    # Egress on the tick hosts goes through fwdproxy; a scheduled unit inherits
    # no proxy variables, so the wrapper is applied here rather than by callers.
    return ["with-proxy", "gh", *args]


def _parse_time(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _iso(moment: dt.datetime) -> str:
    return moment.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def list_runs(repo: str, run: Runner = _run) -> list[dict] | str:
    rc, out, err = run(
        _gh(
            [
                "run", "list", "--repo", repo, "--workflow", WORKFLOW,
                "--limit", str(MAX_RUNS), "--json",
                "databaseId,event,status,conclusion,createdAt,updatedAt,headSha,url",
            ]
        )
    )
    if rc != 0:
        return f"could not list {WORKFLOW} runs: rc={rc} {err.strip()[:200]}"
    try:
        runs = json.loads(out)
    except ValueError as exc:
        return f"{WORKFLOW} run list did not parse: {exc}"
    if not isinstance(runs, list):
        return f"{WORKFLOW} run list was not a list"
    return runs


def run_steps(repo: str, run_id: int, run: Runner = _run) -> list[dict] | str:
    rc, out, err = run(_gh(["run", "view", str(run_id), "--repo", repo, "--json", "jobs"]))
    if rc != 0:
        return f"could not read jobs of run {run_id}: rc={rc} {err.strip()[:200]}"
    try:
        jobs = json.loads(out).get("jobs") or []
    except (ValueError, AttributeError) as exc:
        return f"jobs of run {run_id} did not parse: {exc}"
    return [step for job in jobs for step in (job.get("steps") or [])]


def is_rebuild_attempt(run_record: dict, steps: list[dict]) -> bool:
    if run_record.get("event") == "schedule" and run_record.get("conclusion") != "skipped":
        return True
    return any(
        step.get("name") in REBUILD_GATED_STEPS
        and step.get("conclusion") not in (None, "", "skipped")
        for step in steps
    )


def measure(
    repo: str,
    now: dt.datetime,
    stale_hours: float,
    run: Runner = _run,
) -> dict:
    runs = list_runs(repo, run)
    if isinstance(runs, str):
        return {"state": "unknown", "reason": runs}
    runs = sorted(runs, key=lambda r: r.get("createdAt") or "", reverse=True)
    in_flight = [r for r in runs if r.get("status") in IN_FLIGHT]

    newest_attempt: dict | None = None
    newest_success: dict | None = None
    job_reads = 0
    for record in runs:
        if record.get("status") != "completed":
            continue
        if newest_attempt is not None and newest_success is not None:
            break
        if job_reads >= MAX_JOB_READS:
            break
        steps = run_steps(repo, int(record["databaseId"]), run)
        job_reads += 1
        if isinstance(steps, str):
            return {"state": "unknown", "reason": steps}
        if not is_rebuild_attempt(record, steps):
            continue
        if newest_attempt is None:
            newest_attempt = record
        if record.get("conclusion") == "success" and newest_success is None:
            newest_success = record

    success_age_hours = None
    if newest_success is not None:
        finished = _parse_time(newest_success.get("updatedAt"))
        if finished is not None:
            success_age_hours = round((now - finished).total_seconds() / 3600.0, 2)

    result: dict = {
        "measured_at": _iso(now),
        "stale_hours": stale_hours,
        "runs_listed": len(runs),
        "job_reads": job_reads,
        "in_flight": [
            {"run_id": r.get("databaseId"), "event": r.get("event"), "status": r.get("status")}
            for r in in_flight
        ],
        "newest_attempt": _brief(newest_attempt),
        "newest_success": _brief(newest_success),
        "newest_success_age_hours": success_age_hours,
    }
    if newest_attempt is None and not runs:
        result.update(state="unknown", reason=f"{WORKFLOW} has no runs at all")
        return result
    if newest_attempt is not None and newest_attempt.get("conclusion") != "success":
        result["state"] = "failed"
    elif success_age_hours is None or success_age_hours >= stale_hours:
        result["state"] = "stale"
    else:
        result["state"] = "current"
    return result


def _brief(record: dict | None) -> dict | None:
    if record is None:
        return None
    return {
        "run_id": record.get("databaseId"),
        "event": record.get("event"),
        "conclusion": record.get("conclusion"),
        "created_at": record.get("createdAt"),
        "finished_at": record.get("updatedAt"),
        "head_sha": record.get("headSha"),
        "url": record.get("url"),
    }


def outcome_key(m: dict) -> str:
    state = m["state"]
    if state == "failed":
        return f"failed:{m['newest_attempt']['run_id']}"
    if state == "stale":
        success = m.get("newest_success") or {}
        return f"stale:{success.get('run_id') or 'none'}"
    return state


def should_dispatch(m: dict, state: dict, now: dt.datetime, min_interval_hours: float) -> str | None:
    """Return why a dispatch is skipped, or None when it should happen."""
    if m["state"] not in ("failed", "stale"):
        return "the site rebuilt recently"
    age = m.get("newest_success_age_hours")
    if age is not None and age < m["stale_hours"]:
        return f"the last successful rebuild is {age} h old, under {m['stale_hours']} h"
    if m["in_flight"]:
        return "a docs.yml run is already queued or running"
    last = _parse_time(state.get("last_dispatch_at"))
    if last is not None:
        since = (now - last).total_seconds() / 3600.0
        if since < min_interval_hours:
            return f"this gate dispatched {since:.1f} h ago (minimum {min_interval_hours} h)"
    return None


def dispatch(repo: str, run: Runner = _run) -> tuple[bool, str]:
    rc, out, err = run(
        _gh(
            [
                "workflow", "run", WORKFLOW, "--repo", repo, "--ref", "main",
                "-f", "rebuild_compatibility=true",
            ]
        )
    )
    if rc != 0:
        return False, f"gh workflow run refused: rc={rc} {err.strip()[:200]}"
    return True, (out.strip() or err.strip() or "dispatched")[:200]


def render(m: dict) -> str:
    # A warning names times, never ages, so an unchanged condition renders the
    # same text on every evaluation (site_gate_policy: THE PAGE TEXT IS STABLE).
    verdict = m.get("policy")
    if m["state"] == "unknown":
        text = f"site-rebuild: UNKNOWN -- {m['reason']}"
        if verdict:
            text = _with_persistence(text, verdict)
        return text
    attempt = m.get("newest_attempt") or {}
    success = m.get("newest_success") or {}
    age = m.get("newest_success_age_hours")
    if m["state"] == "current":
        text = (
            f"site-rebuild: CURRENT -- run {success.get('run_id')} ({success.get('event')}) "
            f"rebuilt {SITE_URL} {age} h ago"
        )
    elif m["state"] == "failed":
        text = (
            f"site-rebuild: FAILED -- docs.yml rebuild run {attempt.get('run_id')} "
            f"({attempt.get('event')}, head {str(attempt.get('head_sha'))[:10]}) concluded "
            f"{attempt.get('conclusion')}: {attempt.get('url')}. Last successful rebuild: "
            f"run {success.get('run_id') or 'none in window'}, finished "
            f"{success.get('finished_at') or 'never'}"
        )
    else:
        text = (
            f"site-rebuild: STALE -- no successful docs.yml rebuild in {m['stale_hours']} h; "
            f"the newest is run {success.get('run_id') or 'none in window'} (finished "
            f"{success.get('finished_at') or 'never'})"
        )
    dispatched = m.get("dispatch")
    if dispatched:
        text += f". Dispatch: {dispatched}"
    if verdict and m["state"] != "current":
        text = _with_persistence(text, verdict)
    return text


def _with_persistence(text: str, verdict: dict) -> str:
    """Put since-when right after the state, where a truncated capture keeps it."""
    head, sep, rest = text.partition(" -- ")
    return f"{head} ({policy.persistence_text(verdict)}){sep}{rest}"


def captured_summary(m: dict) -> str:
    """One inert line for tick-hub's key=value capture."""
    summary = " ".join(render(m).split())
    return summary.replace("{", "(").replace("}", ")")[:500]


def main(argv: list[str] | None = None, run: Runner = _run, now: dt.datetime | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--gate", action="store_true",
        help="exit 1 on every evaluation in a warning state, and on persistent unknown",
    )
    ap.add_argument("--state", type=Path, help="state file; required with --gate or --dispatch")
    ap.add_argument("--dispatch", action="store_true", help="rebuild a failed or stale site")
    ap.add_argument("--stale-hours", type=float, default=30.0)
    ap.add_argument("--min-dispatch-interval-hours", type=float, default=12.0)
    ap.add_argument(
        "--unknown-limit", type=int, default=policy.UNKNOWN_LIMIT,
        help="consecutive unknown evaluations before the gate itself warns",
    )
    args = ap.parse_args(argv)
    if (args.gate or args.dispatch) and args.state is None:
        ap.error("--gate and --dispatch need --state")
    if args.unknown_limit < 1:
        ap.error("--unknown-limit must be at least 1")

    now = now or dt.datetime.now(dt.timezone.utc)
    m = measure(args.repo, now, args.stale_hours, run)
    verdict: dict | None = None
    if args.state is not None:
        with locked_state(args.state) as state:
            if m["state"] == "unknown":
                key = policy.UNKNOWN_KEY
            else:
                key = outcome_key(m)
                if args.dispatch:
                    skipped = should_dispatch(m, state, now, args.min_dispatch_interval_hours)
                    if skipped is None:
                        ok, detail = dispatch(args.repo, run)
                        state["last_dispatch_at"] = _iso(now)
                        state["last_dispatch_ok"] = ok
                        state["last_dispatch_result"] = detail
                        m["dispatch"] = (
                            f"dispatched docs.yml rebuild_compatibility=true at {_iso(now)}"
                            if ok
                            else f"FAILED: {detail}"
                        )
                    else:
                        m["dispatch_skipped"] = skipped
                # A refused dispatch stays part of the outcome until a dispatch
                # is accepted or the site recovers, so the key does not flap
                # between ticks that retry and ticks that wait out the interval.
                if key == "current":
                    state.pop("last_dispatch_ok", None)
                elif state.get("last_dispatch_ok") is False:
                    key += ":dispatch-failed"
                state["state"] = m["state"]
            verdict = policy.evaluate(state, key, now, unknown_limit=args.unknown_limit)
            m["policy"] = verdict
            m["outcome_key"] = verdict["outcome_key"]
            m["previous_outcome_key"] = verdict.get("previous_outcome_key")

    if args.json:
        print(json.dumps(m, indent=2, sort_keys=True))
    else:
        # Only key=value lines: tick-hub captures every stdout line with '='.
        attempt = m.get("newest_attempt") or {}
        print(f"state={m['state']}")
        print(f"summary={captured_summary(m)}")
        print(f"run_id={attempt.get('run_id') or 'none'}")
        if m.get("outcome_key"):
            key = m["outcome_key"]
        else:
            key = "unknown" if m["state"] == "unknown" else outcome_key(m)
        print(f"outcome_key={key}")

    fire = bool(verdict and verdict["fire"])
    if args.gate and fire:
        return FIRE
    if m["state"] == "unknown":
        return UNKNOWN
    return QUIET


if __name__ == "__main__":
    sys.exit(main())
