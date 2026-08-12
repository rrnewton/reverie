#!/usr/bin/env python3
"""The one validation-ledger boundary used by producers and consumers.

The authority is the append-only event union exposed by this module. Producers
first append to the ignored machine-local spool under
``ignored/ci-hub/validate-ledger-spool``; the publisher copies those exact
events into the tracked per-team/per-machine shards under
``ledger/<team>/<host>/<YYYY>-<MM>.jsonl``. Readers union both locations, so an
event is visible before publication and collapses to one event after it lands.

No checkout-local or parent-discovered receipt file is consulted here.  Keeping
path resolution in this module is intentional: a new reader cannot quietly
invent another source of truth.
"""

from __future__ import annotations

import argparse
import json
import os
import hashlib
import socket
import sys
from pathlib import Path
from typing import Iterable

from ledger import read_shard, union_events, validate_event
from publisher import retained, spool, spooled
WORKSPACE_TOKEN = "{{WORKSPACE_ROOT}}"
HOME_TOKEN = "{{HOME}}"
LIVE_SPOOL_RELATIVE = Path("ignored/ci-hub/validate-ledger-spool")
# This producer was used by a live-sync acceptance probe that accidentally
# wrote 300 synthetic PASS rows into the production Hermit shard.  The shard is
# append-only, so those bytes remain historical evidence, but they are test
# data rather than validation observations and must never enter the canonical
# authority view again.
ACCEPTANCE_FIXTURE_PRODUCERS = frozenset({"ledger-live-sync-acceptance"})


def _producer_tool(value: object) -> str:
    if isinstance(value, dict):
        return str(value.get("tool") or "")
    return str(value or "")


def _is_acceptance_fixture_row(row: object) -> bool:
    return (
        isinstance(row, dict)
        and _producer_tool(row.get("producer")) in ACCEPTANCE_FIXTURE_PRODUCERS
    )


def _is_acceptance_fixture_event(event: dict) -> bool:
    return _producer_tool(event.get("producer")) in ACCEPTANCE_FIXTURE_PRODUCERS or (
        _is_acceptance_fixture_row(event.get("legacy_row"))
    )


def repository_root() -> Path:
    if os.environ.get("HERMIT_VALIDATE_STOP_TEST_MODE") == "1":
        test_root = os.environ.get("CI_HUB_VALIDATE_LEDGER_TEST_ROOT")
        if test_root:
            return Path(test_root)
    return Path(__file__).resolve().parents[2]


def short_hostname() -> str:
    return socket.gethostname().split(".", 1)[0]


def shard_paths(root: Path) -> list[Path]:
    """Every published primary or migration-preservation shard."""
    primary = root.glob("ledger/*/*/[0-9][0-9][0-9][0-9]-[0-9][0-9].jsonl")
    preserved = root.glob(
        "ledger-recovery/*/*/[0-9][0-9][0-9][0-9]-[0-9][0-9]/*.jsonl"
    )
    return sorted([*primary, *preserved])


def live_spool_dir(root: Path) -> Path:
    """The ignored append-only producer store for this parent checkout."""
    return root / LIVE_SPOOL_RELATIVE


def _replace(value, old: str, new: str):
    if isinstance(value, str):
        return value.replace(old, new)
    if isinstance(value, dict):
        return {key: _replace(item, old, new) for key, item in value.items()}
    if isinstance(value, list):
        return [_replace(item, old, new) for item in value]
    return value


def _workspace_root_for_row(row: dict, fallback: Path) -> str:
    cwd = str(row.get("cwd") or "")
    if "/worktrees/" in cwd:
        return cwd.split("/worktrees/", 1)[0]
    # Historical admitted runs also used ephemeral checkouts directly under
    # dev-hermit/ignored (for example ignored/validate-fresh-*).  Those paths
    # have no trailing product directory, but they still carry the canonical
    # workspace boundary.  Recognise that boundary explicitly so migration
    # preserves the row without leaking an owner path or weakening the ledger
    # validator.
    if "/dev-hermit/" in cwd:
        prefix, _ = cwd.split("/dev-hermit/", 1)
        return prefix + "/dev-hermit"
    if cwd.endswith("/hermit") or cwd.endswith("/reverie"):
        return str(Path(cwd).parent)
    return str(fallback)


def _owner_home_for_workspace(workspace: str) -> str | None:
    parts = Path(workspace).parts
    if len(parts) >= 3 and parts[0] == "/" and parts[1] == "home":
        return str(Path(parts[0]) / parts[1] / parts[2])
    return None


def _ledger_team(repo: object) -> str:
    """Map repository identity to the path-safe ledger shard identity.

    ``repo`` remains unmodified inside ``legacy_row`` so readers can bind the
    receipt to the exact repository.  The team is a filesystem/shard key,
    however, and must never inherit the slash in a canonical ``owner/repo``
    identity: the publisher intentionally snapshots only top-level batches.
    """
    value = str(repo or "hermit")
    aliases = {
        "hermit": "hermit",
        "rrnewton/hermit": "hermit",
        "reverie": "reverie",
        "rrnewton/reverie": "reverie",
    }
    if value in aliases:
        return aliases[value]
    if "/" in value or "\\" in value or "__" in value:
        raise ValueError(f"repository has no path-safe ledger team mapping: {value!r}")
    return value


def _event_for_row(row: dict, root: Path) -> dict:
    record_id = str(row.get("record_id") or "")
    if not record_id:
        body = json.dumps(row, sort_keys=True, separators=(",", ":")).encode()
        record_id = "legacy-row-" + hashlib.sha256(body).hexdigest()
    host = str(row.get("host") or short_hostname()).split(".", 1)[0]
    emitted_at = str(row.get("finished_at") or "")
    if len(emitted_at) < 7:
        raise ValueError("validation row has no usable finished_at")
    team = _ledger_team(row.get("repo"))
    outcome = str(row.get("result") or "no_result")
    if outcome not in {"pass", "fail", "timeout", "incomplete", "killed", "no_result"}:
        outcome = "no_result"
    workspace_root = _workspace_root_for_row(row, root)
    portable = _replace(row, workspace_root, WORKSPACE_TOKEN)
    # Failure text can mention cargo/git checkouts outside the dev-hermit tree.
    # Keep the diagnostic shape while stripping the machine owner's identity;
    # otherwise a perfectly valid failing run cannot be journaled at all.
    owner_home = _owner_home_for_workspace(workspace_root)
    if owner_home:
        portable = _replace(portable, owner_home, HOME_TOKEN)
    event = {
        "schema": "validate-ledger/v1",
        "event_id": record_id,
        "event_type": "run.result",
        "run_id": record_id,
        "emitted_at": emitted_at,
        "team": team,
        "host": host,
        "producer": {
            "source": "observed",
            "tool": str(row.get("producer") or "unknown"),
            "tool_version": str(row.get("schema_version") or "unknown"),
        },
        "outcome": outcome,
        "commit": row.get("commit"),
        "started_at": row.get("started_at"),
        "workspace": {"repo_relative": portable.get("cwd")},
        "legacy_row": portable,
    }
    rel = f"ledger/{team}/{host}/{emitted_at[:7]}.jsonl"
    violations = validate_event(event, rel)
    if violations:
        raise ValueError(f"validation event violates {','.join(violations)}")
    return event


def record(row: dict, *, root: Path | None = None) -> dict:
    """Append one observed row before any Git operation can block or tear it."""
    production_root = repository_root().resolve()
    root = (root or production_root).resolve()
    if root == production_root and _is_acceptance_fixture_row(row):
        raise ValueError(
            "acceptance fixture producer is not allowed in the production ledger"
        )
    event = _event_for_row(row, root)
    target = spool(live_spool_dir(root), event["team"], event["host"], [event])
    return {
        "schema": "ci-hub-validate-ledger-record/v1",
        "event_id": event["event_id"],
        "run_id": event["run_id"],
        "spool": target.relative_to(root).as_posix(),
        "published_shard": (
            Path("ledger")
            / event["team"]
            / event["host"]
            / f"{event['emitted_at'][:7]}.jsonl"
        ).as_posix(),
    }


def _read_rows(path: Path) -> list[dict]:
    text = path.read_text()
    if text and not text.endswith("\n"):
        raise ValueError(f"{path}: truncated final ledger line")
    out = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        value = json.loads(line)
        if not isinstance(value, dict):
            raise ValueError(f"{path}:{number}: row is not an object")
        out.append(value)
    return out


def events(*, root: Path | None = None) -> list[dict]:
    """Canonical validation events, excluding acceptance-test data.

    Fixture bytes remain in their append-only shard or isolated test store.
    They are deliberately absent from this authority view so ``result=pass``
    cannot make an acceptance probe indistinguishable from a validation run.
    """
    root = (root or repository_root()).resolve()
    gathered: list[dict] = []
    for path in shard_paths(root):
        gathered.extend(read_shard(path))
    for pending in spooled(live_spool_dir(root)).values():
        gathered.extend(pending)
    # Retained batches are PUBLISHED but not yet durable in THIS checkout. A
    # reader that skipped them would blink: publication writes through a
    # separate clone, so between the publish and this root's next sync the
    # receipt is in neither the spool nor the tracked shards. Measured
    # 2026-08-11 -- a producer answered NOT-VALIDATED with 0 records for a
    # receipt it had published seconds earlier. Unioning them closes that gap;
    # event_id dedup makes the overlap with tracked shards free.
    for published in retained(live_spool_dir(root)).values():
        gathered.extend(published)
    return [
        event
        for event in union_events(gathered)
        if not _is_acceptance_fixture_event(event)
    ]


def rows(*, root: Path | None = None) -> list[dict]:
    """HistoryRow compatibility view of the one canonical event union."""
    root = (root or repository_root()).resolve()
    out: list[dict] = []
    seen: set[str] = set()
    for event in events(root=root):
        row = event.get("legacy_row")
        if isinstance(row, dict):
            restored = _replace(row, WORKSPACE_TOKEN, str(root))
            identity = json.dumps(restored, sort_keys=True, separators=(",", ":"))
            if identity not in seen:
                seen.add(identity)
                out.append(restored)
    return out


def emit_jsonl(items: Iterable[dict]) -> None:
    for item in items:
        print(json.dumps(item, sort_keys=True, separators=(",", ":")))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("rows", description="emit the canonical HistoryRow compatibility view", help="emit the canonical HistoryRow compatibility view")
    commands.add_parser("events", description="emit the canonical event union", help="emit the canonical event union")
    commands.add_parser("record", description="read one HistoryRow JSON object from stdin and journal it", help="read one HistoryRow JSON object from stdin and journal it")
    args = parser.parse_args(argv)
    if args.command == "record":
        try:
            value = json.load(sys.stdin)
            if not isinstance(value, dict):
                raise ValueError("input is not a JSON object")
            print(json.dumps(record(value), sort_keys=True))
            return 0
        except (OSError, ValueError, json.JSONDecodeError) as error:
            print(f"validate-ledger: REFUSED: {error}", file=sys.stderr)
            return 2
    emit_jsonl(events() if args.command == "events" else rows())
    return 0


if __name__ == "__main__":
    sys.exit(main())
