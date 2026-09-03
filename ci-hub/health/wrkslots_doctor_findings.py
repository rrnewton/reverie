#!/usr/bin/env python3
"""Carry `wrkslots doctor`'s registry/storage disagreements into the tick.

WHY THIS EXISTS. `wrkslots doctor` is read-only, exits 0 whatever it finds, and
prints its findings only when a human runs it. Measured 2026-09-03 on devbig014:
122 findings sat there -- 47 `row-without-directory`, 73 `dead-owner`, and two
unregistered-directory findings -- and nothing had surfaced any of them. A check
that reports 122 findings on demand and nothing otherwise is indistinguishable
from a healthy system, which is the shape this project has already paid for: a
monitor whose silence meant both "nothing is wrong" and "nobody looked".

⚠️ DELTA, NOT STANDING STATE. This is the same rule `worktree_disk_residue`
states for the same reason: it pages only when a finding is NEW, and carries the
standing counts in its fields so a permanently-on gate never gets muted. With 122
findings already present, a gate on standing state would fire every hour forever
and be filtered out within a week, and then the signal is worse than absent
because everyone has learned to skip it. The first run therefore ADOPTS the
current set as a baseline WITHOUT paging: that backlog did not happen at this
tick and must not be reported as if it had.

⚠️ DETECT ONLY. Nothing here removes, retires, archives or reclaims anything.
Reclamation is a separate authorised decision, and it must stay that way: a slot
holding an unread `HANDOFF.md` is protected by the FILE, not by its registry row,
so anything acting at the registry level would not see that protection. This
gate reports counts and names; it authorizes nothing.

WHY IT PASSES THE ADMISSION TEST in tick-hub.yaml. The question that config asks
is whether ORC can observe the fact directly. It cannot: a disagreement between
the wrkslots registry and what is on local disk is local filesystem and local
registry state, the same class as `worktree_disk_residue` and `wrkslots_audit`,
which are already admitted. No agent name, no fleet census, no TaskGraph owner is
consulted.

WHY NOT `wrkslots audit --gate`, WHICH ALREADY EXISTS. Two reasons, both
measured 2026-09-03. Its gate covers a DELETABLE slot, an interrupted operation
or an unregistered directory -- it does not cover `row-without-directory`, which
is 47 of the 122. And it did not finish inside 400s on a 93-row registry while
its reminder allows 120s. `doctor` answered the same registry in 31s.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any

#: Where the delta baseline lives when the caller does not choose.
DEFAULT_STATE = ".tick-hub/wrkslots-doctor-state.json"

#: Exit statuses. 1 and 2 follow `wrkslots audit --gate`'s own convention so a
#: reader who knows one knows the other.
EXIT_OK = 0
EXIT_NEW_FINDINGS = 1
EXIT_UNAVAILABLE = 2

OK = "ok"
ACTIONABLE = "actionable"
UNKNOWN = "unknown"


def finding_key(finding: dict[str, Any]) -> str:
    """A stable identity for one finding, so a delta means what it says.

    The same disagreement must produce the same key on every tick or every tick
    reports it as new. `slot` is absent on some kinds, so fall back to the
    checkout path and finally to the detail text, which doctor derives from the
    path rather than from anything time-varying.
    """
    kind = str(finding.get("kind") or "unknown-kind")
    for field in ("slot", "checkout", "detail"):
        value = finding.get(field)
        if isinstance(value, str) and value:
            return f"{kind}\t{value}"
    return kind


def run_doctor(wrkslots: str, root: str, timeout: int) -> tuple[list[dict], str | None]:
    """Return doctor's findings, or an explanation of why they are unavailable.

    An unreadable answer is never treated as a clean one. Reporting "no findings"
    because the command failed is the exact confusion this gate exists to end.
    """
    command = [wrkslots, "--project-root", root, "doctor", "--format", "json"]
    try:
        completed = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout, check=False
        )
    except FileNotFoundError:
        return [], f"wrkslots not executable at {wrkslots}"
    except subprocess.TimeoutExpired:
        return [], f"wrkslots doctor exceeded {timeout}s"
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip().splitlines()
        first = detail[0] if detail else f"exit {completed.returncode}"
        return [], f"wrkslots doctor failed: {first}"
    try:
        document = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        return [], f"wrkslots doctor emitted unparseable JSON: {error}"
    findings = document.get("findings")
    if not isinstance(findings, list):
        return [], "wrkslots doctor emitted no findings list"
    return [item for item in findings if isinstance(item, dict)], None


def read_state(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def write_state(path: Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Report NEW wrkslots registry/storage disagreements; detect only, "
            "authorizes nothing"
        )
    )
    parser.add_argument("--root", default=".", help="project root holding .wrkslots.yml")
    parser.add_argument(
        "--wrkslots",
        default="ci-hub/bin/wrkslots",
        help="wrkslots entry point (a test may point this at a stub)",
    )
    parser.add_argument("--state", default=None, help=f"state file (default {DEFAULT_STATE})")
    parser.add_argument(
        "--timeout", type=int, default=120, help="seconds allowed for wrkslots doctor"
    )
    parser.add_argument(
        "--gate",
        action="store_true",
        help=(
            "exit 1 when a finding is NEW since the recorded baseline; exit 2 when "
            "doctor's answer is unavailable; exit 0 for a clean or unchanged population"
        ),
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="do not write the baseline back"
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args(argv)

    state_path = Path(args.state) if args.state else Path(args.root) / DEFAULT_STATE
    findings, unavailable = run_doctor(args.wrkslots, args.root, args.timeout)

    prior = read_state(state_path)
    reported = prior.get("reported") if isinstance(prior, dict) else None
    baseline_missing = not isinstance(reported, list)

    if unavailable is not None:
        # ⚠️ AN UNAVAILABLE ANSWER PAGES RATHER THAN PASSES. A gate that cannot
        # measure and exits 0 is indistinguishable from a clean population, which
        # is the confusion this whole check exists to remove. The baseline is left
        # untouched so a later working tick still sees every finding as new.
        state, summary = UNKNOWN, unavailable
        fields = {"findings_total": "unknown", "new_findings": "unknown"}
        emit(args, state, summary, fields, [], findings)
        return EXIT_UNAVAILABLE if args.gate else EXIT_OK

    current = {finding_key(item) for item in findings}
    by_kind = Counter(str(item.get("kind") or "unknown-kind") for item in findings)

    if baseline_missing:
        # First run adopts the standing backlog silently: it did not happen at
        # this tick, and paging for it here would make the very first tick the
        # loudest one and teach the reader to filter this line immediately.
        fresh: set[str] = set()
        state = UNKNOWN
        summary = (
            f"recorded a baseline of {len(current)} standing finding(s) at "
            f"{state_path}; no delta verdict is available this tick"
        )
    else:
        # Only findings already in the baseline are suppressed, and the baseline
        # is replaced by the CURRENT set rather than accumulated. A disagreement
        # that is repaired and later returns must page again.
        fresh = current - set(reported)
        state = ACTIONABLE if fresh else OK
        if fresh:
            summary = (
                f"{len(fresh)} new registry/storage disagreement(s); "
                f"{len(current)} standing"
            )
        else:
            summary = (
                f"no new registry/storage disagreement; {len(current)} standing"
                if current
                else "no registry/storage disagreement"
            )

    if not args.dry_run:
        write_state(state_path, {"reported": sorted(current)})

    fields = {
        "findings_total": str(len(current)),
        "new_findings": str(len(fresh)),
    }
    for kind, count in sorted(by_kind.items()):
        fields[f"standing_{kind.replace('-', '_')}"] = str(count)

    emit(args, state, summary, fields, sorted(fresh), findings)
    if not args.gate:
        return EXIT_OK
    return EXIT_NEW_FINDINGS if fresh else EXIT_OK


def emit(
    args: argparse.Namespace,
    state: str,
    summary: str,
    fields: dict[str, str],
    fresh: list[str],
    findings: list[dict[str, Any]],
) -> None:
    if args.json:
        print(
            json.dumps(
                {
                    "state": state,
                    "summary": summary,
                    **fields,
                    "new": fresh,
                    "findings": findings,
                },
                indent=2,
                sort_keys=True,
            )
        )
        return
    print(f"state={state}")
    print(f"summary={summary}")
    for key, value in fields.items():
        print(f"{key}={value}")
    # Name the new ones only. The standing backlog is carried as a count on
    # purpose: printing 122 lines every tick is how a reader learns to skip it.
    for key in fresh:
        kind, _, subject = key.partition("\t")
        print(f"new={kind} {subject}")


if __name__ == "__main__":
    raise SystemExit(main())
