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
import copy
import json
import os
import hashlib
import socket
import subprocess
import sys
from pathlib import Path
from typing import Iterable

from ledger import read_shard, union_events, validate_event, validate_events
from publisher import retained, spool, spooled

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualifying_receipt  # noqa: E402

WORKSPACE_TOKEN = "{{WORKSPACE_ROOT}}"
HOME_TOKEN = "{{HOME}}"
LIVE_SPOOL_RELATIVE = Path("ignored/ci-hub/validate-ledger-spool")
# This producer was used by a live-sync acceptance probe that accidentally
# wrote 300 synthetic PASS rows into the production Hermit shard.  The shard is
# append-only, so those bytes remain historical evidence, but they are test
# data rather than validation observations and must never enter the canonical
# authority view again.
ACCEPTANCE_FIXTURE_PRODUCERS = frozenset({"ledger-live-sync-acceptance"})
RUN_NUMBER_ENV = "CI_HUB_VALIDATE_RUN_NUMBER"


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
    checkout_root = Path(__file__).resolve().parents[2]
    git_env = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    git_env["GIT_CONFIG_NOSYSTEM"] = "1"
    git_env["GIT_CONFIG_GLOBAL"] = os.devnull
    try:
        result = subprocess.run(
            [
                "git",
                "--no-replace-objects",
                "-C",
                str(checkout_root),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
            env=git_env,
        )
    except (OSError, subprocess.TimeoutExpired):
        return checkout_root
    if result.returncode != 0 or not result.stdout.strip():
        return checkout_root

    common_dir = Path(result.stdout.strip()).resolve()
    if common_dir.name != ".git":
        return checkout_root
    shared_root = common_dir.parent
    if not (shared_root / "ci-hub/ledger/validate_rows.py").is_file():
        return checkout_root
    return shared_root


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
    # Admitted runs use ephemeral checkouts beneath dev-hermit. Current runs use
    # worktrees/validate; historical runs used ignored/validate-fresh-*. These
    # paths have no trailing product directory, but they still carry the
    # canonical workspace boundary. Recognise that boundary explicitly so
    # migration preserves the row without leaking an owner path or weakening
    # the ledger validator.
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


def _require_local_commit(row: dict) -> Path:
    """Refuse before classification unless ``commit`` is real in ``cwd``.

    A syntactically plausible SHA is not authority. In particular, an object
    from a different repository must not acquire a verdict merely because Git
    can resolve it somewhere else on the machine.
    """
    commit = row.get("commit")
    checkout_value = row.get("cwd")
    if not (
        isinstance(commit, str)
        and len(commit) == 40
        and commit == commit.lower()
        and all(character in "0123456789abcdef" for character in commit)
    ):
        raise ValueError("validation row commit is not an exact lowercase SHA")
    if not isinstance(checkout_value, str) or not checkout_value:
        raise ValueError("validation row has no checkout cwd")
    checkout = Path(checkout_value).resolve()
    if not checkout.is_dir():
        raise ValueError(f"validation row checkout does not exist: {checkout}")
    git_env = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    git_env["GIT_CONFIG_NOSYSTEM"] = "1"
    git_env["GIT_CONFIG_GLOBAL"] = os.devnull
    try:
        resolved = subprocess.run(
            ["git", "--no-replace-objects", "-C", str(checkout), "cat-file", "-t", commit],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
            env=git_env,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(f"cannot resolve validation row commit in row.cwd: {error}") from error
    if resolved.returncode != 0 or resolved.stdout.strip() != "commit":
        raise ValueError("validation row commit is not a commit object in row.cwd")
    return checkout


def _artifact_path(root: Path, value: object) -> Path:
    if not isinstance(value, str) or not value or Path(value).is_absolute():
        raise ValueError("cell_results artifact path must be parent-root-relative")
    retained_root = (root / "ignored" / "validate" / "artifacts").resolve()
    artifact = (root / value).resolve()
    try:
        artifact.relative_to(retained_root)
    except ValueError as error:
        raise ValueError(
            "cell_results artifact is not in retained ignored/validate/artifacts"
        ) from error
    if not artifact.is_file():
        raise ValueError("cell_results artifact is not a retained regular file")
    return artifact


def _artifact_cell(record: dict) -> dict:
    identity_keys = ("lane", "category", "test", "mode", "backend")
    expected = {"run_id", "hermit_sha", "source_tree_dirty", *identity_keys, "cell_verdict"}
    if set(record) != expected:
        raise ValueError("cell_results artifact row has missing or unknown keys")
    return {**{key: record[key] for key in identity_keys}, "cell_verdict": record["cell_verdict"]}


def _require_cell_results(row: dict, root: Path) -> None:
    """Bind schema-6 summary evidence to its retained per-cell artifact."""
    predicate = qualifying_receipt.active()
    verdict = qualifying_receipt.cell_results_verdict(row, predicate)
    if verdict is qualifying_receipt.CellResultsVerdict.GRANDFATHERED_UNKNOWN:
        return
    if verdict is not qualifying_receipt.CellResultsVerdict.SATISFIED:
        raise ValueError(f"cell_results {verdict.value}")

    evidence = row["cell_results"]
    artifact_spec = evidence["artifact"]
    artifact = _artifact_path(root, artifact_spec["path"])
    raw = artifact.read_bytes()
    if hashlib.sha256(raw).hexdigest() != artifact_spec["sha256"]:
        raise ValueError("cell_results artifact sha256 mismatch")
    if not raw or not raw.endswith(b"\n"):
        raise ValueError("cell_results artifact is empty or has a truncated final line")
    records: list[dict] = []
    for number, line in enumerate(raw.splitlines(), 1):
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError(f"cell_results artifact line {number} is malformed") from error
        if not isinstance(value, dict):
            raise ValueError(f"cell_results artifact line {number} is not an object")
        if value.get("run_id") != evidence["run_id"]:
            raise ValueError("cell_results artifact run_id mismatch")
        if value.get("hermit_sha") != row["commit"]:
            raise ValueError("cell_results artifact hermit_sha mismatch")
        if value.get("source_tree_dirty") is not False:
            raise ValueError("cell_results artifact source_tree_dirty is not false")
        records.append(_artifact_cell(value))
    if len(records) != artifact_spec["row_count"]:
        raise ValueError("cell_results artifact row_count mismatch")
    sort_key = lambda cell: tuple(
        cell[key] for key in ("lane", "category", "test", "mode", "backend")
    )
    if sorted(records, key=sort_key) != sorted(evidence["cells"], key=sort_key):
        raise ValueError("cell_results artifact cells differ from receipt summary")


def _is_failed_gate(gate: object) -> bool:
    return (
        isinstance(gate, dict)
        and gate.get("aborted") is not True
        and str(gate.get("result") or gate.get("kind") or "")
        in {"fail", "failed", "timeout"}
    )


def _require_gate_failure_bindings(row: dict) -> None:
    """Reject contradictory or under-specified producer failure attribution.

    Missing is preserved as unreported evidence. An explicit empty list is a
    different assertion: the named outer gate itself failed and no child node
    did. A lane-carried failure must instead name at least one exact child.
    Explicit null is never normalized into either state.
    """
    gates = row.get("gates")
    if not isinstance(gates, list):
        return
    for index, gate in enumerate(gates):
        if not isinstance(gate, dict):
            continue
        origin = gate.get("failure_origin")
        has_substeps = "failed_substeps" in gate
        substeps = gate.get("failed_substeps")
        failed = _is_failed_gate(gate)
        valid = origin is None and not has_substeps
        valid = valid or (
            origin == "outer_gate" and failed and has_substeps and substeps == []
        )
        valid = valid or (
            origin == "lane_substep"
            and failed
            and isinstance(substeps, list)
            and bool(substeps)
            and all(isinstance(name, str) and bool(name) for name in substeps)
        )
        if valid:
            continue
        name = str(gate.get("name") or f"index {index}")
        raise ValueError(
            f"gate {name!r} has inconsistent failure_origin/failed_substeps"
        )


def _enrich_failure_classification(row: dict, root: Path) -> None:
    """Bind a completed red row to the failure facts in its own durable log.

    ``validate-status`` intentionally refuses a bare red: it needs an observed
    failure origin, the measured flake classification, and the solo-rerun
    relationship.  The Rust validation driver already supplies the exact log,
    commit, scheduler width, and peer count, but historically the canonical
    ledger adapter journaled those values without running the classifier.  The
    resulting row could only request the same solo rerun forever.

    Missing or unreadable inputs remain missing.  In particular, this helper
    never manufactures ``known_flaky_failure=false`` from an absent log; such a
    row continues to receive NEEDS-RERUN.  Existing producer-supplied evidence
    is preserved.
    """
    if str(row.get("result") or "") not in {"fail", "failed", "timeout"}:
        return
    gates = row.get("gates")
    if not isinstance(gates, list):
        return
    failed_gates = [gate for gate in gates if _is_failed_gate(gate)]
    if not failed_gates:
        return

    validate_dir = Path(__file__).resolve().parents[1] / "validate"
    if str(validate_dir) not in sys.path:
        sys.path.insert(0, str(validate_dir))
    import failure_evidence  # noqa: PLC0415

    has_cell_results, exact_failed, _ = failure_evidence.failed_cells(
        row.get("cell_results")
    )
    if has_cell_results and "failed_cells" in row and row["failed_cells"] != exact_failed:
        raise ValueError("failed_cells differ from the validated cell_results summary")
    if has_cell_results:
        # This field comes from the already-validated schema-6 summary, not the
        # diagnostic log. Preserve it even if the log-side classification below
        # is unavailable.
        row["failed_cells"] = exact_failed

    log_file = row.get("log_file")
    dag_jobs = row.get("dag_jobs")
    concurrent = row.get("concurrent_validates")
    typed_failure_classes = failure_evidence.gate_failure_classes_from_row(row)
    readable_log = (
        isinstance(log_file, str) and bool(log_file) and Path(log_file).is_file()
    )
    if (
        not isinstance(dag_jobs, int)
        or isinstance(dag_jobs, bool)
        or (
            concurrent is not None
            and (not isinstance(concurrent, int) or isinstance(concurrent, bool))
        )
        or (typed_failure_classes is None and not readable_log)
    ):
        return

    try:
        log_text = ""
        if readable_log:
            assert isinstance(log_file, str)
            log_text = Path(log_file).read_text(errors="replace")
        evidence = failure_evidence.build_evidence(
            log_text=log_text,
            registry=failure_evidence.flaky_cells(
                failure_evidence.DEFAULT_REGISTRY,
                str(row.get("commit") or ""),
            ),
            prior=rows(root=root),
            commit=str(row.get("commit") or ""),
            dag_jobs=dag_jobs,
            concurrent_validates=concurrent,
            cell_results=row.get("cell_results"),
            failed_nodes=(
                sorted(typed_failure_classes)
                if typed_failure_classes is not None
                else None
            ),
            failure_classes=typed_failure_classes,
        )
    except (OSError, ValueError, json.JSONDecodeError):
        return

    failed_nodes = {
        str(node) for node in evidence.get("failed_substeps", []) if node
    }
    if not failed_nodes:
        return
    if (
        has_cell_results
        and "flaky_failed_cells" in row
        and row["flaky_failed_cells"] != evidence.get("flaky_failed_cells")
    ):
        raise ValueError("flaky_failed_cells differ from the exact-tree registry")
    for key in (
        "failed_substeps",
        "failed_substep_classes",
        "failed_cells",
        "first_error_line",
        "flaky_failed_substeps",
        "flaky_failed_cells",
        "known_flaky_failure",
        "solo_rerun_confirmation",
        "solo_rerun_of",
    ):
        if key in evidence and row.get(key) is None:
            row[key] = evidence.get(key)

    unmatched = failed_nodes.difference(
        str(gate.get("name") or "") for gate in failed_gates
    )
    for gate in failed_gates:
        if gate.get("failure_origin") is not None:
            continue
        name = str(gate.get("name") or "")
        if name in failed_nodes:
            gate["failure_origin"] = "outer_gate"
            gate.setdefault("failed_substeps", [])
        elif len(failed_gates) == 1 and unmatched:
            gate["failure_origin"] = "lane_substep"
            gate["failed_substeps"] = sorted(unmatched)


def _event_for_row(row: dict, root: Path) -> dict:
    record_id = str(row.get("record_id") or "")
    if not record_id:
        body = json.dumps(row, sort_keys=True, separators=(",", ":")).encode()
        record_id = "legacy-row-" + hashlib.sha256(body).hexdigest()
    schema = row.get("schema_version")
    if isinstance(schema, int) and not isinstance(schema, bool) and schema >= 6:
        run_id = row.get("run_id")
        if not isinstance(run_id, str) or not run_id.strip():
            raise ValueError("schema-6 validation row has no stable run_id")
    else:
        # Historical rows predate the stable run identity. Preserve their
        # existing one-event/one-run projection without manufacturing history.
        run_id = record_id
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
        "run_id": run_id,
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
    if row.get("run_number") is not None:
        event["run_number"] = row["run_number"]
    rel = f"ledger/{team}/{host}/{emitted_at[:7]}.jsonl"
    violations = validate_event(event, rel)
    if violations:
        raise ValueError(f"validation event violates {','.join(violations)}")
    return event


def record(row: dict, *, root: Path | None = None) -> dict:
    """Append one observed row before any Git operation can block or tear it."""
    production_root = repository_root().resolve()
    root = (root or production_root).resolve()
    row = copy.deepcopy(row)
    existing_run_number = row.get("run_number")
    if existing_run_number is not None and (
        not isinstance(existing_run_number, int)
        or isinstance(existing_run_number, bool)
        or existing_run_number <= 0
    ):
        raise ValueError("row run_number must be a positive integer")
    environment_run_number = os.environ.get(RUN_NUMBER_ENV)
    if environment_run_number is not None:
        try:
            run_number = int(environment_run_number)
        except ValueError as error:
            raise ValueError(f"{RUN_NUMBER_ENV} must be a positive integer") from error
        if run_number <= 0 or str(run_number) != environment_run_number:
            raise ValueError(f"{RUN_NUMBER_ENV} must be a positive integer")
        if existing_run_number is not None and existing_run_number != run_number:
            raise ValueError(
                f"row run_number {existing_run_number!r} differs from admitted "
                f"run number {run_number}"
            )
        row["run_number"] = run_number
    if root == production_root and _is_acceptance_fixture_row(row):
        raise ValueError(
            "acceptance fixture producer is not allowed in the production ledger"
        )
    _require_local_commit(row)
    _require_cell_results(row, root)
    _enrich_failure_classification(row, root)
    _require_gate_failure_bindings(row)
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


def _legacy_row_corrections(
    items: list[dict],
) -> tuple[dict[str, tuple[str, dict]], set[str]]:
    """Validated ``run.correct`` replacements for the compatibility view.

    The event union and timeline remain append-only.  Only ``rows()`` substitutes
    a corrected ``legacy_row`` at the superseded event's original position.  A
    whole-ledger ``fold()`` cannot be used here: enrichment chains deliberately
    collapse by ``run_id``, while this compatibility view historically exposes
    each distinct legacy receipt.
    """
    by_id = {
        str(event.get("event_id")): event
        for event in items
        if isinstance(event.get("event_id"), str) and event.get("event_id")
    }
    known_ids = set(by_id)
    replacements: dict[str, tuple[str, dict]] = {}
    correction_ids: set[str] = set()

    for event in items:
        if event.get("event_type") != "run.correct" or "legacy_row" not in event:
            continue
        event_id = event.get("event_id")

        def refuse(reason: str) -> None:
            raise ValueError(
                f"HistoryRow correction {event_id!r} is invalid: {reason}"
            )

        if not isinstance(event_id, str) or not event_id:
            refuse("event_id is missing")
        violations = validate_event(event, known_ids=known_ids)
        if violations:
            refuse(f"event violates {','.join(violations)}")
        supersedes = event.get("supersedes")
        if not isinstance(supersedes, str) or not supersedes:
            refuse("supersedes is missing")
        target = by_id.get(supersedes)
        if target is None:
            refuse(f"supersedes unknown event {supersedes!r}")
        reason = event.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            refuse("reason is missing")
        for field in ("run_id", "team", "host"):
            if event.get(field) != target.get(field):
                refuse(f"{field} differs from superseded event")
        corrected = event.get("legacy_row")
        if not isinstance(corrected, dict):
            refuse("legacy_row is not an object")
        if not isinstance(target.get("legacy_row"), dict):
            refuse("superseded event has no legacy_row")
        try:
            _require_gate_failure_bindings(corrected)
        except ValueError as error:
            refuse(str(error))
        if supersedes in replacements:
            refuse(f"a second correction also supersedes {supersedes!r}")
        replacements[supersedes] = (event_id, corrected)
        correction_ids.add(event_id)

    return replacements, correction_ids


def current_validation_run_rows(items: Iterable[dict]) -> list[dict]:
    """Project valid ``corrects`` chains to one current row per stable run.

    This is the Python counterpart of ``ci-hub``'s validate-status run view.
    Finalization keeps the producer row append-only and appends a replacement
    with the same ``run_id``.  Only a complete, unambiguous chain is collapsed;
    malformed or ambiguous groups stay visible so a reader cannot guess away
    evidence.
    """
    rows = list(items)
    groups: dict[str, list[int]] = {}
    for index, row in enumerate(rows):
        run_id = row.get("run_id")
        if isinstance(run_id, str) and run_id.strip():
            groups.setdefault(run_id, []).append(index)

    superseded: set[int] = set()
    for indices in (group for group in groups.values() if len(group) > 1):
        records: dict[str, int] = {}
        valid = True
        for index in indices:
            record_id = rows[index].get("record_id")
            if not isinstance(record_id, str) or not record_id.strip():
                valid = False
                break
            if record_id in records:
                valid = False
                break
            records[record_id] = index
        if not valid:
            continue

        group_superseded: set[int] = set()
        for index in indices:
            corrects = rows[index].get("corrects")
            if not isinstance(corrects, str):
                continue
            corrected_index = records.get(corrects)
            if (
                corrected_index is None
                or corrected_index == index
                or corrected_index in group_superseded
            ):
                valid = False
                break
            group_superseded.add(corrected_index)
        if not valid:
            continue

        terminals = [index for index in indices if index not in group_superseded]
        if len(terminals) != 1:
            continue

        chain: set[int] = set()
        current = terminals[0]
        while True:
            if current in chain:
                valid = False
                break
            chain.add(current)
            corrects = rows[current].get("corrects")
            if not isinstance(corrects, str):
                break
            next_index = records.get(corrects)
            if next_index is None:
                valid = False
                break
            current = next_index
        if valid and len(chain) == len(indices):
            superseded.update(group_superseded)

    return [row for index, row in enumerate(rows) if index not in superseded]


def rows(*, root: Path | None = None) -> list[dict]:
    """HistoryRow compatibility view of the one canonical event union."""
    root = (root or repository_root()).resolve()
    items = events(root=root)
    for index, codes in enumerate(validate_events(items), 1):
        if codes:
            event = items[index - 1]
            if event.get("event_type") == "run.correct":
                raise ValueError(
                    f"HistoryRow correction {event.get('event_id')!r} is invalid: "
                    f"event violates {codes}"
                )
            raise ValueError(f"validation event {index} violates {codes}")
    replacements, correction_ids = _legacy_row_corrections(items)
    out: list[dict] = []
    seen: set[str] = set()
    for event in items:
        event_id = str(event.get("event_id") or "")
        if event_id in correction_ids:
            continue
        row = event.get("legacy_row")
        if isinstance(row, dict):
            followed: set[str] = set()
            current = event_id
            while current in replacements:
                if current in followed:
                    raise ValueError(
                        f"HistoryRow correction cycle starts at {event_id!r}"
                    )
                followed.add(current)
                current, row = replacements[current]
            restored = _replace(row, WORKSPACE_TOKEN, str(root))
            identity = json.dumps(restored, sort_keys=True, separators=(",", ":"))
            if identity not in seen:
                seen.add(identity)
                out.append(restored)
    return out


def emit_jsonl(items: Iterable[dict]) -> None:
    for item in items:
        print(json.dumps(item, sort_keys=True, separators=(",", ":")))


# ⚠️ A FAILURE COUNT WITHOUT ITS DENOMINATOR IS NOT A MEASUREMENT, and on this
# project it has already been reported as one three times in a night.
#
#     752cbc3daef7   63 gates   9 failures
#     4a4830af353f   67 gates   6 failures
#     c0914b5d5e11   66 gates   9 failures
#
# Read as 9 -> 6 -> 9 those numbers describe almost nothing. Between the first
# two, COVERAGE ROSE BY FOUR GATES while the count fell -- the fall was partly
# work landing and partly nodes that had never been running starting to run.
# Between the last two, coverage FELL BY ONE while the count rose. A red set that
# shrinks because a gate STOPPED RUNNING reads identically, on the count alone, to
# one that shrinks because a defect was fixed -- and the first is the opposite of
# progress.
#
# ⚠️ AND THE NON-FAILURES ARE NOT ONE BUCKET EITHER. Folding them together is the
# same error one level down, because they need different actions:
#
#   dependency-skipped  ran nothing because an UPSTREAM node failed. Fixing the
#                       upstream restores it; there is nothing to fix here.
#   never-ran           planned and absent. May never have run at all -- measured
#                       on `test.cli_kvm`, absent from EVERY full receipt in the
#                       retained history. This is missing coverage, not a red.
#   no-outcome          planned, and the scheduler returned neither a result nor
#                       a skip. THE THIRD STATE, and the only one that indicates
#                       the instrument itself failed to account for a node.
#   intentional-skip    excluded on purpose.
#
# So this function returns them as four separate keys and the renderer prints them
# on four separate lines. There is deliberately no total.
def summarise(row: dict) -> dict:
    """Coverage-aware breakdown of one receipt. No conflated total, by design."""
    coverage = row.get("coverage") or {}

    def names(*keys, source=row):
        for key in keys:
            value = source.get(key)
            if value:
                return sorted(value)
        return []

    return {
        "sha": row.get("commit"),
        "finished_at": row.get("finished_at"),
        "profile": row.get("profile"),
        "selection_mode": row.get("selection_mode"),
        "result": row.get("result"),
        # THE DENOMINATOR. Every consumer of `failing` must carry this with it.
        "gates_run": row.get("gates_run"),
        "failing": names("failed_substeps"),
        "dependency_skipped": names("dependency_skipped_nodes"),
        "never_ran": names("absent_nodes", source=coverage),
        "no_outcome": names("unaccounted_nodes"),
        "intentional_skipped": names("intentional_skipped_nodes"),
        "timed_out": names("timed_out_nodes"),
    }


def render_summary(s: dict) -> str:
    """One headline that CANNOT state a failure count without its denominator."""
    lines = [
        f"sha      {s['sha']}",
        f"receipt  {s['finished_at']}  profile={s['profile']}  "
        f"selection={s['selection_mode']}  result={s['result']}",
        "",
        f"{len(s['failing'])} FAILING of {s['gates_run']} gates run",
    ]
    lines += [f"    {n}" for n in s["failing"]]
    # Counted separately, never folded into the failing count: each wants a
    # different action, and two of them are absences rather than failures.
    for key, label, gloss in (
        ("dependency_skipped", "dependency-skipped",
         "ran nothing; an upstream node failed"),
        ("never_ran", "never-ran", "planned and absent -- missing coverage, not a red"),
        ("no_outcome", "NO OUTCOME",
         "planned; scheduler returned neither a result nor a skip -- THE THIRD STATE"),
        ("intentional_skipped", "intentional-skip", "excluded on purpose"),
        ("timed_out", "timed-out", "killed by its own bound"),
    ):
        entries = s[key]
        if not entries and key not in ("dependency_skipped", "never_ran", "no_outcome"):
            continue
        lines.append(f"{len(entries)} {label}  ({gloss})")
        lines += [f"    {n}" for n in entries]
    return "\n".join(lines)


def latest_row(repo: str, commit: str | None, profile: str | None) -> dict | None:
    """The newest matching receipt, or None. `commit` may be abbreviated."""
    best = None
    for row in rows():
        if repo and row.get("repo") != repo:
            continue
        if not row.get("finished_at"):
            continue
        if commit and not str(row.get("commit", "")).startswith(commit):
            continue
        if profile and row.get("profile") != profile:
            continue
        if best is None or row["finished_at"] > best["finished_at"]:
            best = row
    return best


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("rows", description="emit the canonical HistoryRow compatibility view", help="emit the canonical HistoryRow compatibility view")
    commands.add_parser("events", description="emit the canonical event union", help="emit the canonical event union")
    commands.add_parser("record", description="read one HistoryRow JSON object from stdin and journal it", help="read one HistoryRow JSON object from stdin and journal it")
    summary = commands.add_parser(
        "summary",
        description="coverage-aware breakdown of one receipt: gates_run beside the "
        "failure count, with dependency-skipped, never-ran and no-outcome nodes "
        "counted SEPARATELY rather than folded in",
        help="failures BESIDE gates_run, with skips and absences counted separately",
    )
    summary.add_argument("--repo", default="hermit")
    summary.add_argument("--commit", default=None,
                         help="full or abbreviated sha; default is the newest receipt")
    summary.add_argument("--profile", default="full",
                         help="'full' by default; pass '' for any profile")
    summary.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.command == "summary":
        row = latest_row(args.repo, args.commit, args.profile or None)
        if row is None:
            # ⚠️ AN ABSENT RECEIPT IS NOT A GREEN ONE. Exit 2, never 0 with an
            # empty summary: a caller that reads rc=0 and no failing names would
            # conclude the tree is clean when nothing was ever measured.
            where = f"{args.repo}@{args.commit or 'latest'}"
            profile = args.profile or "any"
            print(f"validate-ledger: NO RECEIPT for {where} (profile={profile}) -- "
                  "nothing was measured, which is not the same as nothing failing",
                  file=sys.stderr)
            return 2
        s = summarise(row)
        print(json.dumps(s, sort_keys=True) if args.json else render_summary(s))
        return 0
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
