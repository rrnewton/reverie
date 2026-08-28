#!/usr/bin/env python3
"""Durable store + flaky-is-red verdict for nightly concurrent-stress runs.

This is the *recording + alarm* layer of the nightly stress harness. It sits on
top of the shared concurrent-burst probe primitive (owned jointly with
hermit-multisect; see ci-hub/stress/README.md). It intentionally does NOT
implement its own burst — it only consumes the burst's CSV contract, so nightly
and multisect can never drift on how flakiness is *measured*; they only differ
in how the same k/N is *read* (nightly = flaky-is-red; multisect = rate bands).

Burst CSV contract (one row per (workload, wave), produced by the shared
primitive / multisect's probe.sh):

    sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS

STATUS is ``OK`` when the burst actually ran; otherwise a harness/build failure
token (``WT_FAIL``/``BUILD_FAIL``/``NOBIN``/``NOTEST``) with empty counts.

Verdict (owner rule — determinism of OUTCOME, not "did it pass"):

    CLEAN    every OK burst had passes==N (0 hangs AND 0 others).      -> GREEN
    FLAKY    some OK burst had 0<passes<N (nondeterministic outcome).  -> RED / P0
    FAILING  some OK burst had passes==0 (deterministic failure).      -> RED / P0
    ERROR    at least one burst could not run (non-OK STATUS) and no
             FLAKY/FAILING signal was observed.                        -> RED / P0

Anything other than CLEAN raises the alarm. 29/30 ALARMS; it is never rounded to
green. A run that could not execute its probe is RED too (a silent-green nightly
is exactly the failure mode this task exists to kill).

Store: ``ignored/ci-hub/stress-runs.jsonl`` (gitignored, append-only, one event
per run). Joins the rest of the ci-hub store by ``(repo, git_sha)`` and the same
``OWNER/REPO`` string.
"""

from __future__ import annotations

import argparse
import csv
import fcntl
import io
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from stress_rows import BurstRow
from stress_rows import CSV_COLUMNS
from stress_rows import StoreError
from stress_rows import StressRunRecord
from stress_rows import StressSummary
from stress_rows import StressVerdict

ROOT = Path(__file__).resolve().parents[2]

# Verdicts, ordered by severity (CLEAN is the only non-alarm).
CLEAN = StressVerdict.CLEAN
FLAKY = StressVerdict.FLAKY
FAILING = StressVerdict.FAILING
ERROR = StressVerdict.ERROR


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def default_store_path() -> Path:
    override = os.environ.get("CI_HUB_STRESS_STORE")
    return Path(override).expanduser() if override else ROOT / "ignored/ci-hub/stress-runs.jsonl"


def parse_burst_csv(text: str) -> list[BurstRow]:
    """Parse the shared burst CSV without padding or truncating producer rows."""
    rows: list[BurstRow] = []
    reader = csv.reader(io.StringIO(text))
    for line_number, raw in enumerate(reader, 1):
        if not raw or all(c.strip() == "" for c in raw):
            continue
        # Skip a header line if present.
        if tuple(cell.strip().lower() for cell in raw) == tuple(
            name.lower() for name in CSV_COLUMNS
        ):
            continue
        rows.append(BurstRow.from_csv_cells(raw, line_number))
    return rows


def classify(rows: list[BurstRow]) -> StressSummary:
    """Fold per-burst rows into one run verdict under flaky-is-red."""
    return StressSummary.from_rows(rows)


def build_record(
    *,
    csv_text: str,
    repo: str,
    git_sha: str,
    workload: str,
    width: int | None = None,
    timeout_s: int | None = None,
    backend: str = "ptrace",
    host: str | None = None,
    agent: str | None = None,
    started_at: str | None = None,
    finished_at: str | None = None,
    source_tool: str | None = None,
    trigger: str = "nightly",
    extra: dict[str, Any] | None = None,
) -> StressRunRecord:
    rows = parse_burst_csv(csv_text)
    now = finished_at or utc_now()
    return StressRunRecord.build(
        rows=rows,
        repo=repo,
        git_sha=git_sha,
        workload=workload,
        width=width,
        timeout_s=timeout_s,
        backend=backend,
        host=host or os.uname().nodename,
        agent=agent or os.environ.get("CI_HUB_AGENT", "hermit-250"),
        started_at=started_at or now,
        finished_at=now,
        source_tool=source_tool,
        trigger=trigger,
        extra=extra or None,
    )


def append_record(record: StressRunRecord, store: Path | None = None) -> Path:
    path = store or default_store_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps(record.to_dict(), separators=(",", ":"), sort_keys=True)
    with open(path, "a", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            handle.write(line + "\n")
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    return path


def load_records(store: Path | None = None) -> list[StressRunRecord]:
    path = store or default_store_path()
    if not path.exists():
        return []
    out: list[StressRunRecord] = []
    with open(path, encoding="utf-8") as handle:
        for n, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise StoreError(f"corrupt stress-run store at line {n}: {error}") from error
            out.append(StressRunRecord.from_dict(value, f"stress-run store line {n}"))
    return out


# --------------------------------------------------------------------------- CLI


def _cmd_record(args: argparse.Namespace) -> int:
    csv_text = sys.stdin.read() if args.csv in ("-", None) else Path(args.csv).read_text()
    record = build_record(
        csv_text=csv_text,
        repo=args.repo,
        git_sha=args.sha,
        workload=args.workload,
        width=args.width,
        timeout_s=args.timeout,
        backend=args.backend,
        source_tool=args.source_tool,
        trigger=args.trigger,
    )
    path = append_record(record, Path(args.store) if args.store else None)
    # Machine-readable summary on stdout (the driver captures this for alarm text);
    # human line on stderr.
    summary = record.summary
    tokens = ",".join(summary.error_tokens) if summary.error_tokens else "-"
    print(
        f"VERDICT={summary.verdict.value} passes={summary.passes} hangs={summary.hangs} "
        f"others={summary.others} instances={summary.total_instances} "
        f"bursts_ok={summary.bursts_ok} errors={summary.bursts_error} tokens={tokens}"
    )
    print(
        f"{summary.verdict.value}  {record.workload}  "
        f"passes={summary.passes} hangs={summary.hangs} others={summary.others} "
        f"(instances={summary.total_instances}, bursts_ok={summary.bursts_ok}, "
        f"errors={summary.bursts_error}) -> {path}",
        file=sys.stderr,
    )
    # Exit code IS the alarm: 0 = CLEAN/green, 2 = RED (flaky/failing/error -> P0).
    return 2 if summary.alarm else 0


def _cmd_summary(args: argparse.Namespace) -> int:
    records = load_records(Path(args.store) if args.store else None)
    if args.since:
        records = [record for record in records if record.finished_at >= args.since]
    if not records:
        print("(no stress runs recorded)")
        return 0
    for record in records[-args.limit:]:
        flag = "🔴" if record.summary.alarm else "🟢"
        print(
            f"{flag} {record.finished_at:20} {record.summary.verdict.value:8} "
            f"{record.git_short:13} {record.workload:40} "
            f"p={record.summary.passes} h={record.summary.hangs} "
            f"o={record.summary.others} N={record.summary.total_instances} "
            f"[{record.trigger}]"
        )
    reds = [record for record in records if record.summary.alarm]
    latest = records[-1]
    print(
        f"\n{len(records)} runs, {len(reds)} RED. "
        f"Latest: {latest.summary.verdict.value} @ {latest.git_short}"
    )
    return 2 if latest.summary.alarm else 0


def _cmd_selftest(_args: argparse.Namespace) -> int:
    sha = "0" * 40
    cases = {
        "all-pass -> CLEAN": (f"{sha},short,10,64,0,64,0,0.000,OK", CLEAN, False),
        "one-hang -> FLAKY": (f"{sha},short,10,64,1,63,0,0.016,OK", FLAKY, True),
        "many-hang partial -> FLAKY": (f"{sha},short,10,64,18,46,0,0.281,OK", FLAKY, True),
        "all-hang -> FAILING": (f"{sha},short,10,64,64,0,0,1.000,OK", FAILING, True),
        "other-exit -> FLAKY": (f"{sha},short,10,64,0,63,1,0.000,OK", FLAKY, True),
        "build-fail -> ERROR": (f"{sha},,,,,,,,BUILD_FAIL", ERROR, True),
        "clean+one-error -> ERROR": (
            f"{sha},short,10,64,0,64,0,0.000,OK\n{sha},,,,,,,,NOBIN", ERROR, True),
        "flake dominates error": (
            f"{sha},short,10,64,2,62,0,0.031,OK\n{sha},,,,,,,,NOBIN", FLAKY, True),
    }
    ok = True
    for name, (text, want_verdict, want_alarm) in cases.items():
        got = classify(parse_burst_csv(text))
        good = got.verdict == want_verdict and got.alarm == want_alarm
        ok = ok and good
        print(f"[{'PASS' if good else 'FAIL'}] {name}: "
              f"got {got.verdict.value}/alarm={got.alarm} "
              f"want {want_verdict.value}/alarm={want_alarm}")
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("record", description="record a burst CSV as one stress run (exit 2 if RED)", help="record a burst CSV as one stress run (exit 2 if RED)")
    r.add_argument("--csv", default="-", help="burst CSV path, or - for stdin")
    r.add_argument("--repo", default="rrnewton/hermit")
    r.add_argument("--sha", required=True, help="main HEAD SHA the burst ran at")
    r.add_argument("--workload", required=True, help="workload/test-set slug")
    r.add_argument("--width", type=int)
    r.add_argument("--timeout", type=int)
    r.add_argument("--backend", default="ptrace")
    r.add_argument("--source-tool", default=None)
    r.add_argument("--trigger", default="nightly")
    r.add_argument("--store", default=None)
    r.set_defaults(func=_cmd_record)

    s = sub.add_parser("summary", description="print recorded stress runs (exit 2 if latest RED)", help="print recorded stress runs (exit 2 if latest RED)")
    s.add_argument("--store", default=None)
    s.add_argument("--since", default=None, help="ISO timestamp lower bound")
    s.add_argument("--limit", type=int, default=30)
    s.set_defaults(func=_cmd_summary)

    t = sub.add_parser("selftest", description="verify flaky-is-red verdict logic", help="verify flaky-is-red verdict logic")
    t.set_defaults(func=_cmd_selftest)

    args = p.parse_args(argv)
    try:
        return args.func(args)
    except StoreError as error:
        print(f"stress_store: {error}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
