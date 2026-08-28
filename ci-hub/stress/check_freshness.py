#!/usr/bin/env python3
"""Is the nightly stress lane still PRODUCING? — the missing-run alarm.

THE GAP THIS CLOSES
-------------------
The stress harness already alarms loudly on a bad result (`stress_store.py`:
CLEAN is the only non-alarm; 29/30 raises P0). What nothing watched was the case
where **no result arrives at all**. That is not hypothetical:

* The only live schedule was a USER CRONTAB (`30 4 * * *`). On 2026-08-06
  `crontab -l` reports no crontab for the coordinator user — the schedule is GONE. It lived
  in unversioned, machine-local state, so it could vanish with no commit, no
  review, and no alarm. It did, and nothing noticed for two nights.
* The GitHub `super` lane (`validation-levels.yml`, weekly) fired exactly twice
  and was CANCELLED at `timeout-minutes: 360` both times — 0 verdicts ever, and
  again no alarm, because a cancelled run produces no red to notice.

Both are the same shape: **the absence of a result is itself the signal**, and an
alarm that only fires on results can never raise it.

THE ONE NON-OBVIOUS RULE — FRESHNESS KEYS ON MEASUREMENTS, NOT ON RUNS
----------------------------------------------------------------------
The obvious implementation ("alarm if the newest store row is older than N
hours") has a hole that the 2026-08-04 run walks straight through. That run
fired, recorded a row, and measured NOTHING: `CALIB_UNDERPOWERED`,
`instances=0, bursts_ok=0, errors=1`. Under a run-keyed check it counts as
freshness. So a harness whose calibrator is permanently under-powered — the very
fault that makes it stop measuring — would keep the staleness alarm silent
forever. **An alarm that the fault it watches for can switch off is not an
alarm.**

So freshness is keyed on the newest row that actually MEASURED something
(`bursts_ok >= 1` and `total_instances >= 1`), and rows that ran-but-measured-
nothing are reported separately as `no_result_runs`. A lane that is firing
nightly and producing nothing is STALE with a distinct reason, not fresh.

THE THRESHOLD IS A POLICY CHOICE, AND IS LABELLED AS ONE
--------------------------------------------------------
`--cadence-hours` is the schedule's own period (24 for a nightly). The staleness
bound is `cadence + grace`, and `--grace-hours` defaults to the cadence — i.e.
one fully missed cycle is tolerated, two is an alarm. That is a judgement, not a
derivation, so the output states the bound it used rather than presenting it as
if the data implied it.

VERDICTS
  FRESH        a measuring run inside the bound.
  STALE        the newest measuring run is older than the bound (reason names
               whether runs are absent entirely or merely not measuring).
  NEVER        the store has no measuring run at all.
  NO_STORE     the store file does not exist — the lane has never recorded.

EXIT CODES
  0 FRESH   ·   2 STALE / NEVER / NO_STORE (alarm)   ·   3 error

USAGE
  check_freshness.py [--store PATH] [--cadence-hours 24] [--grace-hours N]
                     [--now ISO8601] [--json]
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

from stress_rows import StoreError
from stress_rows import StressRunRecord

HERE = Path(os.path.abspath(__file__)).parent
PARENT = HERE.parent.parent
DEFAULT_STORE = PARENT / "ignored" / "ci-hub" / "stress-runs.jsonl"

FRESH, STALE, NEVER, NO_STORE = "FRESH", "STALE", "NEVER", "NO_STORE"
EXIT_OK, EXIT_ALARM, EXIT_ERROR = 0, 2, 3


def parse_ts(value) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    text = value.strip().replace("Z", "+00:00")
    try:
        stamp = datetime.fromisoformat(text)
    except ValueError:
        return None
    return stamp if stamp.tzinfo else stamp.replace(tzinfo=timezone.utc)


def row_time(row: StressRunRecord) -> datetime | None:
    """A row's event time. `finished_at` is the completion instant; `started_at`
    is the fallback for a row that recorded a start but no finish."""
    return parse_ts(row.finished_at) or parse_ts(row.started_at)


def measured(row: StressRunRecord) -> bool:
    """Did this run actually MEASURE anything?

    A row with zero successful bursts or zero instances executed nothing — it is
    a no-result wearing a run's clothing (the 2026-08-04 CALIB_UNDERPOWERED
    case). Counting it as freshness is what would let the harness go quiet
    without the alarm firing.
    """
    return row.summary.bursts_ok >= 1 and row.summary.total_instances >= 1


def load_rows(store: Path) -> list[StressRunRecord]:
    rows: list[StressRunRecord] = []
    with open(store) as handle:
        for line_number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
                rows.append(
                    StressRunRecord.from_dict(
                        value, f"stress-run store line {line_number}"
                    )
                )
            except json.JSONDecodeError as error:
                raise StoreError(
                    f"stress-run store line {line_number}: invalid JSON: {error}"
                ) from error
    return rows


def assess(rows: list[StressRunRecord], now: datetime, cadence_hours: float,
           grace_hours: float, malformed: int = 0) -> dict:
    bound = timedelta(hours=cadence_hours + grace_hours)
    measuring: list[tuple[datetime, StressRunRecord]] = []
    for row in rows:
        timestamp = row_time(row)
        if measured(row) and timestamp is not None:
            measuring.append((timestamp, row))
    measuring.sort(key=lambda item: item[0])

    any_time: list[datetime] = []
    for row in rows:
        timestamp = row_time(row)
        if timestamp is not None:
            any_time.append(timestamp)
    newest_any = max(any_time) if any_time else None
    no_result_runs = len(rows) - len(measuring)

    report = {
        "now": now.isoformat(),
        "bound_hours": cadence_hours + grace_hours,
        "bound_basis": (
            f"cadence {cadence_hours}h + grace {grace_hours}h — a POLICY CHOICE "
            "(one missed cycle tolerated, two alarms), not a derivation"
        ),
        "rows": len(rows),
        "malformed_lines": malformed,
        "measuring_runs": len(measuring),
        "no_result_runs": no_result_runs,
        "newest_run_utc": newest_any.isoformat() if newest_any else None,
        "newest_measuring_run_utc": measuring[-1][0].isoformat() if measuring else None,
    }

    if not measuring:
        report["verdict"] = NEVER
        report["age_hours"] = None
        if rows:
            report["reason"] = (
                f"{len(rows)} run(s) recorded but NONE measured anything "
                f"(bursts_ok>=1 and instances>=1). The lane is firing and "
                f"producing no measurement — a no-result, not a green."
            )
        else:
            report["reason"] = "the store is empty: the lane has never recorded a run"
        report["alarm"] = True
        return report

    newest_time = measuring[-1][0]
    age = now - newest_time
    report["age_hours"] = round(age.total_seconds() / 3600.0, 2)
    if age <= bound:
        report["verdict"] = FRESH
        report["alarm"] = False
        report["reason"] = (
            f"newest measuring run is {report['age_hours']}h old, within the "
            f"{report['bound_hours']}h bound"
        )
        return report

    report["verdict"] = STALE
    report["alarm"] = True
    if newest_any is not None and newest_any > newest_time:
        stale_since = round((now - newest_any).total_seconds() / 3600.0, 2)
        report["reason"] = (
            f"the lane is still FIRING ({stale_since}h since the newest run) but "
            f"has not MEASURED anything for {report['age_hours']}h — "
            f"{no_result_runs} run(s) produced no result. A firing-but-not-"
            f"measuring lane is stale, not fresh."
        )
    else:
        report["reason"] = (
            f"no measuring run for {report['age_hours']}h, exceeding the "
            f"{report['bound_hours']}h bound — the schedule is not firing "
            f"(check the workflow, the runner, and `crontab -l`)"
        )
    return report


def render(report: dict) -> str:
    mark = "🔴" if report.get("alarm") else "🟢"
    lines = [f"{mark} nightly-stress freshness: {report['verdict']}",
             f"   {report['reason']}"]
    lines.append(
        f"   rows={report['rows']} measuring={report['measuring_runs']} "
        f"no-result={report['no_result_runs']} malformed={report['malformed_lines']}"
    )
    if report.get("newest_measuring_run_utc"):
        lines.append(f"   newest measuring run: {report['newest_measuring_run_utc']} "
                     f"({report['age_hours']}h ago)")
    if report.get("newest_run_utc"):
        lines.append(f"   newest run of any kind: {report['newest_run_utc']}")
    lines.append(f"   bound: {report['bound_basis']}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Alarm when the nightly stress lane stops producing.")
    parser.add_argument("--store", default=str(DEFAULT_STORE))
    parser.add_argument("--cadence-hours", type=float, default=24.0,
                        help="the schedule's period (24 for a nightly)")
    parser.add_argument("--grace-hours", type=float, default=None,
                        help="tolerance beyond one cadence (default: one cadence)")
    parser.add_argument("--now", default=None, help="ISO8601 override, for tests")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    grace = args.cadence_hours if args.grace_hours is None else args.grace_hours
    now = parse_ts(args.now) if args.now else datetime.now(timezone.utc)
    if now is None:
        print(f"check_freshness: cannot parse --now {args.now!r}", file=sys.stderr)
        return EXIT_ERROR

    store = Path(args.store)
    if not store.exists():
        report = {
            "verdict": NO_STORE, "alarm": True, "rows": 0, "malformed_lines": 0,
            "measuring_runs": 0, "no_result_runs": 0, "age_hours": None,
            "newest_run_utc": None, "newest_measuring_run_utc": None,
            "now": now.isoformat(),
            "bound_hours": args.cadence_hours + grace,
            "bound_basis": "n/a — no store",
            "reason": f"stress store not found at {store}: the lane has never recorded",
        }
    else:
        try:
            rows = load_rows(store)
        except (OSError, StoreError) as exc:
            print(f"check_freshness: cannot read store: {exc}", file=sys.stderr)
            return EXIT_ERROR
        report = assess(rows, now, args.cadence_hours, grace)

    print(json.dumps(report, indent=2) if args.json else render(report))
    return EXIT_ALARM if report["alarm"] else EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
