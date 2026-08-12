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
``OWNER/REPO`` string, matching obligations.py's file-contract convention.
"""

from __future__ import annotations

import argparse
import csv
import fcntl
import io
import json
import os
import re
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

ROOT = Path(__file__).resolve().parents[2]
SCHEMA_VERSION = 1
SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")

CSV_COLUMNS = (
    "sha", "short", "build_s", "burst_N",
    "hangs", "passes", "other", "hang_rate", "status",
)

# Verdicts, ordered by severity (CLEAN is the only non-alarm).
CLEAN, FLAKY, FAILING, ERROR = "CLEAN", "FLAKY", "FAILING", "ERROR"
RED_VERDICTS = frozenset((FLAKY, FAILING, ERROR))


class StoreError(RuntimeError):
    """The stress-run store is missing, corrupt, or inconsistent."""


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def default_store_path() -> Path:
    override = os.environ.get("CI_HUB_STRESS_STORE")
    return Path(override).expanduser() if override else ROOT / "ignored/ci-hub/stress-runs.jsonl"


def _to_int(value: str) -> int | None:
    value = (value or "").strip()
    if value == "":
        return None
    try:
        return int(value)
    except ValueError:
        return None


def parse_burst_csv(text: str) -> list[dict[str, Any]]:
    """Parse the shared burst CSV into per-burst dicts.

    Tolerates an optional header row and blank lines. Each returned dict has the
    contract columns plus an ``ok`` flag (status == OK and counts are present).
    """
    rows: list[dict[str, Any]] = []
    reader = csv.reader(io.StringIO(text))
    for raw in reader:
        if not raw or all(c.strip() == "" for c in raw):
            continue
        # Skip a header line if present.
        if raw[0].strip().lower() == "sha":
            continue
        # Pad/truncate to the contract width.
        cells = (list(raw) + [""] * len(CSV_COLUMNS))[: len(CSV_COLUMNS)]
        row = dict(zip(CSV_COLUMNS, (c.strip() for c in cells)))
        status = row["status"] or "OK"
        row["status"] = status
        for key in ("hangs", "passes", "other", "burst_N"):
            row[key] = _to_int(row[key])
        row["ok"] = (
            status == "OK"
            and row["hangs"] is not None
            and row["passes"] is not None
            and row["other"] is not None
        )
        rows.append(row)
    return rows


def classify(rows: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """Fold per-burst rows into one run verdict under flaky-is-red."""
    rows = list(rows)
    ok_rows = [r for r in rows if r.get("ok")]
    bad_rows = [r for r in rows if not r.get("ok")]

    hangs = sum(r["hangs"] for r in ok_rows)
    passes = sum(r["passes"] for r in ok_rows)
    others = sum(r["other"] for r in ok_rows)
    total = hangs + passes + others

    # Per-burst determinism: a burst is CLEAN only if every instance passed.
    flaky_bursts = [r for r in ok_rows if r["passes"] not in (None, r.get("burst_N")) and 0 < r["passes"]
                    and (r["hangs"] or r["other"])]
    failing_bursts = [r for r in ok_rows if r["passes"] == 0 and (r["hangs"] or r["other"])]

    if failing_bursts:
        verdict = FAILING
    elif flaky_bursts or (ok_rows and (hangs or others)):
        verdict = FLAKY
    elif not ok_rows:
        verdict = ERROR  # nothing ran
    else:
        verdict = CLEAN

    # A harness/build error alongside otherwise-clean data is still RED: the
    # nightly could not fully execute its probe, so it cannot claim green.
    if verdict == CLEAN and bad_rows:
        verdict = ERROR

    return {
        "verdict": verdict,
        "alarm": verdict in RED_VERDICTS,
        "bursts": len(rows),
        "bursts_ok": len(ok_rows),
        "bursts_error": len(bad_rows),
        "error_tokens": sorted({r["status"] for r in bad_rows}),
        "total_instances": total,
        "hangs": hangs,
        "passes": passes,
        "others": others,
        "hang_rate": round(hangs / total, 4) if total else None,
    }


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
) -> dict[str, Any]:
    if not REPO_RE.fullmatch(repo):
        raise StoreError(f"invalid repo {repo!r}; expected OWNER/REPO")
    if not SHA_RE.fullmatch(git_sha):
        raise StoreError(f"invalid git_sha {git_sha!r}")
    rows = parse_burst_csv(csv_text)
    summary = classify(rows)
    now = finished_at or utc_now()
    record = {
        "schema_version": SCHEMA_VERSION,
        "run_id": uuid.uuid4().hex,
        "trigger": trigger,
        "repo": repo,
        "git_sha": git_sha,
        "git_short": git_sha[:12],
        "workload": workload,
        "backend": backend,
        "width": width,
        "timeout_s": timeout_s,
        "host": host or os.uname().nodename,
        "agent": agent or os.environ.get("CI_HUB_AGENT", "hermit-250"),
        "started_at": started_at or now,
        "finished_at": now,
        "source_tool": source_tool,
        **summary,
        "rows": rows,
    }
    if extra:
        record["extra"] = extra
    return record


def append_record(record: dict[str, Any], store: Path | None = None) -> Path:
    path = store or default_store_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps(record, separators=(",", ":"), sort_keys=True)
    with open(path, "a", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            handle.write(line + "\n")
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    return path


def load_records(store: Path | None = None) -> list[dict[str, Any]]:
    path = store or default_store_path()
    if not path.exists():
        return []
    out: list[dict[str, Any]] = []
    with open(path, encoding="utf-8") as handle:
        for n, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError as error:
                raise StoreError(f"corrupt stress-run store at line {n}: {error}") from error
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
    tokens = ",".join(record["error_tokens"]) if record["error_tokens"] else "-"
    print(
        f"VERDICT={record['verdict']} passes={record['passes']} hangs={record['hangs']} "
        f"others={record['others']} instances={record['total_instances']} "
        f"bursts_ok={record['bursts_ok']} errors={record['bursts_error']} tokens={tokens}"
    )
    print(
        f"{record['verdict']}  {record['workload']}  "
        f"passes={record['passes']} hangs={record['hangs']} others={record['others']} "
        f"(instances={record['total_instances']}, bursts_ok={record['bursts_ok']}, "
        f"errors={record['bursts_error']}) -> {path}",
        file=sys.stderr,
    )
    # Exit code IS the alarm: 0 = CLEAN/green, 2 = RED (flaky/failing/error -> P0).
    return 2 if record["alarm"] else 0


def _cmd_summary(args: argparse.Namespace) -> int:
    records = load_records(Path(args.store) if args.store else None)
    if args.since:
        records = [r for r in records if r.get("finished_at", "") >= args.since]
    if not records:
        print("(no stress runs recorded)")
        return 0
    for r in records[-args.limit:]:
        flag = "🔴" if r.get("alarm") else "🟢"
        print(
            f"{flag} {r.get('finished_at','?'):20} {r.get('verdict','?'):8} "
            f"{r.get('git_short','?'):13} {r.get('workload','?'):40} "
            f"p={r.get('passes')} h={r.get('hangs')} o={r.get('others')} "
            f"N={r.get('total_instances')} [{r.get('trigger','?')}]"
        )
    reds = [r for r in records if r.get("alarm")]
    print(f"\n{len(records)} runs, {len(reds)} RED. "
          f"Latest: {records[-1].get('verdict')} @ {records[-1].get('git_short')}")
    return 2 if records[-1].get("alarm") else 0


def _cmd_selftest(_args: argparse.Namespace) -> int:
    cases = {
        "all-pass -> CLEAN": ("s,short,10,64,0,64,0,0.000,OK", CLEAN, False),
        "one-hang -> FLAKY": ("s,short,10,64,1,63,0,0.016,OK", FLAKY, True),
        "many-hang partial -> FLAKY": ("s,short,10,64,18,46,0,0.281,OK", FLAKY, True),
        "all-hang -> FAILING": ("s,short,10,64,64,0,0,1.000,OK", FAILING, True),
        "other-exit -> FLAKY": ("s,short,10,64,0,63,1,0.000,OK", FLAKY, True),
        "build-fail -> ERROR": ("s,,,,,,,,BUILD_FAIL", ERROR, True),
        "clean+one-error -> ERROR": (
            "s,short,10,64,0,64,0,0.000,OK\ns,,,,,,,,NOBIN", ERROR, True),
        "flake dominates error": (
            "s,short,10,64,2,62,0,0.031,OK\ns,,,,,,,,NOBIN", FLAKY, True),
    }
    ok = True
    for name, (text, want_verdict, want_alarm) in cases.items():
        got = classify(parse_burst_csv(text))
        good = got["verdict"] == want_verdict and got["alarm"] == want_alarm
        ok = ok and good
        print(f"[{'PASS' if good else 'FAIL'}] {name}: "
              f"got {got['verdict']}/alarm={got['alarm']} "
              f"want {want_verdict}/alarm={want_alarm}")
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
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
