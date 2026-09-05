#!/usr/bin/env python3
"""Find work made ownerless by correct refusals and agent recycling.

This is deliberately a report/router, never a cleaner.  A slot is considered
held by a dead owner only when both independent facts agree: no registered
owner is in the live ORC fleet *and* no same-user process has a cwd below the
slot.  That second condition prevents a live delegated worker without a tmux
window from being mistaken for residue.

The tick-hub slot-cap gate uses the narrower ``guarded_dead_owner_slots``
interface below.  It does not consult the ORC fleet or validate-lock: it accepts
only each slot row's schema-versioned owner sidecar through
``slot-owner-authority.py``.  Only the typed ``dead`` verdict is proof of
absence; alive, indeterminate, malformed, or unrelated owners produce no
dead-owner finding.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable, Sequence
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402
from worktree_registry import (  # noqa: E402
    RegistrySchemaError,
    read_registry as read_typed_registry,
)


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import taskgraph_db  # noqa: E402

TASK_ID = "periodic-residue-sweep-correct-refusals-leave-unowned-work"
OWNER_QUEUE = "owner-decision-queue-2026-08-04"
ORPHAN_DETECTOR = ROOT / "scripts" / "orphaned-task-detector.sh"
REGISTRY = ROOT / "worktree-state.json"
OWNER_AUTHORITY_TIMEOUT_SECS = 10

NOTE_MARKERS = (
    "did not commit",
    "not committed",
    "untracked",
    "needs authorization",
    "left for coordinator",
    "left for the coordinator",
    "no reset/clean/force-push done",
)
OWNER_MARKERS = (
    "needs authorization",
    "owner decision",
    "owner-decision",
    "irreversible",
)
NO_ACTION_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"\bnothing (?:is |was )?(?:left )?(?:uncommitted|untracked)\b",
        r"\bnothing to commit\b",
        r"\bno (?:files?|changes?|work) (?:are |is |were |was )?(?:left )?(?:uncommitted|untracked)\b",
        r"\bno (?:code|files?) (?:was |were )?(?:changed|modified)\b",
        r"\bno (?:follow[- ]?up|action) (?:is )?(?:needed|required)\b",
    )
)
DISPOSITION_RE = re.compile(r"source_note_ids?=([0-9,]+)")


class SweepUnavailable(RuntimeError):
    """An authority could not be read; silence is not a clean result."""


@dataclass(frozen=True)
class NoteRow:
    note_id: int
    task: str
    owner: str
    content: str


@dataclass(frozen=True)
class Residue:
    kind: str
    key: str
    evidence: str
    route_task: str
    route_authority: str
    disposition: str = "route"

    @property
    def actionable(self) -> bool:
        return self.disposition == "route"


def run_checked(
    argv: Sequence[str], db: Path | None = None
) -> subprocess.CompletedProcess[str]:
    """Run a child bound to the resolved TaskGraph, or refuse.

    Every child here reaches the graph through `tg` -- directly, or inside
    scripts/orphaned-task-detector.sh.  `tg` with no database named reads an
    empty `tasks` default and returns 0 rows, which this sweep would have
    reported as a clean, ownerless-work-free fleet.  Binding the child
    explicitly is what makes that unrepresentable: unresolvable is
    could-not-measure, never zero.
    """

    try:
        env = taskgraph_db.child_env(taskgraph_db.resolve(db))
    except taskgraph_db.TaskGraphUnavailable as error:
        raise SweepUnavailable(str(error)) from error
    return bounded_capture.capture(
        argv,
        cwd=ROOT,
        env=env,
    )


def read_live_fleet(db: Path | None = None) -> tuple[set[str], list[Residue]]:
    result = run_checked((str(ORPHAN_DETECTOR), "--gate"), db)
    output = result.stdout + result.stderr
    if result.returncode not in (0, 1):
        raise SweepUnavailable(
            f"orphan detector rc={result.returncode}: {' '.join(output.split())}"
        )

    live: set[str] = set()
    orphans: list[Residue] = []
    in_table = False
    for line in result.stdout.splitlines():
        stripped = line.strip()
        if stripped.startswith("live agents:"):
            live.update(stripped.removeprefix("live agents:").split())
        if stripped.startswith("---"):
            in_table = not in_table
            continue
        if not in_table or not stripped or stripped.startswith("TASK "):
            continue
        fields = stripped.split()
        if len(fields) != 4 or fields[2] not in {"IN_PROGRESS", "OPEN", "BACKLOG"}:
            continue
        task, owner, status, age_h = fields
        orphans.append(
            Residue(
                kind="orphaned-task",
                key=task,
                evidence=f"owner={owner} status={status} age_h={age_h}",
                route_task=TASK_ID,
                route_authority="coordinator",
            )
        )
    if not live:
        raise SweepUnavailable("orphan detector returned no live-agent identity set")
    return live, orphans


def _sql_rows(query: str) -> list[str]:
    result = run_checked(("tg", "sql", query))
    if result.returncode != 0:
        raise SweepUnavailable(
            f"TaskGraph query rc={result.returncode}: {' '.join(result.stderr.split())}"
        )
    return [line for line in result.stdout.splitlines() if line.startswith("ROW|")]


def read_note_rows() -> tuple[list[NoteRow], set[int]]:
    marker_sql = " OR ".join(
        "lower(n.content) LIKE '%" + marker.replace("'", "''") + "%'"
        for marker in NOTE_MARKERS
    )
    rows = _sql_rows(
        "SELECT 'ROW|' || n.id || '|' || t.local_id || '|' || "
        "coalesce(t.owner,'') || '|' || hex(n.content) "
        "FROM task_notes n JOIN tasks t ON t.local_id=n.task_id "
        "WHERE t.status='IN_PROGRESS' AND (" + marker_sql + ") ORDER BY n.id"
    )
    notes: list[NoteRow] = []
    for row in rows:
        _, raw_id, task, owner, content_hex = row.split("|", 4)
        try:
            content = bytes.fromhex(content_hex).decode("utf-8")
        except (ValueError, UnicodeDecodeError) as exc:
            raise SweepUnavailable(f"malformed TaskGraph note row {raw_id}: {exc}") from exc
        notes.append(NoteRow(int(raw_id), task, owner, content))

    disposed: set[int] = set()
    for row in _sql_rows(
        "SELECT 'ROW|' || id || '|' || hex(content) FROM task_notes "
        "WHERE content LIKE '%RESIDUE-DISPOSITION:%'"
    ):
        _, _note_id, content_hex = row.split("|", 2)
        try:
            content = bytes.fromhex(content_hex).decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            continue
        for match in DISPOSITION_RE.finditer(content):
            disposed.update(int(value) for value in match.group(1).split(","))
    return notes, disposed


def classify_notes(notes: Iterable[NoteRow], disposed: set[int]) -> list[Residue]:
    by_task: dict[tuple[str, str, str, str], list[int]] = {}
    for note in notes:
        if note.note_id in disposed:
            continue
        no_action = any(pattern.search(note.content) for pattern in NO_ACTION_PATTERNS)
        needs_owner = any(marker in note.content.lower() for marker in OWNER_MARKERS)
        disposition = "no-action" if no_action else "route"
        route_task = OWNER_QUEUE if needs_owner and not no_action else TASK_ID
        authority = "owner" if needs_owner and not no_action else "coordinator"
        if no_action:
            route_task, authority = "none", "none"
        key = (note.task, route_task, authority, disposition)
        by_task.setdefault(key, []).append(note.note_id)

    residues: list[Residue] = []
    for (task, route_task, authority, disposition), note_ids in sorted(by_task.items()):
        joined = ",".join(str(note_id) for note_id in note_ids)
        residues.append(
            Residue(
                kind="declined-action",
                key=task,
                evidence=f"source_note_ids={joined}",
                route_task=route_task,
                route_authority=authority,
                disposition=disposition,
            )
        )
    return residues


def process_cwds(proc_root: Path = Path("/proc")) -> set[Path]:
    cwds: set[Path] = set()
    uid = os.getuid()
    try:
        entries = tuple(proc_root.iterdir())
    except OSError as exc:
        raise SweepUnavailable(f"cannot enumerate {proc_root}: {exc}") from exc
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            if entry.stat().st_uid != uid:
                continue
            cwds.add((entry / "cwd").resolve(strict=True))
        except (FileNotFoundError, PermissionError, OSError):
            continue
    return cwds


def classify_slots(
    registry: dict[str, object], live_agents: set[str], cwds: set[Path]
) -> list[Residue]:
    raw_slots = registry.get("slots", {})
    if not isinstance(raw_slots, dict):
        raise SweepUnavailable("worktree-state.json slots is not an object")
    residues: list[Residue] = []
    for slot, raw in sorted(raw_slots.items()):
        if not isinstance(raw, dict) or raw.get("status") not in {"active", "held"}:
            continue
        raw_agents = raw.get("agents", [])
        agents = {
            str(agent.get("name", ""))
            for agent in raw_agents
            if isinstance(agent, dict) and agent.get("name")
        }
        if not agents or agents & live_agents:
            continue
        slot_path = (ROOT / "worktrees" / slot).resolve()
        process_bound = any(
            cwd == slot_path or slot_path in cwd.parents for cwd in cwds
        )
        if process_bound:
            continue
        tasks = sorted(
            {
                str(agent.get("task", ""))
                for agent in raw_agents
                if isinstance(agent, dict) and agent.get("task")
            }
        )
        residues.append(
            Residue(
                kind="held-slot-dead-owner",
                key=slot,
                evidence=(
                    f"owners={','.join(sorted(agents))} tasks={','.join(tasks) or 'none'} "
                    "live_owner=0 process_cwd_under_slot=0"
                ),
                route_task=TASK_ID,
                route_authority="coordinator-slot-lifecycle",
            )
        )
    return residues


def read_guarded_owner_authorities(
    registry: dict[str, object], root: Path = ROOT, fixture: Path | None = None
) -> dict[str, dict[str, object]]:
    """Read one guarded process-identity verdict for every active slot.

    A fixture file is accepted only as an explicit function argument so tests
    can bracket the consumer without forging the production authority. The
    production verifier has no path override and takes the registry lock for
    each exact slot snapshot.
    """

    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise SweepUnavailable("worktree-state.json slots is not an object")
    selected = sorted(
        str(slot)
        for slot, row in raw_slots.items()
        if isinstance(row, dict) and row.get("status") in {"active", "held"}
    )

    if fixture is not None:
        try:
            raw = fixture.read_text(encoding="utf-8")
        except OSError as exc:
            raise SweepUnavailable(
                f"cannot read owner-authority fixture {fixture}: {exc}"
            ) from exc
        try:
            value = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise SweepUnavailable(
                f"guarded owner authority returned invalid JSON: {exc}"
            ) from exc
        if (
            not isinstance(value, dict)
            or value.get("schema_version") != 1
            or not isinstance(value.get("authorities"), dict)
        ):
            raise SweepUnavailable("guarded owner authority has unsupported schema")
        authorities = value["authorities"]
    else:
        command = root / "scripts" / "slot-owner-authority.py"
        try:
            result = subprocess.run(
                (sys.executable, str(command), "--all", "--json"),
                cwd=root,
                text=True,
                capture_output=True,
                timeout=OWNER_AUTHORITY_TIMEOUT_SECS,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise SweepUnavailable(f"slot owner authority unavailable: {exc}") from exc
        if result.returncode != 0:
            detail = " ".join((result.stdout + result.stderr).split())
            raise SweepUnavailable(
                f"bulk slot owner authority rc={result.returncode}: {detail}"
            )
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError as exc:
            raise SweepUnavailable(
                f"bulk slot owner authority returned invalid JSON: {exc}"
            ) from exc
        expected_digest = hashlib.sha256(
            json.dumps(
                registry, sort_keys=True, separators=(",", ":"), ensure_ascii=True
            ).encode("utf-8")
        ).hexdigest()
        if (
            not isinstance(payload, dict)
            or payload.get("schema_version") != 1
            or payload.get("registry_sha256") != expected_digest
            or not isinstance(payload.get("authorities"), dict)
        ):
            raise SweepUnavailable(
                "bulk slot owner authority is not bound to the registry snapshot"
            )
        authorities = payload["authorities"]

    for slot in selected:
        authority = authorities.get(slot)
        if (
            not isinstance(authority, dict)
            or authority.get("schema_version") != 1
            or authority.get("state") not in {"alive", "dead", "indeterminate"}
            or authority.get("slot") != slot
        ):
            raise SweepUnavailable(f"slot owner authority for {slot} has invalid shape")
        if authority["state"] in {"alive", "dead"}:
            row = raw_slots[slot]
            sidecar = row.get("owner_sidecar")
            identity = authority.get("owner_identity")
            agent = authority.get("agent")
            if (
                not isinstance(sidecar, dict)
                or not isinstance(identity, dict)
                or not isinstance(agent, str)
                or not agent
                or sidecar.get("agent") != agent
                or sidecar.get("generation") != identity.get("generation")
            ):
                raise SweepUnavailable(
                    f"slot owner authority for {slot} is not bound to the registry snapshot"
                )
    return {slot: authorities[slot] for slot in selected}


def guarded_dead_owner_slots(
    registry: dict[str, object],
    authorities: dict[str, dict[str, object]],
    occupied_slots: set[str],
) -> list[Residue]:
    """Return slots whose exact recorded owner incarnation is proven dead.

    This deliberately does not turn absence from a name census into death.
    The slot authority already did the PID-reuse and reboot checks under the
    registry guard. A live process using the slot still wins: dead registration
    ownership is evidence, but never permission to call a busy directory
    reclaimable.
    """

    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise SweepUnavailable("worktree-state.json slots is not an object")

    residues: list[Residue] = []
    authority_source = (
        "wrkslots-owner"
        if registry.get("_source") == "wrkslots"
        else "slot-owner-sidecar"
    )
    for slot, raw in sorted(raw_slots.items()):
        if not isinstance(raw, dict) or raw.get("status") not in {"active", "held"}:
            continue
        authority = authorities[slot]
        if authority.get("state") != "dead":
            continue
        if slot in occupied_slots:
            continue
        agent = authority.get("agent")
        identity = authority.get("owner_identity")
        if not isinstance(agent, str) or not isinstance(identity, dict):
            raise SweepUnavailable(
                f"dead owner authority for {slot} omitted its guarded identity"
            )
        pid = identity.get("supervisor_pid")
        boot_id = identity.get("boot_id")
        start_ticks = identity.get("start_ticks")
        if (
            type(pid) is not int
            or pid <= 0
            or not isinstance(boot_id, str)
            or not boot_id
            or type(start_ticks) is not int
            or start_ticks <= 0
        ):
            raise SweepUnavailable(
                f"dead owner authority for {slot} has an incomplete identity"
            )
        task = raw.get("task")
        residues.append(
            Residue(
                kind="held-slot-dead-owner",
                key=str(slot),
                evidence=(
                    f"owner={agent} tasks={task or 'none'} "
                    f"owner_identity={pid}@{start_ticks} boot_id={boot_id} "
                    f"authority={authority_source} authority_state=dead "
                    "live_processes_under_slot=0"
                ),
                route_task=TASK_ID,
                route_authority="coordinator-slot-lifecycle",
            )
        )
    return residues


def guarded_live_owner_slots(
    registry: dict[str, object], authorities: dict[str, dict[str, object]]
) -> set[str]:
    """Slots whose registered owner is the exact guarded live process.

    Positive liveness wins over an idle clock.  This is the other half of the
    dead-owner predicate: the tick must not recommend reclaiming a slot merely
    because its live owner has not touched files there recently.
    """

    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise SweepUnavailable("worktree-state.json slots is not an object")
    return {
        str(slot)
        for slot, raw in raw_slots.items()
        if isinstance(raw, dict)
        and raw.get("status") in {"active", "held"}
        and authorities[slot].get("state") == "alive"
    }


def guarded_unassessable_owner_slots(
    registry: dict[str, object], authorities: dict[str, dict[str, object]]
) -> set[str]:
    """Held slots whose owner liveness cannot be decided in EITHER direction.

    THE THIRD OUTCOME, and until now the silent one.  ``guarded_dead_owner_slots``
    names owners proven dead and ``guarded_live_owner_slots`` names owners proven
    alive.  Everything else -- a slot with no ``owner_sidecar``, a sidecar that
    fails its schema-exact check, an unreadable ``/proc`` or cgroup -- lands in
    neither set and therefore reads to a caller exactly like an unowned slot.
    It is not one.  It is a slot whose owner might be working in it right now.

    WHY THIS IS KEYED ON THE TYPED ``indeterminate`` VERDICT rather than on a
    re-derived "has it got lease fields" test: the authority already owns that
    judgement, its reason codes distinguish the causes, and a second opinion
    computed here could disagree with it.  ``indeterminate`` is a superset of
    "the allocator recorded no owner evidence" -- it also covers a transient
    unreadable ``/proc`` -- and every member of the difference is one more slot
    this predicate declines to name.  Wider here is strictly safer.

    THE PROPERTY THIS EXISTS FOR: a caller may only ever REMOVE slots from a
    reclaim list on the strength of this set, never add one.  A wrong answer
    therefore costs disk, and a wrong answer in the other design would cost
    somebody's uncommitted work.  Those prices are not comparable.

    ``read_only`` owners are ignored: an observer holding a slot open is not the
    incarnation whose death would free it, and a row with no mutating owner has
    nothing for the authority to have been unable to assess.
    """

    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise SweepUnavailable("worktree-state.json slots is not an object")
    return {
        str(slot)
        for slot, raw in raw_slots.items()
        if isinstance(raw, dict)
        and raw.get("status") in {"active", "held"}
        and isinstance(raw.get("agents"), list)
        and any(
            isinstance(agent, dict) and agent.get("read_only") is not True
            for agent in raw["agents"]
        )
        and authorities[slot].get("state") == "indeterminate"
    }


def read_registry() -> dict[str, object]:
    try:
        value = read_typed_registry(REGISTRY)
    except RegistrySchemaError as exc:
        raise SweepUnavailable(f"cannot read {REGISTRY}: {exc}") from exc
    return value


def route(residues: Iterable[Residue]) -> None:
    grouped: dict[str, list[Residue]] = {}
    for residue in residues:
        destination = residue.route_task if residue.actionable else TASK_ID
        grouped.setdefault(destination, []).append(residue)
    for route_task, items in sorted(grouped.items()):
        lines = [
            "FROM periodic-residue-sweep-correct-refusals-leave-unowned-work: "
            "RESIDUE-DISPOSITION: routed by the periodic local sweep; no slot was "
            "cleaned/released and no task was closed/reassigned."
        ]
        for item in items:
            lines.append(
                f"- kind={item.kind} key={item.key} {item.evidence} "
                f"disposition={item.disposition} route={item.route_authority}"
            )
        result = run_checked(("tg", "note", route_task, "\n".join(lines)))
        if result.returncode != 0:
            raise SweepUnavailable(
                f"failed routing {len(items)} item(s) to {route_task}: "
                f"{' '.join(result.stderr.split())}"
            )


def build_report() -> tuple[list[Residue], set[str]]:
    live, orphaned = read_live_fleet()
    notes, disposed = read_note_rows()
    slots = classify_slots(read_registry(), live, process_cwds())
    return orphaned + classify_notes(notes, disposed) + slots, live


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--json", action="store_true", help="emit the complete typed report")
    parser.add_argument("--gate", action="store_true", help="exit 1 when actionable residue exists")
    parser.add_argument("--route", action="store_true", help="append typed routes to authority tasks")
    args = parser.parse_args(argv)
    try:
        residues, live = build_report()
        if args.route:
            route(residues)
    except SweepUnavailable as exc:
        print(f"state=unknown\nsummary=residue sweep unavailable: {exc}", file=sys.stderr)
        return 2

    actionable = [item for item in residues if item.actionable]
    counts = {
        kind: sum(1 for item in residues if item.kind == kind and item.actionable)
        for kind in ("orphaned-task", "declined-action", "held-slot-dead-owner")
    }
    if args.json:
        print(
            json.dumps(
                {
                    "state": "actionable" if actionable else "clean",
                    "live_agents": sorted(live),
                    "counts": counts,
                    "actionable_count": len(actionable),
                    "items": [asdict(item) for item in residues],
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(f"state={'actionable' if actionable else 'clean'}")
        print(
            "summary="
            f"actionable={len(actionable)} orphaned_tasks={counts['orphaned-task']} "
            f"declined_actions={counts['declined-action']} "
            f"dead_owner_slots={counts['held-slot-dead-owner']} "
            f"live_agents={len(live)}"
        )
        for item in residues:
            print(
                f"item={item.kind}:{item.key} disposition={item.disposition} "
                f"route={item.route_authority}:{item.route_task} evidence={item.evidence}"
            )
    return 1 if args.gate and actionable else 0


if __name__ == "__main__":
    raise SystemExit(main())
