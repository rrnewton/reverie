#!/usr/bin/env python3
"""Bound the Herdr ``validate-hermit`` workspace: cap, reap, and a TTL backstop.

WHY THIS EXISTS. ``pane_owner.create_pane`` issued one ``herdr tab create`` per
``ci-hub validate-run`` and returned a ``PaneHandle`` that **nothing ever
closed** -- there was no close, no TTL and no cap anywhere in ci-hub. Creation
was unconditional and destruction was nobody's job, so the workspace grew
without bound; it was observed at tab ``wC:t2E`` (46 tabs) on devbig176.

THE CONTRACT, in the order a caller meets it:

1. **Cap.** :data:`DEFAULT_PANE_CAP` panes per workspace. Before creating,
   :func:`ensure_capacity` reaps, and if the workspace is still full it evicts
   the oldest *reapable* pane to make room. If it cannot free a slot it
   **refuses to create** rather than growing past the cap, and the refusal names
   what is holding every slot.
2. **Lifecycle close.** A run that finishes is closed by the next reaper pass
   once its linger has elapsed -- see 4.
3. **TTL backstop.** A pane whose owning run died without cleanup is reaped once
   it is older than :data:`ORPHAN_TTL_SECONDS`. This is what makes the system
   self-healing rather than dependent on every script exiting cleanly.
4. **Cleanup delay.** A finished pane is deliberately kept for
   :data:`COMPLETED_LINGER_SECONDS` so a human can still read what an agent did
   after it exited. Cleanliness that destroys the evidence is not the goal.

WHAT IS NEVER REAPED, and why the asymmetry is deliberate. A live validate holds
the box-exclusive ``validate-lock``; closing its observer pane while it runs
destroys the only human-visible window onto a run that may have minutes of CPU
invested. A pane this module cannot *prove* is finished is therefore kept, and
the reason is reported. That mirrors ``agent-utils``'s ``herdr_run.reap``
("unknown is never reaped") and the parent's Verify-Before-You-Replace rule:
failing to prove life is not proving death.

Liveness is decided by the RUNNING THING, not by a field. The run record carries
``process_identity`` (boot_id, pid, start_ticks); :func:`process_alive`
re-derives that from ``/proc`` so a record left at ``state: running`` by a
crashed producer does not pin a slot forever.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pane_owner  # noqa: E402  (same-directory module, path set above)


#: Panes allowed in one workspace before a create must first free a slot.
DEFAULT_PANE_CAP = 20

#: How long a FINISHED run's pane is kept so its output stays readable.
COMPLETED_LINGER_SECONDS = 30 * 60

#: How long a pane with no live owner is kept before the backstop reaps it.
#: Longer than the p99 validate so a slow run is never mistaken for an orphan.
ORPHAN_TTL_SECONDS = 6 * 60 * 60

#: `pane_owner.create_pane` writes "<identity> | <sha12> | since <iso8601>".
_SINCE = re.compile(r"\bsince\s+(\S+)")

KEEP_RUNNING = "owner process is alive"
KEEP_LINGER = "finished, still inside the cleanup delay"
KEEP_YOUNG = "no owner record yet, younger than the orphan TTL"
KEEP_UNDATED = "no run record and no parseable start time -- unknown is never reaped"
REAP_FINISHED = "finished and past the cleanup delay"
REAP_ORPHAN = "no live owner and older than the orphan TTL"


@dataclass(frozen=True)
class Decision:
    tab_id: str
    label: str
    reapable: bool
    reason: str
    age_seconds: float | None


@dataclass(frozen=True)
class ReapReport:
    workspace_id: str | None
    decisions: tuple[Decision, ...]
    closed: tuple[str, ...]
    failed: tuple[tuple[str, str], ...]

    @property
    def kept(self) -> tuple[Decision, ...]:
        return tuple(d for d in self.decisions if not d.reapable)

    def render(self) -> str:
        """Always state both sides: "reaped 0 because nothing was stale" and
        "reaped 0 because the reaper is broken" must not look identical."""
        lines = [
            f"pane-lifecycle: workspace={self.workspace_id or '(absent)'} "
            f"panes={len(self.decisions)} closed={len(self.closed)} kept={len(self.kept)}"
        ]
        for tab in self.closed:
            lines.append(f"  closed {tab}")
        for tab, detail in self.failed:
            lines.append(f"  FAILED to close {tab}: {detail}")
        for decision in self.kept:
            age = "?" if decision.age_seconds is None else f"{decision.age_seconds / 60:.0f}m"
            lines.append(f"  kept   {decision.tab_id} age={age} -- {decision.reason}")
        return "\n".join(lines)


def _now() -> float:
    return time.time()


def parse_started_at(label: str) -> float | None:
    match = _SINCE.search(label or "")
    if not match:
        return None
    try:
        return datetime.fromisoformat(match.group(1)).timestamp()
    except ValueError:
        return None


def boot_id() -> str | None:
    try:
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        return None


def process_alive(identity: Mapping[str, Any] | None) -> bool:
    """True only when THIS pid, on THIS boot, still has the recorded start time.

    A bare ``kill(pid, 0)`` would call a recycled pid alive and keep a dead run's
    slot forever, which is exactly the failure this module exists to end.
    """
    if not isinstance(identity, Mapping):
        return False
    pid = identity.get("pid")
    ticks = identity.get("start_ticks")
    recorded_boot = identity.get("boot_id")
    if not isinstance(pid, int) or not isinstance(ticks, int):
        return False
    if isinstance(recorded_boot, str) and recorded_boot and recorded_boot != boot_id():
        return False
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[-1].split()
    except OSError:
        return False
    try:
        return int(fields[19]) == ticks
    except (IndexError, ValueError):
        return False


def load_records(runs_dir: Path) -> dict[str, dict[str, Any]]:
    """Index every run record by tab id. Unreadable records are skipped, not
    guessed at -- a record we cannot parse leaves its pane UNKNOWN, and unknown
    is kept."""
    out: dict[str, dict[str, Any]] = {}
    if not runs_dir.is_dir():
        return out
    for path in runs_dir.glob("*.json"):
        try:
            record = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        tab = record.get("tab_id")
        if isinstance(tab, str) and tab:
            out[tab] = record
    return out


def classify(
    tabs: Sequence[Mapping[str, Any]],
    records: Mapping[str, Mapping[str, Any]],
    *,
    now: float | None = None,
    linger_seconds: int = COMPLETED_LINGER_SECONDS,
    ttl_seconds: int = ORPHAN_TTL_SECONDS,
) -> tuple[Decision, ...]:
    moment = _now() if now is None else now
    decisions: list[Decision] = []
    for tab in tabs:
        tab_id = str(tab.get("tab_id") or "")
        label = str(tab.get("label") or "")
        if not tab_id:
            continue
        record = records.get(tab_id)
        started = parse_started_at(label)
        if record is not None:
            raw_started = record.get("started_at")
            if isinstance(raw_started, str):
                try:
                    started = datetime.fromisoformat(raw_started).timestamp()
                except ValueError:
                    pass
        age = None if started is None else max(0.0, moment - started)

        if record is not None and process_alive(record.get("process_identity")):
            decisions.append(Decision(tab_id, label, False, KEEP_RUNNING, age))
            continue

        finished_at = record.get("finished_at") if record is not None else None
        if isinstance(finished_at, str):
            try:
                done = datetime.fromisoformat(finished_at).timestamp()
            except ValueError:
                done = None
            if done is not None:
                if moment - done >= linger_seconds:
                    decisions.append(Decision(tab_id, label, True, REAP_FINISHED, age))
                else:
                    decisions.append(Decision(tab_id, label, False, KEEP_LINGER, age))
                continue

        if age is None:
            decisions.append(Decision(tab_id, label, False, KEEP_UNDATED, None))
        elif age >= ttl_seconds:
            decisions.append(Decision(tab_id, label, True, REAP_ORPHAN, age))
        else:
            decisions.append(Decision(tab_id, label, False, KEEP_YOUNG, age))
    return tuple(decisions)


def _run_host(command: Sequence[str], run, environment: Mapping[str, str]):
    return pane_owner.run_host(command, run=run, environment=environment)


def find_workspace(
    label: str, *, run, environment: Mapping[str, str]
) -> str | None:
    result = _run_host(["herdr", "workspace", "list"], run, environment)
    if result.returncode != 0:
        return None
    try:
        payload = json.loads(result.stdout).get("result", {})
    except (AttributeError, json.JSONDecodeError):
        return None
    for item in payload.get("workspaces", []):
        if isinstance(item, dict) and item.get("label") == label:
            workspace = item.get("workspace_id")
            if isinstance(workspace, str) and workspace:
                return workspace
    return None


def list_tabs(workspace: str, *, run, environment: Mapping[str, str]) -> list[dict[str, Any]]:
    result = _run_host(["herdr", "tab", "list", "--workspace", workspace], run, environment)
    if result.returncode != 0:
        raise RuntimeError(f"cannot list tabs in {workspace}: {result.stderr.strip() or result.stdout.strip()}")
    try:
        payload = json.loads(result.stdout).get("result", {})
    except (AttributeError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"herdr tab list returned non-JSON for {workspace}") from exc
    return [t for t in payload.get("tabs", []) if isinstance(t, dict)]


def close_tab(tab_id: str, *, run, environment: Mapping[str, str]) -> tuple[bool, str]:
    result = _run_host(["herdr", "tab", "close", tab_id], run, environment)
    if result.returncode == 0:
        return True, ""
    return False, (result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}")


def reap(
    root: Path,
    *,
    workspace_label: str = pane_owner.WORKSPACE_LABEL,
    run=pane_owner.run_command,
    environment: Mapping[str, str] | None = None,
    linger_seconds: int = COMPLETED_LINGER_SECONDS,
    ttl_seconds: int = ORPHAN_TTL_SECONDS,
    dry_run: bool = False,
    now: float | None = None,
) -> ReapReport:
    env = environment or os.environ
    workspace = find_workspace(workspace_label, run=run, environment=env)
    if workspace is None:
        return ReapReport(None, (), (), ())
    tabs = list_tabs(workspace, run=run, environment=env)
    records = load_records(root / "ignored" / "validate" / "runs")
    decisions = classify(
        tabs, records, now=now, linger_seconds=linger_seconds, ttl_seconds=ttl_seconds
    )
    closed: list[str] = []
    failed: list[tuple[str, str]] = []
    if not dry_run:
        for decision in decisions:
            if not decision.reapable:
                continue
            ok, detail = close_tab(decision.tab_id, run=run, environment=env)
            (closed if ok else failed).append(decision.tab_id if ok else (decision.tab_id, detail))
    return ReapReport(workspace, decisions, tuple(closed), tuple(failed))


class WorkspaceFull(RuntimeError):
    """Raised instead of creating pane number cap+1."""


def ensure_capacity(
    root: Path,
    *,
    workspace_label: str = pane_owner.WORKSPACE_LABEL,
    cap: int = DEFAULT_PANE_CAP,
    run=pane_owner.run_command,
    environment: Mapping[str, str] | None = None,
    linger_seconds: int = COMPLETED_LINGER_SECONDS,
    ttl_seconds: int = ORPHAN_TTL_SECONDS,
    now: float | None = None,
    report: Callable[[str], None] | None = None,
) -> ReapReport:
    """Guarantee room for one more pane, or refuse.

    Order matters: reap on the normal rules first, and only then start evicting
    live-but-old panes to make room. Evicting before reaping would throw away a
    pane a human might still want while a genuinely dead one sat next to it.
    """
    env = environment or os.environ
    say = report or (lambda _message: None)
    result = reap(
        root,
        workspace_label=workspace_label,
        run=run,
        environment=env,
        linger_seconds=linger_seconds,
        ttl_seconds=ttl_seconds,
        now=now,
    )
    if result.workspace_id is None:
        return result
    say(result.render())

    remaining = [d for d in result.decisions if d.tab_id not in set(result.closed)]
    if len(remaining) < cap:
        return result

    # Still full. Evict the oldest panes that are not provably alive, oldest
    # first, until one slot is free. A running validate is never a candidate.
    evictable = sorted(
        (d for d in remaining if d.reason != KEEP_RUNNING and d.age_seconds is not None),
        key=lambda d: d.age_seconds or 0.0,
        reverse=True,
    )
    closed = list(result.closed)
    failed = list(result.failed)
    while len(remaining) >= cap and evictable:
        victim = evictable.pop(0)
        ok, detail = close_tab(victim.tab_id, run=run, environment=env)
        if ok:
            closed.append(victim.tab_id)
            remaining = [d for d in remaining if d.tab_id != victim.tab_id]
            say(f"pane-lifecycle: evicted {victim.tab_id} to stay under cap={cap}")
        else:
            failed.append((victim.tab_id, detail))

    result = ReapReport(result.workspace_id, result.decisions, tuple(closed), tuple(failed))
    if len(remaining) >= cap:
        holders = ", ".join(f"{d.tab_id} ({d.reason})" for d in remaining[:cap])
        raise WorkspaceFull(
            f"Herdr workspace {workspace_label!r} is at its cap of {cap} panes and no slot could be "
            f"freed. Refusing to create pane {cap + 1}. Every slot is held by a pane that could not be "
            f"proven finished: {holders}. Close one by hand (`herdr tab close <id>`) or raise the cap "
            f"with CI_HUB_PANE_CAP."
        )
    return result


def _int_env(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return value if value > 0 else default


def cap_from_env() -> int:
    return _int_env("CI_HUB_PANE_CAP", DEFAULT_PANE_CAP)


def linger_from_env() -> int:
    return _int_env("CI_HUB_PANE_LINGER_SECONDS", COMPLETED_LINGER_SECONDS)


def ttl_from_env() -> int:
    return _int_env("CI_HUB_PANE_TTL_SECONDS", ORPHAN_TTL_SECONDS)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--workspace-label", default=pane_owner.WORKSPACE_LABEL)
    parser.add_argument("--cap", type=int, default=cap_from_env())
    parser.add_argument("--linger-seconds", type=int, default=linger_from_env())
    parser.add_argument("--ttl-seconds", type=int, default=ttl_from_env())
    parser.add_argument("--dry-run", action="store_true", help="classify and report, close nothing")
    parser.add_argument("--ensure-capacity", action="store_true",
                        help="also evict oldest non-running panes until one slot is free")
    args = parser.parse_args(argv)

    if args.ensure_capacity and not args.dry_run:
        try:
            report = ensure_capacity(
                args.root,
                workspace_label=args.workspace_label,
                cap=args.cap,
                linger_seconds=args.linger_seconds,
                ttl_seconds=args.ttl_seconds,
                report=print,
            )
        except WorkspaceFull as exc:
            print(str(exc), file=sys.stderr)
            return 2
    else:
        report = reap(
            args.root,
            workspace_label=args.workspace_label,
            linger_seconds=args.linger_seconds,
            ttl_seconds=args.ttl_seconds,
            dry_run=args.dry_run,
        )
        print(report.render())
    return 1 if report.failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
