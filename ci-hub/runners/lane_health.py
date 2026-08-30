#!/usr/bin/env python3
"""Durable self-hosted capacity record + an observable, self-bounding throttle.

WHY THIS EXISTS
    ci-hub's queue-health check DETECTS observed self-hosted capacity pressure
    (missing configured capacity, or zero idle runners with runs queued -- see
    ``queue_health.binding_constraint``). It does not query queued-job labels,
    so it deliberately does not claim which label is binding. Detection alone
    lived only in an
    agent's working context and in a one-shot coordinator hard-warn message.
    Neither survives agent recycling, and we recycle agents many times a day,
    several at 0% context. An alarm that lives in a thought is not an alarm.

    This module makes the saturation signal DURABLE (append-only JSONL), turns "sustained
    saturation" into a machine-readable predicate a load-throttling consumer can
    read WITHOUT a GitHub call, and keeps that throttle OBSERVABLE and
    SELF-BOUNDING.

DESIGN PROPERTIES (each earned from an incident, per the owner's directive)
  * SURVIVES RECYCLING -- state is on disk, not in an agent context.
  * OBSERVABLE + REVERSIBLE -- ``throttle-check`` states WHEN it engages, WHY,
    and exactly WHAT it suppresses. A silent throttle is indistinguishable from
    a broken scheduler; "CI seems slow today" with no explanation costs hours.
  * SELF-BOUNDING -- the throttle names what un-throttles it and FAILS OPEN on a
    stale signal. A throttle that cannot turn itself off is a new outage.
  * NEVER GATES LANDING -- landing is what DRAINS the queue; blocking landings
    during saturation would make saturation permanent (throttle the cure, leave
    the cause running). The throttle only ever downgrades NEW speculative load.
  * HOST SIGNAL IS load-probe, NOT load average -- an observation records the
    ci-hub load-probe verdict (executing-CPU% vs policy), never 1-min loadavg,
    which overstates demand ~3x on a big, mostly-idle box.

INTERFACES
  tick            producer: fetch saturation (bounded gh) + host verdict, append
                  one durable observation, exit 1 iff SUSTAINED (fires the
                  ``ci_lane_saturation`` hard-warn), exit 2 iff the check could
                  not measure (surfaced, never silent), else 0.
  throttle-check  read-only predicate (NO gh, NO probe): reads the durable store
                  and exits 3 iff the throttle is engaged, else 0. This is the
                  stable interface the fleet load-planner consumes.
  status          human dump of the latest observation per repo.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import sys
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = 2

# A single saturated reading can be a momentary supersession blip; only a streak
# is worth throttling on. Default 3 consecutive observations x 900s cadence
# ~= 45 min of continuous saturation before the throttle engages.
DEFAULT_SUSTAINED_TICKS = int(os.environ.get("CI_HUB_LANE_SUSTAINED_TICKS", "3"))

# The throttle's OWN BOUND: if the most recent observation is older than this,
# the signal is stale and the throttle FAILS OPEN. 2x the default cadence.
DEFAULT_STALE_AFTER_SECS = float(os.environ.get("CI_HUB_LANE_STALE_AFTER", "1800"))

# Short host sample for the tick's load-probe reading.
DEFAULT_PROBE_SAMPLE_SECS = float(os.environ.get("CI_HUB_LANE_PROBE_SAMPLE", "1.0"))

# Bounded per-gh-call timeout so `tick` resolves under tick-hub's 30s guillotine
# (<=2 gh calls/repo + a ~1s probe).
DEFAULT_PER_CALL_TIMEOUT = float(os.environ.get("CI_HUB_LANE_GH_TIMEOUT", "10"))

# throttle-check exit codes: 0 = proceed (clear or fail-open), 3 = throttled.
EXIT_CLEAR = 0
EXIT_THROTTLED = 3

_CI_HUB = Path(__file__).resolve().parents[1]
_PARENT_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", _CI_HUB.parent)).resolve()


# --- lazy by-path imports (no sys.path pollution / name clashes) --------------
def _load_module(relpath: str, name: str):
    import importlib.util
    path = _CI_HUB / relpath
    spec = importlib.util.spec_from_file_location(name, str(path))
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    # Register before exec: a module using `from __future__ import annotations`
    # + @dataclass needs its own name resolvable in sys.modules during class
    # construction (KW_ONLY detection), else dataclass() raises AttributeError.
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _queue_health():
    return _load_module("runners/queue_health.py", "ci_hub_queue_health")


def _load_probe():
    return _load_module("health/load_probe.py", "ci_hub_load_probe")


def _history_query():
    return _load_module("history/query.py", "ci_hub_history_query")


def _now() -> tuple[float, str]:
    n = time.time()
    return n, datetime.fromtimestamp(n, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def store_path(explicit: str | None = None) -> Path:
    if explicit:
        return Path(explicit)
    env = os.environ.get("CI_HUB_LANE_HEALTH_STORE")
    if env:
        return Path(env)
    return _PARENT_ROOT / "ignored" / "ci-hub" / "lane-health.jsonl"


@dataclass
class Observation:
    repo: str
    observed_at: str
    observed_epoch: float
    saturated: bool
    reason: str
    consecutive_saturated: int
    streak_since: str | None
    sustained: bool
    host_suitable: bool | None
    host_note: str
    green_pct: float | None
    green_hours: float | None = None
    green_authoritative_run_hours: float | None = None
    green_window_start: str | None = None
    green_window_end: str | None = None
    schema: int = SCHEMA_VERSION


# --- durable store ------------------------------------------------------------
def _read_records(fh) -> list[dict]:
    fh.seek(0)
    out: list[dict] = []
    for line in fh:
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue  # tolerate a torn/partial trailing line
        if isinstance(rec, dict) and rec.get("repo"):
            out.append(rec)
    return out


def _latest_by_repo(records: list[dict]) -> dict[str, dict]:
    latest: dict[str, dict] = {}
    for rec in records:  # append order == time order; last wins
        latest[rec["repo"]] = rec
    return latest


def _to_obs(rec: dict) -> Observation:
    return Observation(
        repo=rec["repo"],
        observed_at=rec.get("observed_at", ""),
        observed_epoch=float(rec.get("observed_epoch", 0.0)),
        saturated=bool(rec.get("saturated", False)),
        reason=rec.get("reason", ""),
        consecutive_saturated=int(rec.get("consecutive_saturated", 0)),
        streak_since=rec.get("streak_since"),
        sustained=bool(rec.get("sustained", False)),
        host_suitable=rec.get("host_suitable"),
        host_note=rec.get("host_note", ""),
        green_pct=rec.get("green_pct"),
        green_hours=rec.get("green_hours"),
        green_authoritative_run_hours=rec.get(
            "green_authoritative_run_hours"),
        green_window_start=rec.get("green_window_start"),
        green_window_end=rec.get("green_window_end"),
        schema=int(rec.get("schema", SCHEMA_VERSION)),
    )


def latest_observation(path: Path, repo: str) -> Observation | None:
    """Latest durable observation for ``repo`` (LOCK_SH), or None. Survives a
    fresh process -- this is the recycle-durable read."""
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as fh:
        fcntl.flock(fh.fileno(), fcntl.LOCK_SH)
        try:
            records = _read_records(fh)
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
    rec = _latest_by_repo(records).get(repo)
    return _to_obs(rec) if rec else None


def record_observation(path: Path, repo: str, *, saturated: bool, reason: str,
                       host_suitable: bool | None, host_note: str,
                       green_pct: float | None, now_epoch: float, now_iso: str,
                       sustained_ticks: int = DEFAULT_SUSTAINED_TICKS,
                       green_hours: float | None = None,
                       green_authoritative_run_hours: float | None = None,
                       green_window_start: str | None = None,
                       green_window_end: str | None = None,
                       ) -> Observation:
    """Append one observation, computing the saturation streak from the prior
    record for ``repo`` under a single exclusive lock (read-latest + append is
    atomic, so a concurrent writer cannot corrupt the streak count)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a+", encoding="utf-8") as fh:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
        try:
            prev = _latest_by_repo(_read_records(fh)).get(repo)
            prev_obs = _to_obs(prev) if prev else None
            if saturated:
                if prev_obs and prev_obs.saturated:
                    consecutive = prev_obs.consecutive_saturated + 1
                    streak_since = prev_obs.streak_since or now_iso
                else:
                    consecutive = 1
                    streak_since = now_iso
            else:
                consecutive = 0
                streak_since = None
            obs = Observation(
                repo=repo, observed_at=now_iso, observed_epoch=now_epoch,
                saturated=saturated, reason=reason,
                consecutive_saturated=consecutive, streak_since=streak_since,
                sustained=bool(saturated and consecutive >= sustained_ticks),
                host_suitable=host_suitable, host_note=host_note,
                green_pct=green_pct, green_hours=green_hours,
                green_authoritative_run_hours=green_authoritative_run_hours,
                green_window_start=green_window_start,
                green_window_end=green_window_end,
            )
            fh.write(json.dumps(asdict(obs), sort_keys=True) + "\n")
            fh.flush()
            os.fsync(fh.fileno())
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
    return obs


# --- throttle predicate (read-only, observable, self-bounding) ----------------
@dataclass
class ThrottleStatus:
    engaged: bool
    code: int
    reason: str    # observable: WHY (or why not)
    detail: str    # observable: WHAT it suppresses, and its BOUND


def _host_phrase(obs: Observation) -> str:
    if obs.host_suitable is True:
        return ("host SUITABLE (executing-CPU under policy) at last obs -> the "
                "bottleneck is the single PMU runner (capacity), not host "
                "contention")
    if obs.host_suitable is False:
        return ("host NOT SUITABLE at last obs -> genuine host contention on top "
                "of the queue")
    return "host verdict unavailable at last obs"


def throttle_status(path: Path, repo: str, *, now_epoch: float,
                    stale_after: float = DEFAULT_STALE_AFTER_SECS,
                    sustained_ticks: int = DEFAULT_SUSTAINED_TICKS
                    ) -> ThrottleStatus:
    unthrottle = (
        "Un-throttles automatically when the next observation shows an idle "
        "runner or a drained queue (the saturation streak resets), or if this "
        f"signal goes stale (>{stale_after:g}s old) -- FAIL-OPEN, so it can "
        "always turn itself off.")
    latest = latest_observation(path, repo)
    if latest is None:
        return ThrottleStatus(
            False, EXIT_CLEAR,
            f"throttle CLEAR: no lane observation recorded yet for {repo}",
            "Suppresses nothing. " + unthrottle)
    age = now_epoch - latest.observed_epoch
    if age > stale_after:
        return ThrottleStatus(
            False, EXIT_CLEAR,
            f"throttle CLEAR (FAIL-OPEN): last lane observation for {repo} is "
            f"{age:.0f}s old (> {stale_after:g}s bound) -- refusing to throttle "
            f"on a stale signal; a throttle that cannot turn itself off is a new "
            f"outage",
            "Suppresses nothing while stale. " + unthrottle)
    if latest.sustained and latest.saturated:
        return ThrottleStatus(
            True, EXIT_THROTTLED,
            f"throttle ENGAGED: {repo} self-hosted capacity pressure sustained "
            f"{latest.consecutive_saturated} consecutive observations (since "
            f"{latest.streak_since}); {latest.reason}",
            "SUPPRESSES: new speculative CI load (CI refires / non-landing "
            "pushes) so we stop piling work onto a saturated lane. DOES NOT "
            "suppress: LANDINGS -- landing drains the queue, so blocking it "
            f"would make saturation permanent. {_host_phrase(latest)}. "
            + unthrottle)
    if latest.saturated:
        return ThrottleStatus(
            False, EXIT_CLEAR,
            f"throttle CLEAR: {repo} capacity pressure observed but only "
            f"{latest.consecutive_saturated} of {sustained_ticks} consecutive "
            f"observations -- not yet sustained",
            "Suppresses nothing yet (recording, watching). " + unthrottle)
    return ThrottleStatus(
        False, EXIT_CLEAR,
        f"throttle CLEAR: {repo} lane not saturated (last obs {age:.0f}s ago)",
        "Suppresses nothing. " + unthrottle)


# --- producer -----------------------------------------------------------------
def _probe_host(sample_seconds: float) -> tuple[bool | None, str]:
    try:
        lp = _load_probe()
        ns = argparse.Namespace(
            sample_seconds=sample_seconds,
            max_executing_percent=lp.DEFAULT_MAX_EXECUTING_PERCENT,
            min_memory_available_percent=lp.DEFAULT_MIN_MEMORY_AVAILABLE_PERCENT,
            top=1, json=True)
        _code, payload = lp.run(ns)
        verdict = payload["verdict"]
        return bool(verdict["suitable"]), "; ".join(verdict["reasons"])
    except Exception as exc:  # probe is advisory; never let it break the tick
        return None, f"load-probe unavailable: {exc}"


def _green_metric(repo: str) -> dict | None:
    try:
        q = _history_query()
        res = q.green_time(q.parent_root(), repo, None, None)
        return res if res.get("green_pct") is not None else None
    except Exception:
        return None


def do_tick(repo: str, gh_cmd: str, limit: int, *,
            per_call_timeout: float = DEFAULT_PER_CALL_TIMEOUT,
            path: Path | None = None,
            sustained_ticks: int = DEFAULT_SUSTAINED_TICKS,
            sample_seconds: float = DEFAULT_PROBE_SAMPLE_SECS,
            now: tuple[float, str] | None = None) -> int:
    path = path or store_path()
    qh = _queue_health()
    _code, fields = qh.compute_gate(repo, gh_cmd, limit,
                                    per_call_timeout=per_call_timeout)
    if str(fields.get("state")) == "unknown":
        # Could not fetch -> cannot determine saturation. Do NOT append (that
        # would corrupt the streak). Surface it: a broken check must be visible,
        # never silently "ok".
        print("state=degraded")
        print(f"summary=lane check could not measure: {fields.get('summary', '')}")
        return 2

    bc = str(fields.get("binding_constraint", "none"))
    saturated = bc not in ("", "none")
    reason = bc if saturated else "lane not saturated"
    host_suitable, host_note = _probe_host(sample_seconds)
    green = _green_metric(repo)
    green_pct = float(green["green_pct"]) if green else None
    now_epoch, now_iso = now or _now()
    obs = record_observation(
        path, repo, saturated=saturated, reason=reason,
        host_suitable=host_suitable, host_note=host_note, green_pct=green_pct,
        now_epoch=now_epoch, now_iso=now_iso, sustained_ticks=sustained_ticks,
        green_hours=green.get("green_hours") if green else None,
        green_authoritative_run_hours=(
            green.get("authoritative_run_hours") if green else None),
        green_window_start=green.get("window_start") if green else None,
        green_window_end=green.get("window_end_utc") if green else None)

    state = "sustained" if obs.sustained else ("saturating" if obs.saturated
                                               else "ok")
    green_txt = (
        f"green-time {obs.green_pct}% = {obs.green_hours} green h / "
        f"{obs.green_authoritative_run_hours} authoritative-run h; window "
        f"{obs.green_window_start}..{obs.green_window_end}"
        if obs.green_pct is not None else "green-time n/a"
    )
    host_txt = ("host=suitable" if obs.host_suitable is True else
                "host=unsuitable" if obs.host_suitable is False else "host=n/a")
    if obs.saturated:
        summary = (f"{repo} capacity pressure {obs.consecutive_saturated} consec "
                   f"(since {obs.streak_since}); {host_txt}; {green_txt}; "
                   f"{obs.reason}")
    else:
        summary = f"{repo} lane clear; {host_txt}; {green_txt}"
    print(f"state={state}")
    print(f"summary={summary}")
    print(f"durable_record={path}")
    # Exit 1 ONLY when sustained -> fires the ci_lane_saturation hard-warn.
    return 1 if obs.sustained else 0


# --- CLI ----------------------------------------------------------------------
DEFAULT_REPO = "rrnewton/hermit"


def _cmd_throttle_check(args) -> int:
    path = store_path(args.store)
    now_epoch, _ = _now()
    st = throttle_status(path, args.repo, now_epoch=now_epoch,
                         stale_after=args.stale_after,
                         sustained_ticks=args.sustained_ticks)
    if args.json:
        print(json.dumps({
            "engaged": st.engaged, "code": st.code, "repo": args.repo,
            "reason": st.reason, "detail": st.detail,
            "store": str(path)}, sort_keys=True))
    else:
        print(st.reason)
        print(f"  {st.detail}")
    return st.code


def _cmd_status(args) -> int:
    path = store_path(args.store)
    repos = ([DEFAULT_REPO, "rrnewton/reverie"] if args.all else [args.repo])
    if not path.exists():
        print(f"(no durable lane-health store yet at {path})")
        return 0
    for repo in repos:
        obs = latest_observation(path, repo)
        if obs is None:
            print(f"{repo}: (no observation)")
            continue
        print(f"{repo}: saturated={obs.saturated} sustained={obs.sustained} "
              f"consec={obs.consecutive_saturated} since={obs.streak_since} "
              f"host_suitable={obs.host_suitable} green_pct={obs.green_pct} "
              f"at={obs.observed_at}")
        print(f"    reason: {obs.reason}")
    return 0


def _cmd_tick(args) -> int:
    gh_cmd = args.gh or os.environ.get("GH", "with-proxy gh")
    return do_tick(args.repo, gh_cmd, args.limit,
                   per_call_timeout=args.per_call_timeout,
                   path=store_path(args.store),
                   sustained_ticks=args.sustained_ticks,
                   sample_seconds=args.sample_seconds)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    t = sub.add_parser("tick", description="observe + record saturation (producer)", help="observe + record saturation (producer)")
    t.add_argument("--repo", default=DEFAULT_REPO)
    t.add_argument("--gh", default=None, help="gh command (default $GH or "
                                              "'with-proxy gh')")
    t.add_argument("--limit", type=int, default=100)
    t.add_argument("--per-call-timeout", type=float,
                   default=DEFAULT_PER_CALL_TIMEOUT)
    t.add_argument("--sustained-ticks", type=int, default=DEFAULT_SUSTAINED_TICKS)
    t.add_argument("--sample-seconds", type=float,
                   default=DEFAULT_PROBE_SAMPLE_SECS)
    t.add_argument("--store", default=None)
    t.set_defaults(func=_cmd_tick)

    c = sub.add_parser("throttle-check",
                       description="read-only throttle predicate (exit 3 if engaged)",
                       help="read-only throttle predicate (exit 3 if engaged)")
    c.add_argument("--repo", default=DEFAULT_REPO)
    c.add_argument("--stale-after", type=float, default=DEFAULT_STALE_AFTER_SECS)
    c.add_argument("--sustained-ticks", type=int, default=DEFAULT_SUSTAINED_TICKS)
    c.add_argument("--store", default=None)
    c.add_argument("--json", action="store_true")
    c.set_defaults(func=_cmd_throttle_check)

    s = sub.add_parser("status", description="human dump of the latest observation", help="human dump of the latest observation")
    s.add_argument("--repo", default=DEFAULT_REPO)
    s.add_argument("--all", action="store_true")
    s.add_argument("--store", default=None)
    s.set_defaults(func=_cmd_status)

    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
