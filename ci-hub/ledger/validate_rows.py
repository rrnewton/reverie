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
import fcntl
import json
import os
import hashlib
import re
import socket
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from datetime import datetime
from typing import Iterable

from ledger import LedgerError, parse_line, read_shard, union_events, validate_event, validate_events
from publisher import retained, spool, spooled

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualifying_receipt  # noqa: E402
from admission_paths import AdmissionPaths, read_admission_log  # noqa: E402

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
BRANCH_ENV = "CI_HUB_VALIDATE_BRANCH"

# The initial legacy migration treated one mixed HistoryRow file as a Hermit
# shard.  Nineteen schema-3 Reverie rows therefore have a Hermit envelope in
# both the primary shard and its immutable recovery copy.  Keep this exception
# closed over the exact canonical event content already in history: producer
# attributes, result, timestamps, paths, and every nested row field are all
# covered by the digest.
# A producer name, id prefix, date range, or commit alone would also admit a new
# mismatched event and would weaken the source-binding check this compatibility
# exists alongside.
HISTORICAL_REPOSITORY_MISMATCH_SHARDS = (
    Path("ledger/hermit/devbig014/2026-08.jsonl"),
    Path(
        "ledger-recovery/hermit/devbig014/2026-08/"
        "4b681068b432ff230051fd7e105a60c0ae849a1b.jsonl"
    ),
)
HISTORICAL_REPOSITORY_MISMATCHES = {
    "legacy-000246-000": (
        "088fc5d216bd38e58939c04afa10f97865e26b06",
        "d1354fa83910e8af1a767d45e619a5cf9f5d7959339340e1032af2fa3a65a44b",
    ),
    "legacy-000256-000": (
        "f9f11510dfd499282f8cef581d07dd2c0882bfc0",
        "34cc95fd55df112d4737ed3d7b0ff8e81957ef9ed497a3afdf94b8bc587d9b6e",
    ),
    "legacy-000288-000": (
        "74f8ecee97a1f73af831c59ae15519b5e18af373",
        "68ef4b86e7ad1c8d0b11e9cdc962895423023a9c46b13ac1476fa4408f7281d5",
    ),
    "legacy-000289-000": (
        "6aa371ea9de07454f63a2fd425c1dd4a040f9a80",
        "0c9fc801c865e4647e73ba4380caca1e91f2b1fb4b302a296498fcbf3c5a7b48",
    ),
    "legacy-000290-000": (
        "d4dbcd4377e4a32d2fa65969fee5ceed25db9603",
        "11b262d53fc68d4eb607e7ac73c4f9380c2cb44c0864eacf6de0e233c8215b24",
    ),
    "legacy-000291-000": (
        "f1aa47e1a268e51127dc5209acba05c97c8d32ea",
        "c1b4827beb65f3f4a1f3cc91d2ea5a3f66c84c5931ce525a322711728589b8b5",
    ),
    "legacy-000369-000": (
        "975c9fa8c432fefb6c7ae88b27ef97685fd94f50",
        "ea40271c3858bbea2ddbb22f1819d55858e1d1c36cbcfbe14b837585ae8480fc",
    ),
    "legacy-000403-000": (
        "fb7672aacdd3d13db64be16fe55c3e5f8157b3a1",
        "7a88195ec6510475d9cb3dcfef89eaa1696d3f27ade0b14382eb1578fac202ce",
    ),
    "legacy-000419-000": (
        "bff3cd32100fac09114d8048721986a80dda5db2",
        "1f18c1a0b360ea9f31241c2da44e8154cf53c208acbb59a922c3b85fbb2b9c1a",
    ),
    "legacy-000420-000": (
        "156c623994d9bf4fa98978b712febb56d3a2176f",
        "17d621c26ac667159757aed762fb5de8f5f6dfa06d15674f3910a3ba02346606",
    ),
    "legacy-000421-000": (
        "a14934271a780c9ab352fdd65c1f183a63d6564a",
        "adb314a37e3ba415fedfe173af28d2e95b60a40e31db199a78c6e9e8b4a29ba4",
    ),
    "legacy-000422-000": (
        "a14934271a780c9ab352fdd65c1f183a63d6564a",
        "ca51a2025840a5d6ad8ca2f760c90e5710c3fd152187bf3ede4dbec1991ed7d1",
    ),
    "legacy-000425-000": (
        "22781c51a9caa065218dbadcdca0b03d34abbbe7",
        "d5c128e417ee984ae94edc371c32fb215a02755e996a64506c07c633ec3b378e",
    ),
    "legacy-000449-000": (
        "8fa44eda6f751820b86e5e2a671b211bd2525eee",
        "4156fac41dce0439b159ef98dbab3d404e482ab13aa8efc733c46aeff0e53493",
    ),
    "legacy-000541-000": (
        "ad1b845c5d0a454d51c5d4d44f3bb655ab59241c",
        "8b1a68875969f7ae99b8e44a666dc1d9e62462fddeb48850f8ebb49fa220a048",
    ),
    "legacy-000542-000": (
        "e33d3b742faf49087ee3845bca9836ce1466ae08",
        "5cd0aae23562ba6d846e283ca30fcf4fa9b84df0f354be2a01f9a829afc9b7b3",
    ),
    "legacy-000543-000": (
        "bf8ee2dd8ebcf61c18e742c943851823bc4904f4",
        "44c0a59eeaea76360055d79433414e226c01effd13352cf72fb1af6cb6a50df6",
    ),
    "legacy-000608-000": (
        "5d6242fc2b6af5f8e841ae3badccf939c66e5327",
        "3c0eb81afa43604755ae6c8d75f8a459e73bbd11182d4ec49bf8fa1099fe88de",
    ),
    "legacy-000609-000": (
        "5d6242fc2b6af5f8e841ae3badccf939c66e5327",
        "3ee5d5a8780426dbbd2d9799a19be468a5c087fa4a31b5e4f15abeeb2bd5437b",
    ),
}


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


def _canonical_event_sha256(event: dict) -> str:
    body = json.dumps(event, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(body).hexdigest()


def _historical_repository_mismatch_digest(event: dict) -> str | None:
    """Return the digest only for one exact, known mixed-corpus import event."""
    event_id = event.get("event_id")
    if not isinstance(event_id, str):
        return None
    expected = HISTORICAL_REPOSITORY_MISMATCHES.get(event_id)
    if expected is None:
        return None
    expected_commit, expected_digest = expected
    producer = event.get("producer")
    row = event.get("legacy_row")
    if not (
        event.get("schema") == "validate-ledger/v1"
        and event.get("event_type") == "run.result"
        and event.get("team") == "hermit"
        and event.get("host") == "devbig014"
        and producer
        == {
            "source": "imported",
            "tool": "ci-hub/ledger/migrate_legacy",
            "tool_version": "1",
        }
        and isinstance(row, dict)
        and row.get("schema_version") == 3
        and row.get("repo") == "reverie"
        and event.get("commit") == expected_commit
        and row.get("commit") == expected_commit
    ):
        return None
    digest = _canonical_event_sha256(event)
    return digest if digest == expected_digest else None


def _git_environment() -> dict[str, str]:
    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    return environment


def _tracked_historical_shard(
    root: Path, relative: Path, *, commit: str = "HEAD", snapshot_root: Path | None = None,
) -> list[dict] | None:
    """Read a named Git blob, checking any captured copy byte for byte."""
    current_root = snapshot_root if snapshot_root is not None else root
    path = current_root / relative
    component = current_root
    for part in relative.parts:
        component /= part
        if component.is_symlink():
            return None
    try:
        path.resolve().relative_to(current_root.resolve())
    except ValueError:
        return None
    if not path.is_file():
        return None
    arguments = ["git", "--no-replace-objects", "-C", str(root)]
    environment = _git_environment()
    try:
        top = subprocess.run(
            [*arguments, "rev-parse", "--path-format=absolute", "--show-toplevel"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
            env=environment,
        )
        if top.returncode != 0 or Path(top.stdout.strip()).resolve() != root.resolve():
            return None
        entry = subprocess.run(
            [*arguments, "ls-tree", "-z", f"{commit}^{{commit}}", "--", relative.as_posix()],
            check=False,
            capture_output=True,
            timeout=10,
            env=environment,
        )
        expected_suffix = b"\t" + os.fsencode(relative.as_posix()) + b"\0"
        if (
            entry.returncode != 0
            or not entry.stdout.startswith(b"100644 blob ")
            or not entry.stdout.endswith(expected_suffix)
            or entry.stdout.count(b"\0") != 1
        ):
            return None
        contents = subprocess.run(
            [*arguments, "show", f"{commit}:{relative.as_posix()}"],
            check=False,
            capture_output=True,
            timeout=10,
            env=environment,
        )
    except (OSError, subprocess.TimeoutExpired, UnicodeError):
        return None
    if contents.returncode != 0:
        return None
    try:
        if snapshot_root is not None and path.read_bytes() != contents.stdout:
            return None
        return [json.loads(line) for line in contents.stdout.splitlines() if line]
    except (OSError, UnicodeError, json.JSONDecodeError, TypeError):
        return None


def _historical_repository_mismatch_source_digests(
    root: Path, *, committed_source: tuple[Path, str] | None = None,
) -> frozenset[str]:
    """Require the exact exception in both current and Git-bound named shards.

    Ordinary readers bind to their worktree HEAD. An immutable export instead
    supplies its original repository and commit; its non-Git snapshot must
    contain byte-identical copies of both committed shards.
    """
    if committed_source is not None:
        source_root, source_commit = committed_source
        if not isinstance(source_commit, str) or re.fullmatch(r"[0-9a-f]{40}", source_commit) is None:
            raise ValueError("captured validation source requires an immutable commit SHA")
    copies: list[dict[str, str]] = []
    for relative in HISTORICAL_REPOSITORY_MISMATCH_SHARDS:
        path = root / relative
        tracked_events = (
            _tracked_historical_shard(root, relative)
            if committed_source is None else
            _tracked_historical_shard(source_root, relative, commit=source_commit, snapshot_root=root)
        )
        if tracked_events is None:
            return frozenset()
        for events_in_source in (read_shard(path), tracked_events):
            source: dict[str, str] = {}
            for event in events_in_source:
                digest = _historical_repository_mismatch_digest(event)
                if digest is not None:
                    source[str(event["event_id"])] = digest
            copies.append(source)

    admitted = {
        digest
        for event_id, (_commit, digest) in HISTORICAL_REPOSITORY_MISMATCHES.items()
        if all(source.get(event_id) == digest for source in copies)
    }
    return frozenset(admitted)


def repository_root() -> Path:
    if os.environ.get("HERMIT_VALIDATE_STOP_TEST_MODE") == "1":
        test_root = os.environ.get("CI_HUB_VALIDATE_LEDGER_TEST_ROOT")
        if test_root:
            return Path(test_root)
    # An immutable operational-tool checkout supplies this module's code while
    # the append-only ledger and live spool remain in the mutable parent. Keep
    # that split explicit; deriving state from __file__ would read the private
    # tool snapshot and make existing validation evidence disappear.
    parent = os.environ.get("DEV_HERMIT_PARENT")
    if parent:
        root = Path(parent)
        if not root.is_absolute() or not root.is_dir():
            raise ValueError(
                "DEV_HERMIT_PARENT must name an existing absolute directory: "
                f"{root}"
            )
        return root.resolve()
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
    if isinstance(value, qualifying_receipt.AdmissionJsonObject):
        return qualifying_receipt.AdmissionJsonObject(
            (key, _replace(item, old, new)) for key, item in value.pairs
        )
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


def _legacy_row_repo(
    event: dict,
    *,
    historical_source_digests: frozenset[str] = frozenset(),
) -> str | None:
    """Validate and resolve repository identity embedded in a record event.

    ``team`` is the path-safe outer identity while ``legacy_row.repo`` is the
    repository identity consumed by the HistoryRow and raw-record views. They
    must describe the same repository. The only missing-repository
    compatibility is the measured historical Hermit population, whose rows
    predate ``repo``; Reverie has no such historical population.

    A result without an embedded compatibility row remains a valid generic
    ledger event. A correction without an object row is rejected separately by
    the correction projector.
    """
    event_id = event.get("event_id")
    if event_id in HISTORICAL_REPOSITORY_MISMATCHES:
        digest = _historical_repository_mismatch_digest(event)
        if digest is not None and digest in historical_source_digests:
            return "reverie"
        raise ValueError(
            f"historical repository-mismatch event {event_id!r} differs from "
            "its immutable source record"
        )

    if event.get("event_type") not in {"run.result", "run.enrich", "run.correct"}:
        return None
    row = event.get("legacy_row")
    if not isinstance(row, dict):
        return None

    team = str(event.get("team") or "")
    repo = row.get("repo")
    if repo is None:
        if team == "hermit":
            return "hermit"
        raise ValueError(
            "legacy_row.repo is absent or null outside the historical Hermit "
            "compatibility population"
        )
    if not isinstance(repo, str) or not repo.strip():
        raise ValueError("legacy_row.repo is not a non-empty string")
    row_team = _ledger_team(repo)
    if row_team != team:
        raise ValueError(
            f"event team {team!r} conflicts with legacy_row.repo {repo!r} "
            f"(maps to team {row_team!r})"
        )
    return repo


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
    """Bind supported summary evidence to its retained per-cell artifact."""
    predicate = qualifying_receipt.active()
    verdict = qualifying_receipt.cell_results_verdict(row, predicate)
    if verdict is qualifying_receipt.CellResultsVerdict.GRANDFATHERED_UNKNOWN:
        return
    if verdict is qualifying_receipt.CellResultsVerdict.READER_FAULT:
        raise qualifying_receipt.ValidationEvidenceReaderFault(
            qualifying_receipt.validation_evidence(row).reason
        )
    if verdict is qualifying_receipt.CellResultsVerdict.ARTIFACT_REFUSED:
        raise ValueError(f"cell_results {qualifying_receipt.validation_evidence(row).reason}")
    if verdict not in {
        qualifying_receipt.CellResultsVerdict.SATISFIED,
        qualifying_receipt.CellResultsVerdict.LEGACY_UNBOUND,
    }:
        raise ValueError(f"cell_results {verdict.value}")
    if row.get("schema_version") == 10:
        # The shared verifier has read and bound all three artifacts, including
        # the full attempt records that intentionally are not copied into the
        # compact ledger summary. Legacy-unbound is readable historical evidence,
        # not a qualifying bound receipt. Preserve its failures and observations;
        # never compare that artifact using V8 keys.
        return

    evidence = row["cell_results"]
    artifact_spec = evidence["artifact"]
    artifact = _artifact_path(root, artifact_spec["path"])
    raw = artifact.read_bytes()
    if hashlib.sha256(raw).hexdigest() != artifact_spec["sha256"]:
        raise ValueError("cell_results artifact sha256 mismatch")
    if raw and not raw.endswith(b"\n"):
        raise ValueError("cell_results artifact has a truncated final line")
    if not raw and artifact_spec["row_count"] != 0:
        raise ValueError("cell_results artifact is empty but row_count is nonzero")
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


def _bind_comparison_failures(row: dict, observations: list[dict]) -> None:
    """Persist shared comparison attribution without changing its relation."""
    failures = [
        observation for observation in observations
        if observation["verdict"]["state"] != "matched"
    ]
    if "failed_comparisons" in row and row["failed_comparisons"] != failures:
        raise ValueError("failed_comparisons differ from the verified artifact observations")
    row["failed_comparisons"] = failures
    ordinary_failures = sorted({
        "/".join(observation["identity"][key] for key in ("test", "mode", "backend"))
        for observation in failures
        if observation["relation"]["kind"] == "ordinary"
        and observation["verdict"]["state"] == "diverged"
    })
    if "failed_cells" in row and row["failed_cells"] != ordinary_failures:
        raise ValueError("failed_cells differ from the verified ordinary attempt history")
    row["failed_cells"] = ordinary_failures
    if row.get("known_flaky_failure") is True and any(
        observation["relation"]["kind"] != "ordinary" for observation in failures
    ):
        raise ValueError("ordinary flake classification cannot cover reference or backend parity failures")


def _enrich_failure_classification(
    row: dict, root: Path, *, comparison_observations: list[dict] | None = None
) -> None:
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
        row.get("cell_results"), comparison_observations=comparison_observations
    )
    if has_cell_results and "failed_cells" in row and row["failed_cells"] != exact_failed:
        raise ValueError("failed_cells differ from the validated cell_results summary")
    if has_cell_results:
        # This field describes failures among the recorded cells in the
        # validated summary. Preserve it when log-side classification is
        # unavailable; selected cells without a result do not imply success.
        row["failed_cells"] = exact_failed

    log_file = row.get("log_file")
    dag_jobs = row.get("dag_jobs")
    concurrent = row.get("concurrent_validates")
    typed_failure_classes = failure_evidence.gate_failure_classes_from_row(row)
    readable_log = (
        isinstance(log_file, str) and bool(log_file) and Path(log_file).is_file()
    )
    portable_log_text = None
    if qualifying_receipt.admission_is_v2(row):
        try:
            portable_log_text = read_admission_log(row, root).decode("utf-8", errors="replace")
            readable_log = True
        except (OSError, ValueError):
            readable_log = False
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
            if portable_log_text is not None:
                log_text = portable_log_text
            else:
                assert isinstance(log_file, str)
                log_text = Path(log_file).read_text(errors="replace")
        evidence = failure_evidence.build_evidence(
            log_text=log_text,
            registry=failure_evidence.flaky_cells(
                failure_evidence.DEFAULT_REGISTRY,
                str(row.get("commit") or ""),
            ),
            prior=rows(root=root, preserve_admission=True),
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
            comparison_observations=comparison_observations,
            state_root=root,
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
    v2 = qualifying_receipt.admission_is_v2(row)
    immutable = None
    if v2:
        immutable = qualifying_receipt.admission_transport_identity(row)
        with AdmissionPaths(root) as paths:
            paths.require_log(row["admission_floor_evidence"]["context"], row.get("log_file"))
    record_id = str(row.get("record_id") or "")
    if not record_id:
        body = json.dumps(row, sort_keys=True, separators=(",", ":")).encode()
        record_id = "legacy-row-" + hashlib.sha256(body).hexdigest()
    schema = row.get("schema_version")
    if v2 or (isinstance(schema, int) and not isinstance(schema, bool) and schema >= 6):
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
    workspace_root = str(root) if v2 else _workspace_root_for_row(row, root)
    portable = _replace(row, workspace_root, WORKSPACE_TOKEN)
    # Failure text can mention cargo/git checkouts outside the dev-hermit tree.
    # Keep the diagnostic shape while stripping the machine owner's identity;
    # otherwise a perfectly valid failing run cannot be journaled at all.
    owner_home = _owner_home_for_workspace(workspace_root)
    if owner_home:
        portable = _replace(portable, owner_home, HOME_TOKEN)
    if v2 and qualifying_receipt.admission_transport_identity(portable) != immutable:
        raise ValueError("ledger path rendering changed immutable v2 admission evidence")
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


def _record_admission_identity(row: dict):
    if "admission_floor_evidence" not in row:
        return None
    evidence = row["admission_floor_evidence"]
    encoded = qualifying_receipt.validate_admission_floor_evidence(evidence, row)
    log_key = "log_identity" if qualifying_receipt.admission_is_v2(row) else "log_file"
    return encoded, copy.deepcopy(evidence["artifact"]), copy.deepcopy(row[log_key])


def _prepare_record_event(row: dict, root: Path) -> dict:
    """Apply the ordinary writer's evidence checks before either append path."""
    production_root = repository_root().resolve()
    # Inspect original pairs before display normalization or enrichment can
    # discard an ambiguous claim. The ordinary writer view is safe only after
    # that present claim has passed the shared verifier; absence stays legacy.
    admitted_identity = _record_admission_identity(row)
    if isinstance(row, qualifying_receipt.AdmissionJsonObject):
        row = json.loads(json.dumps(row))
    else:
        row = copy.deepcopy(row)
    existing_run_number = row.get("run_number")
    if existing_run_number is not None and (
        not isinstance(existing_run_number, int)
        or isinstance(existing_run_number, bool)
        or existing_run_number <= 0
    ):
        # This counter is machine-local display metadata, never row identity or
        # receipt authority.  A malformed optional label must not suppress the
        # producer-owned result.
        row.pop("run_number", None)
        existing_run_number = None
    environment_run_number = os.environ.get(RUN_NUMBER_ENV)
    if environment_run_number is not None:
        try:
            run_number = int(environment_run_number)
        except ValueError:
            run_number = 0
        if run_number > 0 and str(run_number) == environment_run_number:
            # The admission producer owns this optional label.  Prefer it over
            # any stale row copy without turning a display mismatch into a
            # missing validation result.
            row["run_number"] = run_number
    existing_branch = row.get("branch")
    if existing_branch is not None and (
        not isinstance(existing_branch, str)
        or not existing_branch.strip()
        or any(
            ord(character) < 32 or ord(character) == 127
            for character in existing_branch
        )
    ):
        # Branch is display provenance, never receipt authority. A malformed
        # optional label must not suppress the producer-owned result.
        row.pop("branch", None)
    admission_branch = os.environ.get(BRANCH_ENV)
    if (
        admission_branch is not None
        and admission_branch.strip()
        and not any(
            ord(character) < 32 or ord(character) == 127
            for character in admission_branch
        )
    ):
        # validate-lock supplies this from the source checkout's symbolic HEAD
        # after admission. Prefer it over any stale row copy, exactly as for the
        # admission-owned run number above.
        row["branch"] = admission_branch
    if root == production_root and _is_acceptance_fixture_row(row):
        raise ValueError(
            "acceptance fixture producer is not allowed in the production ledger"
        )
    _require_local_commit(row)
    comparison_observations = None
    with qualifying_receipt.validation_evidence_batch([row], artifact_root=root):
        _require_cell_results(row, root)
        if row.get("schema_version") == 10:
            comparison_observations = qualifying_receipt.validation_evidence(row).evidence["observations"]
    if comparison_observations is not None:
        _bind_comparison_failures(row, comparison_observations)
    if comparison_observations is None:
        _enrich_failure_classification(row, root)
    else:
        _enrich_failure_classification(row, root, comparison_observations=comparison_observations)
    _require_gate_failure_bindings(row)
    if _record_admission_identity(row) != admitted_identity:
        raise ValueError("writer normalization changed admitted evidence identity")
    return _event_for_row(row, root)


def _spool_record_event(event: dict, root: Path) -> dict:
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


def record(row: dict, *, root: Path | None = None) -> dict:
    """Append one observed row before any Git operation can block or tear it."""
    root = (root or repository_root()).resolve()
    return _spool_record_event(_prepare_record_event(row, root), root)


def reverie_source(
    target: str, checkout: str, log: str, started: str, *, root: Path | None = None
) -> dict:
    """Select the exact raw producer event, retaining its existing identity."""
    root = (root or repository_root()).resolve()
    items, _ = _validated_events(root=root, preserve_admission=True)
    # Keep the ordinary reader's refusal on ambiguous or cyclic corrections.
    _rows_from_events(items, root=root)
    boundary = datetime.fromisoformat(started.replace("Z", "+00:00"))
    candidates = []
    for event in items:
        if event.get("event_type") != "run.result":
            continue
        rows_for_event = _rows_from_events([event], root=root)
        if len(rows_for_event) != 1:
            continue
        row = rows_for_event[0]
        if (row.get("producer") != "reverie-validate-sh"
                or row.get("repo") not in {"reverie", "rrnewton/reverie"}
                or row.get("commit") != target or row.get("cwd") != checkout
                or row.get("log_file") != log):
            continue
        try:
            row_started = datetime.fromisoformat(row["started_at"].replace("Z", "+00:00"))
            within_run = row_started >= boundary
        except (KeyError, AttributeError, TypeError, ValueError):
            continue
        if within_run:
            candidates.append({"event": event, "row": row})
    if len(candidates) != 1:
        raise ValueError(f"expected exactly one Reverie source event, found {len(candidates)}")
    return candidates[0]


def record_reverie_finalization(
    source: dict, row: dict, *, root: Path | None = None
) -> dict:
    """Append finalizer provenance without inventing a second execution.

    Every original execution fact stays immutable. This is intentionally not
    a general correction API: only the existing Reverie adapter's metadata
    additions may differ from its exact, still-retained source event.
    """
    root = (root or repository_root()).resolve()
    if not isinstance(source, dict) or not isinstance(row, dict):
        raise ValueError("Reverie finalization requires source event and row objects")
    source_id = source.get("event_id")
    if not isinstance(source_id, str) or not source_id:
        raise ValueError("Reverie source event has no identity")
    guard_dir = live_spool_dir(root) / ".reverie-finalization"
    guard_dir.mkdir(parents=True, exist_ok=True)
    guard_name = hashlib.sha256(source_id.encode()).hexdigest() + ".lock"
    # Do not unlink the guard: a second inode would split concurrent writers.
    with (guard_dir / guard_name).open("a") as guard:
        try:
            fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("Reverie source finalization is already in progress") from error
        items, _ = _validated_events(root=root, preserve_admission=True)
        _rows_from_events(items, root=root)
        matches = [item for item in items if item.get("event_id") == source_id]
        if len(matches) != 1 or _admission_event_identity(matches[0]) != _admission_event_identity(source):
            raise ValueError("Reverie source event does not match the retained event")
        original = source.get("legacy_row")
        if (source.get("event_type") != "run.result"
                or source.get("team") != "reverie"
                or not isinstance(original, dict)
                or original.get("producer") != "reverie-validate-sh"
                or source.get("producer", {}).get("tool") != "reverie-validate-sh"
                or source.get("producer", {}).get("source") != "observed"
                or type(original.get("schema_version")) is not int
                or original.get("schema_version") not in {3, 4, 5}
                or original.get("repo") not in {"reverie", "rrnewton/reverie"}):
            raise ValueError("Reverie finalization source is not a raw Reverie producer result")
        event = _prepare_record_event(row, root)
        final = event["legacy_row"]
        normalized = {"schema_version": 5, "producer": "ci-hub-reverie-validate-run", "repo": "rrnewton/reverie"}
        additions = {
            "tree", "raw_result", "gates_run", "gates_expected", "coverage",
            "admission", "concurrent_validates", "concurrency_proof", "safe_ci",
            "validate_lock_authority", "reverie_validation_policy",
        }
        if any(final.get(key) != value for key, value in normalized.items()):
            raise ValueError("Reverie finalizer producer/schema/repository is invalid")
        for key, value in original.items():
            if key not in normalized and (
                key not in final
                or _admission_event_identity(final[key]) != _admission_event_identity(value)
            ):
                raise ValueError(f"Reverie finalization changed immutable source field {key!r}")
        if set(final) - set(original) - set(normalized) - additions:
            raise ValueError("Reverie finalization added an unsupported source field")
        if final.get("raw_result") != original.get("result"):
            raise ValueError("Reverie finalization raw_result differs from the source")
        tree = subprocess.run(
            ["git", "--no-replace-objects", "-C", row["cwd"], "rev-parse", row["commit"] + "^{tree}"],
            capture_output=True, text=True, check=False, timeout=10,
            env={**{key: value for key, value in os.environ.items() if not key.startswith("GIT_")},
                 "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull},
        )
        if tree.returncode or final.get("tree") != tree.stdout.strip():
            raise ValueError("Reverie finalization tree does not bind the source commit")
        for field in ("commit", "host", "started_at", "emitted_at", "outcome", "run_number"):
            if event.get(field) != source.get(field):
                raise ValueError(f"Reverie finalization changed envelope field {field!r}")
        body = _admission_event_identity({"source": source, "final": event}).encode()
        event.update(
            event_id="reverie-final-" + hashlib.sha256(body).hexdigest(),
            event_type="run.correct", run_id=source["run_id"], supersedes=source_id,
            reason="Bind observed safe-ci finalizer provenance to the original Reverie execution",
        )
        successors = [item for item in items if item.get("supersedes") == source_id]
        if successors:
            if len(successors) == 1 and _admission_event_identity(successors[0]) == _admission_event_identity(event):
                return {"schema": "ci-hub-validate-ledger-record/v1", "event_id": event["event_id"], "run_id": event["run_id"], "already_recorded": True}
            raise ValueError("Reverie source already has a different finalization")
        # Apply the actual correction consumer before making new bytes visible.
        _rows_from_events([*items, event], root=root)
        return _spool_record_event(event, root)


def _select_reverie_finalization(
    items: list[dict], source_id: str, final_id: str, run_id: str, *, root: Path
) -> dict:
    """Select a retained two-event chain, never the newest matching row.

    The caller supplies one already-validated union. Keep the full correction
    graph check: an otherwise unrelated malformed/conflicting chain is not an
    excuse to read a partial authority view.
    """
    _rows_from_events(items, root=root)
    if not all(isinstance(value, str) and value for value in (source_id, final_id, run_id)):
        raise ValueError("Reverie finalization requires three nonempty identities")
    sources = [event for event in items if event.get("event_id") == source_id]
    finals = [event for event in items if event.get("event_id") == final_id]
    if source_id == final_id or len(sources) != 1 or len(finals) != 1:
        raise ValueError("Reverie source/final event is missing or ambiguous")
    source, final = sources[0], finals[0]
    original, row = source.get("legacy_row"), final.get("legacy_row")
    if (source.get("event_type") != "run.result"
            or source.get("team") != "reverie"
            or source.get("producer", {}).get("tool") != "reverie-validate-sh"
            or source.get("producer", {}).get("source") != "observed"
            or not isinstance(original, dict)
            or original.get("producer") != "reverie-validate-sh"
            or type(original.get("schema_version")) is not int
            or original.get("schema_version") not in {3, 4, 5}
            or original.get("repo") not in {"reverie", "rrnewton/reverie"}
            or final.get("event_type") != "run.correct"
            or final.get("team") != "reverie"
            or final.get("supersedes") != source_id
            or source.get("run_id") != run_id or final.get("run_id") != run_id
            or final.get("producer", {}).get("tool") != "ci-hub-reverie-validate-run"
            or final.get("producer", {}).get("source") != "observed"
            or not isinstance(row, dict)):
        raise ValueError("Reverie finalization does not bind the requested producer chain")
    normalized = {"schema_version": 5, "producer": "ci-hub-reverie-validate-run", "repo": "rrnewton/reverie"}
    additions = {
        "tree", "raw_result", "gates_run", "gates_expected", "coverage",
        "admission", "concurrent_validates", "concurrency_proof", "safe_ci",
        "validate_lock_authority", "reverie_validation_policy",
    }
    if (any(row.get(key) != value for key, value in normalized.items())
            or any(key not in row or _admission_event_identity(row[key]) != _admission_event_identity(value)
                   for key, value in original.items() if key not in normalized)
            or set(row) - set(original) - set(normalized) - additions
            or row.get("raw_result") != original.get("result")):
        raise ValueError("Reverie finalization changed immutable execution facts")
    for field in ("commit", "host", "started_at", "emitted_at", "outcome", "run_number"):
        if source.get(field) != final.get(field):
            raise ValueError(f"Reverie finalization changed envelope field {field!r}")
    if any(event.get("supersedes") == final_id or (
        event.get("run_id") == run_id
        and event.get("event_type") in {"run.result", "run.correct"}
        and event.get("event_id") not in {source_id, final_id}
    ) for event in items):
        raise ValueError("Reverie finalization has a different or moving chain")
    projected = _rows_from_events([source, final], root=root)
    if len(projected) != 1:
        raise ValueError("Reverie finalization has no single projected row")
    return {"source": source, "final": final, "row": projected[0]}


def reverie_finalization(
    source_id: str, final_id: str, run_id: str, *, root: Path | None = None
) -> dict:
    """Read exact finalization objects from one strict canonical snapshot."""
    root = (root or repository_root()).resolve()
    items, _ = _validated_events(root=root, preserve_admission=True)
    return _select_reverie_finalization(items, source_id, final_id, run_id, root=root)


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


def events(*, root: Path | None = None, preserve_admission: bool = False) -> list[dict]:
    """Canonical validation events, excluding acceptance-test data.

    Fixture bytes remain in their append-only shard or isolated test store.
    They are deliberately absent from this authority view so ``result=pass``
    cannot make an acceptance probe indistinguishable from a validation run.
    """
    root = (root or repository_root()).resolve()
    read_options = ({"object_pairs_hook": qualifying_receipt.AdmissionJsonObject}
                    if preserve_admission else {})
    union_options = ({"canonical": _admission_event_identity} if preserve_admission else {})
    gathered: list[dict] = []
    for path in shard_paths(root):
        gathered.extend(read_shard(path, **read_options))
    for pending in spooled(live_spool_dir(root), **read_options, **union_options).values():
        gathered.extend(pending)
    # Retained batches are PUBLISHED but not yet durable in THIS checkout. A
    # reader that skipped them would blink: publication writes through a
    # separate clone, so between the publish and this root's next sync the
    # receipt is in neither the spool nor the tracked shards. Measured
    # 2026-08-11 -- a producer answered NOT-VALIDATED with 0 records for a
    # receipt it had published seconds earlier. Unioning them closes that gap;
    # event_id dedup makes the overlap with tracked shards free.
    for published in retained(live_spool_dir(root), **read_options, **union_options).values():
        gathered.extend(published)
    return [
        event
        for event in union_events(gathered, **union_options)
        if not _is_acceptance_fixture_event(event)
    ]


def _admission_event_identity(value):
    return qualifying_receipt.admission_json_dumps(value, sort_keys=True)


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


def _validated_events(
    *, root: Path, preserve_admission: bool = False,
    committed_source: tuple[Path, str] | None = None,
) -> tuple[list[dict], frozenset[str]]:
    """Read the event union once and refuse every malformed member."""
    items = events(root=root, preserve_admission=preserve_admission)
    historical_source_digests = _historical_repository_mismatch_source_digests(
        root, committed_source=committed_source,
    )
    # Preserve the original envelope too: normalizing a duplicate legacy_row
    # could erase an earlier admission claim before Rust judges its shape.
    validation_options = ({"serializer": _admission_event_identity} if preserve_admission else {})
    for index, codes in enumerate(validate_events(items, **validation_options), 1):
        event = items[index - 1]
        if codes:
            if event.get("event_type") == "run.correct":
                raise ValueError(
                    f"HistoryRow correction {event.get('event_id')!r} is invalid: "
                    f"event violates {codes}"
                )
            raise ValueError(f"validation event {index} violates {codes}")
        try:
            _legacy_row_repo(
                event,
                historical_source_digests=historical_source_digests,
            )
        except ValueError as error:
            if event.get("event_type") == "run.correct":
                raise ValueError(
                    f"HistoryRow correction {event.get('event_id')!r} is invalid: "
                    f"{error}"
                ) from error
            raise ValueError(
                f"validation event {index} ({event.get('event_id')!r}) is invalid: "
                f"{error}"
            ) from error
    return items, historical_source_digests


def _row_projections(items: list[dict], *, root: Path) -> Iterable[tuple[str, dict]]:
    """Keep each projected row's actual terminal envelope identity."""
    replacements, correction_ids = _legacy_row_corrections(items)
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
            if qualifying_receipt.admission_is_v2(row):
                try:
                    if (qualifying_receipt.admission_transport_identity(row)
                            != qualifying_receipt.admission_transport_identity(restored)):
                        raise ValueError("ledger expansion changed v2 admission evidence")
                    with AdmissionPaths(root) as paths:
                        if restored.get("log_file") != str(paths.path(restored["log_identity"])):
                            raise ValueError("rendered log path differs from v2 identity")
                except (OSError, ValueError):
                    # Retain the original bad row for its per-row refusal.
                    # Do not repair/re-hash it or poison another selected row.
                    restored = row
            yield current, restored


def _rows_from_events(items: list[dict], *, root: Path) -> list[dict]:
    """Project the already-read event union to the HistoryRow compatibility view."""
    out: list[dict] = []
    seen: set[str] = set()
    for _event_id, restored in _row_projections(items, root=root):
        identity = _admission_event_identity(restored)
        if identity not in seen:
            seen.add(identity)
            out.append(restored)
    return out


def _record_identities(
    items: list[dict],
    *,
    historical_source_digests: frozenset[str] = frozenset(),
) -> list[dict]:
    """Identify raw result/correction records before compatibility projection.

    The compact identity keeps the one-read status response bounded even when a
    receipt embeds large per-cell evidence.  Event validation and correction
    validation have already succeeded before this function is called.
    """
    records: list[dict] = []
    for event in items:
        event_type = event.get("event_type")
        if event_type not in {"run.result", "run.correct"}:
            continue
        row = event.get("legacy_row")
        if event_type == "run.correct" and not isinstance(row, dict):
            continue

        row_commit = row.get("commit") if isinstance(row, dict) else None
        if event_type == "run.result":
            commit = event.get("commit")
            if row_commit is not None and row_commit != commit:
                raise ValueError(
                    f"validation record {event.get('event_id')!r} has conflicting "
                    "event and legacy_row commits"
                )
        else:
            commit = row_commit

        if not (
            isinstance(commit, str)
            and len(commit) == 40
            and commit == commit.lower()
            and all(character in "0123456789abcdef" for character in commit)
        ):
            raise ValueError(
                f"validation record {event.get('event_id')!r} has no exact lowercase commit"
            )
        repo = _legacy_row_repo(
            event,
            historical_source_digests=historical_source_digests,
        )
        if repo is None:
            repo = {"hermit": "hermit", "reverie": "reverie"}.get(
                str(event.get("team") or "")
            )
        if not isinstance(repo, str) or not repo:
            raise ValueError(
                f"validation record {event.get('event_id')!r} has no readable repo"
            )
        records.append(
            {
                "event_id": event["event_id"],
                "event_type": event_type,
                "run_id": event["run_id"],
                "repo": repo,
                "commit": commit,
            }
        )
    return records


def status_input(*, root: Path | None = None, preserve_admission: bool = False) -> dict:
    """One-read input for status: projected rows plus every raw record identity."""
    root = (root or repository_root()).resolve()
    items, historical_source_digests = _validated_events(root=root, preserve_admission=preserve_admission)
    projected = _rows_from_events(items, root=root)
    return {
        "schema_version": 1,
        "rows": projected,
        "records": _record_identities(
            items,
            historical_source_digests=historical_source_digests,
        ),
    }


def rows(
    *, root: Path | None = None, preserve_admission: bool = False,
    committed_source: tuple[Path, str] | None = None,
) -> list[dict]:
    """HistoryRow view, with explicit Git provenance for an immutable snapshot."""
    root = (root or repository_root()).resolve()
    items, _historical_source_digests = _validated_events(
        root=root, preserve_admission=preserve_admission, committed_source=committed_source,
    )
    return _rows_from_events(items, root=root)


def canonical_finalized_row(
    root: Path | None = None,
    *,
    repo: str,
    run_id: str,
    record_id: str,
    commit: str,
    cwd: str,
    started_at: str,
) -> dict:
    """Select one exact canonical terminal row, without qualifying its verdict.

    Unlike the diagnostic current-run view, this query refuses incomplete or
    ambiguous correction groups, including malformed singletons. The caller
    must still verify the selected row's admission, finalized evidence and
    producer census; this selection does not manufacture those proofs.
    """
    root = (root or repository_root()).resolve()
    items, _ = _validated_events(root=root, preserve_admission=True)
    return _select_finalized_row(
        _rows_from_events(items, root=root), repo=repo, run_id=run_id,
        record_id=record_id, commit=commit, cwd=cwd, started_at=started_at,
    )


def _select_finalized_row(
    rows: list[dict], *, repo: str, run_id: str, record_id: str,
    commit: str, cwd: str, started_at: str,
) -> dict:
    expected = dict(repo=repo, run_id=run_id, record_id=record_id,
                    commit=commit, cwd=cwd, started_at=started_at)
    for field, value in expected.items():
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"canonical producer selector requires nonempty {field}")

    selected = _current_finalized_row(rows, repo=repo, run_id=run_id)
    for field, value in expected.items():
        if selected.get(field) != value:
            raise ValueError(f"canonical producer terminal differs from requested {field}")
    return selected


def _current_finalized_row(rows: list[dict], *, repo: str, run_id: str) -> dict:
    """Check the whole correction group before projecting its terminal."""

    team = _ledger_team(repo)
    # Canonical repository aliases share a correction group. Literal repo
    # equality is required only for the selected terminal, below.
    group = [row for row in rows
             if _ledger_team(row.get("repo")) == team and row.get("run_id") == run_id]
    if not group:
        raise ValueError("canonical producer run is absent")

    # Do not filter by the expected record/head/cwd/start first: that could hide
    # an opposing successor or a missing predecessor under a different identity.
    records: dict[str, dict] = {}
    for row in group:
        identity = row.get("record_id")
        if not isinstance(identity, str) or not identity.strip():
            raise ValueError("canonical producer run has a missing record_id")
        if identity in records:
            raise ValueError("canonical producer run has duplicate record_id values")
        records[identity] = row

    superseded: set[str] = set()
    for identity, row in records.items():
        predecessor = row.get("corrects")
        if predecessor is None:
            continue
        if (not isinstance(predecessor, str) or predecessor not in records
                or predecessor == identity):
            raise ValueError("canonical producer run has an invalid correction predecessor")
        if predecessor in superseded:
            raise ValueError("canonical producer run has branched corrections")
        superseded.add(predecessor)

    terminals = records.keys() - superseded
    if len(terminals) != 1:
        raise ValueError("canonical producer run has no unique terminal record")
    visited: set[str] = set()
    current = next(iter(terminals))
    while current is not None:
        if current in visited:
            raise ValueError("canonical producer run has a correction cycle")
        visited.add(current)
        current = records[current].get("corrects")
    if len(visited) != len(records):
        raise ValueError("canonical producer run has a disconnected correction chain")

    # Only a fully checked group may use the existing current-run projection.
    # Keep that reader's historical/diagnostic behavior unchanged.
    [selected] = current_validation_run_rows(group)
    return selected


CANONICAL_PARENT_REPOSITORY = "https://github.com/rrnewton/dev-hermit.git"
_COMMITTED_GIT_BYTES = 256 << 20  # Same per-command capacity as canonical export.


def _committed_git(root: Path, *arguments: str, limit: int = _COMMITTED_GIT_BYTES) -> bytes:
    """Bound Git transport independently of the size claimed by an object."""
    command = ["git", "--no-replace-objects", "-C", str(root), *arguments]
    environment = _git_environment()
    environment["GIT_NO_LAZY_FETCH"] = "1"
    with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, env=environment) as process:
        output = bytearray()
        errors = bytearray()
        deadline = time.monotonic() + 60
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, (output, limit))
                selector.register(process.stderr, selectors.EVENT_READ, (errors, 65536))
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise ValueError("committed validation Git read timed out")
                    for key, _event in selector.select(remaining):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        buffer, capacity = key.data
                        if len(buffer) + len(chunk) > capacity:
                            raise ValueError("committed validation Git read exceeds capacity")
                        buffer.extend(chunk)
                status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            if status != 0 or errors:
                raise ValueError(
                    f"committed validation Git read refused ({status}): "
                    + errors[:1000].decode(errors="replace").strip()
                )
            return bytes(output)
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.wait()
            raise


def _committed_sha(value: object, label: str, length: int = 40) -> str:
    if not isinstance(value, str) or re.fullmatch(rf"[0-9a-f]{{{length}}}", value) is None:
        raise ValueError(f"committed validation {label} requires an exact lowercase digest")
    return value


def _committed_path(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("committed validation input path is not a string")
    parts = value.split("/")
    if (not parts or parts[0] not in {"ledger", "ledger-recovery"}
            or any(part in {"", ".", ".."} or "\0" in part for part in parts)):
        raise ValueError("committed validation input has an unsafe namespace path")
    return value


def _committed_shard(value: str) -> bool:
    return re.fullmatch(
        r"(?:ledger/[^/]+/[^/]+/[0-9]{4}-[0-9]{2}\.jsonl|"
        r"ledger-recovery/[^/]+/[^/]+/[0-9]{4}-[0-9]{2}/[^/]+\.jsonl)", value,
    ) is not None


def _refuse_view_mutation(self, *_arguments, **_keywords):
    raise TypeError(
        "committed validation views are read-only; rows() and events() return private copies"
    )


class _ReadOnlyList(list):
    """A list view whose items cannot change through the list interface.

    Copying it yields the ordinary mutable structure that rows() and events()
    return.  Pickling it is refused; pickle rows() or events() instead.
    """

    __slots__ = ()
    append = extend = insert = pop = remove = clear = sort = reverse = _refuse_view_mutation
    __setitem__ = __delitem__ = __iadd__ = __imul__ = _refuse_view_mutation
    # _read_only_view fills a view with list.extend after __new__, so nothing
    # calls __init__; calling it again would replace the items.  Refusing
    # __setattr__ also refuses rebinding __class__ to a mutable subclass.
    __init__ = __setattr__ = __delattr__ = _refuse_view_mutation

    def __copy__(self):
        return list(self)

    def __deepcopy__(self, memo):
        copied = []
        memo[id(self)] = copied
        list.extend(copied, (copy.deepcopy(item, memo) for item in self))
        return copied

    __reduce_ex__ = _refuse_view_mutation


class _ReadOnlyDict(dict):
    """A dict view whose items cannot change through the dict interface."""

    __slots__ = ()
    clear = pop = popitem = setdefault = update = _refuse_view_mutation
    __setitem__ = __delitem__ = __ior__ = _refuse_view_mutation
    # As for _ReadOnlyList: views are filled with dict.update after __new__.
    __init__ = __setattr__ = __delattr__ = _refuse_view_mutation

    def __copy__(self):
        return dict(self)

    def __deepcopy__(self, memo):
        copied = {}
        memo[id(self)] = copied
        for key, item in dict.items(self):
            copied[copy.deepcopy(key, memo)] = copy.deepcopy(item, memo)
        return copied

    __reduce_ex__ = _refuse_view_mutation


class _ReadOnlyAdmissionObject(qualifying_receipt.AdmissionJsonObject):
    """An AdmissionJsonObject view; its items and ``pairs`` cannot be rebound."""

    clear = pop = popitem = setdefault = update = _refuse_view_mutation
    __setitem__ = __delitem__ = __ior__ = _refuse_view_mutation
    __init__ = __setattr__ = __delattr__ = _refuse_view_mutation

    def _mutable(self, memo):
        copied = qualifying_receipt.AdmissionJsonObject.__new__(
            qualifying_receipt.AdmissionJsonObject
        )
        memo[id(self)] = copied
        for key, item in dict.items(self):
            dict.__setitem__(copied, copy.deepcopy(key, memo), copy.deepcopy(item, memo))
        copied.pairs = copy.deepcopy(self.pairs, memo)
        return copied

    def __copy__(self):
        copied = qualifying_receipt.AdmissionJsonObject.__new__(
            qualifying_receipt.AdmissionJsonObject
        )
        dict.update(copied, self)
        copied.pairs = self.pairs
        return copied

    def __deepcopy__(self, memo):
        return self._mutable(memo)

    __reduce_ex__ = _refuse_view_mutation


_VIEW_LEAVES = frozenset({str, int, float, bool, type(None)})


class _NotViewable(Exception):
    pass


def _read_only_view(value, memo: dict[int, object]):
    """Return a read-only structure equal to ``value``, sharing only immutable leaves.

    Shared references stay shared, as copy.deepcopy keeps them, so an
    AdmissionJsonObject view's ``pairs`` values are the same objects as its
    item values.  Any type other than those rows and events are built from,
    including a dict key that is not a leaf, raises _NotViewable, and the
    caller then returns a private copy instead.
    """
    kind = type(value)
    leaves = _VIEW_LEAVES
    if kind in leaves:
        return value
    key = id(value)
    if key in memo:
        return memo[key]
    if kind is list:
        view = _ReadOnlyList.__new__(_ReadOnlyList)
        memo[key] = view
        list.extend(view, [item if type(item) in leaves else _read_only_view(item, memo)
                           for item in value])
        return view
    if kind is tuple:
        items = tuple([item if type(item) in leaves else _read_only_view(item, memo)
                       for item in value])
        # As copy.deepcopy does: a tuple reached again through its own items
        # is the one already recorded.
        if key in memo:
            return memo[key]
        memo[key] = items
        return items
    if kind is dict:
        view = _ReadOnlyDict.__new__(_ReadOnlyDict)
    elif kind is qualifying_receipt.AdmissionJsonObject:
        if set(vars(value)) != {"pairs"} or type(value.pairs) is not tuple:
            raise _NotViewable("admission object has unexpected attributes")
        view = _ReadOnlyAdmissionObject.__new__(_ReadOnlyAdmissionObject)
    else:
        raise _NotViewable(f"no read-only view for {kind.__name__}")
    if value and not set(map(type, value)) <= leaves:
        raise _NotViewable("object key is not a leaf")
    memo[key] = view
    dict.update(view, [(name, item if type(item) in leaves else _read_only_view(item, memo))
                       for name, item in value.items()])
    if kind is not dict:
        object.__setattr__(view, "pairs", _read_only_view(value.pairs, memo))
    return view


class CommittedValidationNamespace:
    """One complete immutable namespace; construction never accepts event subsets.

    Git supplies authority and exact original bytes. ``state_root`` only expands
    private path tokens. This selector grants no admission or artifact proof.
    """

    def __init__(self, source_root: Path, source_commit: str, *, state_root: Path | None = None):
        self._root = Path(source_root).resolve()
        self._commit = _committed_sha(source_commit, "source commit")
        state = Path(state_root).resolve() if state_root is not None else self._root
        top = _committed_git(self._root, "rev-parse", "--show-toplevel").decode().strip()
        if Path(top).resolve() != self._root:
            raise ValueError("committed validation source must be the exact Git root")
        origin = _committed_git(self._root, "config", "--get-all", "remote.origin.url").decode().strip()
        # The hosted reader uses a read-only deploy key; both exact spellings
        # name the same fixed repository. Public provenance always uses HTTPS.
        if origin not in {CANONICAL_PARENT_REPOSITORY, "git@github.com:rrnewton/dev-hermit.git"}:
            raise ValueError("committed validation source is not the canonical parent repository")
        resolved = _committed_git(self._root, "rev-parse", "--verify", f"{self._commit}^{{commit}}").decode().strip()
        if resolved != self._commit:
            raise ValueError("committed validation source identity changed")
        # The caller fetches first. An arbitrary unpublished object in this
        # repository is not made canonical merely by supplying its full hash.
        _committed_git(self._root, "merge-base", "--is-ancestor", self._commit,
                       "refs/remotes/origin/main")
        self._sources: list[dict] = []
        listed = _committed_git(self._root, "ls-tree", "-r", "-z", self._commit,
                                "--", "ledger", "ledger-recovery")
        for entry in listed.split(b"\0"):
            if not entry:
                continue
            metadata, raw_path = entry.split(b"\t", 1)
            mode, kind, object_id = metadata.decode().split()
            relative = _committed_path(raw_path.decode("utf-8"))
            if kind != "blob" or mode not in {"100644", "100755"}:
                raise ValueError("committed validation namespace contains a nonregular Git entry")
            _committed_sha(object_id, "blob")
            data = _committed_git(self._root, "cat-file", "blob", object_id)
            self._sources.append({"path": relative, "blob_oid": object_id, "data": data})
        self._sources.sort(key=lambda value: value["path"])
        self._occurrences: dict[str, list[dict]] = {}
        with tempfile.TemporaryDirectory(prefix="canonical-committed-validation-") as raw:
            snapshot = Path(raw)
            for source in self._sources:
                path = snapshot / source["path"]
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(source["data"])
            # This reader still validates the entire event union, the original
            # admission pairs, and the two exact historical exception shards.
            items, _ = _validated_events(
                root=snapshot, preserve_admission=True,
                committed_source=(self._root, self._commit),
            )
            for source in self._sources:
                if not _committed_shard(source["path"]):
                    continue
                for line in source["data"].splitlines(keepends=True):
                    if not line.strip():
                        continue
                    event = parse_line(line.decode("utf-8"),
                                       object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
                    digest = hashlib.sha256(_admission_event_identity(event).encode()).hexdigest()
                    self._occurrences.setdefault(digest, []).append({
                        "repository": CANONICAL_PARENT_REPOSITORY,
                        "commit": self._commit, "shard": source["path"],
                        "blob_oid": source["blob_oid"],
                        "event_line_sha256": hashlib.sha256(line).hexdigest(),
                    })
        self._events = {event["event_id"]: event for event in items}
        self._projections = list(_row_projections(items, root=state))
        # Identity -> event ids of self._projections, built by the first select().
        self._projection_event_ids: dict[str, list[str]] | None = None
        self._rows = _rows_from_events(items, root=state)
        self._origin_cache: dict[tuple[str, str, str], bytes] = {}
        self._views: dict[str, list] = {}

    def __enter__(self):
        return self

    def __exit__(self, *_exception):
        return False

    def rows(self) -> list[dict]:
        return copy.deepcopy(self._rows)

    def events(self) -> list[dict]:
        """The original validated union, for existing event count/digest capture."""
        return copy.deepcopy(list(self._events.values()))

    def _view(self, name: str, value: list, *, retain: bool = True) -> list:
        if name in self._views:
            return self._views[name]
        try:
            view = _read_only_view(value, {})
        except _NotViewable:
            return copy.deepcopy(value)
        if retain:
            self._views[name] = view
        return view

    def rows_view(self) -> list[dict]:
        """rows() for a caller that only reads: equal items it cannot change.

        The view is built once and shared, because the website export reads
        rows twice; it lives as long as the namespace.  Every list, dict and
        AdmissionJsonObject in it refuses item and attribute assignment,
        __init__ and __class__ rebinding with TypeError, so an ordinary
        mutation cannot reach a later reader.  It is never the namespace's own
        storage: select(), terminals(), rows() and events() read separate
        objects.  copy.deepcopy of the view is equal, type for type, to rows().
        """
        return self._view("rows", self._rows)

    def events_view(self) -> list[dict]:
        """events() for a caller that only reads, on the rows_view() terms.

        Unlike rows_view(), each call builds a new view and the namespace keeps
        no reference to it.  The export reads events once, and a retained view
        would hold about 10M objects in memory for the rest of the export.
        """
        return self._view("events", list(self._events.values()), retain=False)

    def source_files(self) -> list[dict]:
        """Original path/blob/data operands for provenance and reproduction."""
        return copy.deepcopy(self._sources)

    def terminals(self, *, repo: str, schema_version: int | None = None) -> list[dict]:
        """Select whole current groups without granting website admission.

        A schema filter chooses groups containing that schema, not individual
        rows within a correction chain. Thus a successor of another schema
        still supersedes the older row. Failed and unavailable outcomes are
        retained; callers must use the native admission/proof verifier.
        """
        if not isinstance(repo, str) or not repo.strip():
            raise ValueError("canonical producer selector requires nonempty repo")
        if schema_version is not None and (type(schema_version) is not int or schema_version < 1):
            raise ValueError("committed validation schema selector must be a positive integer")
        team = _ledger_team(repo)
        groups: dict[str, list[dict]] = {}
        for row in self._rows:
            if _ledger_team(row.get("repo")) == team:
                groups.setdefault(row.get("run_id"), []).append(row)
        selected = []
        for run_id, group in groups.items():
            if schema_version is not None and not any(
                row.get("schema_version") == schema_version for row in group
            ):
                continue
            if not isinstance(run_id, str) or not run_id.strip():
                raise ValueError("canonical producer selector requires nonempty run_id")
            row = _current_finalized_row(self._rows, repo=repo, run_id=run_id)
            selected.append(self.select(**{key: row.get(key) for key in (
                "repo", "run_id", "record_id", "commit", "cwd", "started_at",
            )}))
        return sorted(selected, key=lambda item: item["terminal"]["run_id"])

    def census_terminals(self, *, repo: str) -> list[dict]:
        """Select current rows of runs that claim a schema-5 derived census.

        A run claims one when any of its rows has schema_version 5 and a
        raw_result_input_census_v1 field. Only those runs acquire the stable
        run identity requirement; historical schema-5 rows without a census
        keep their existing absence. The repository's rows are reduced with
        current_validation_run_rows, which leaves a malformed or ambiguous
        correction group visible rather than guessing, and each remaining row
        of a claiming run is resolved through select(), which judges its
        predecessors and opposing successors. Like terminals(), this grants no
        admission: callers must still apply the native proof verifier.
        """
        if not isinstance(repo, str) or not repo.strip():
            raise ValueError("canonical producer selector requires nonempty repo")
        team = _ledger_team(repo)
        # Only reads rows, so it takes the shared read-only view instead of a
        # private deep copy.
        rows = [row for row in self.rows_view() if _ledger_team(row.get("repo")) == team]
        census_runs = {row.get("run_id") for row in rows if row.get("schema_version") == 5
                       and "raw_result_input_census_v1" in row}
        if any(not isinstance(run, str) or not run.strip() for run in census_runs):
            raise ValueError("claimed derived census lacks a stable run identity")
        return [
            self.select(**{key: row.get(key) for key in (
                "repo", "run_id", "record_id", "commit", "cwd", "started_at",
            )})
            for row in current_validation_run_rows(rows)
            if row.get("run_id") in census_runs
        ]

    def select(self, **selector) -> dict:
        row = _select_finalized_row(self._rows, **selector)
        identity = _admission_event_identity(row)
        if self._projection_event_ids is None:
            # select() runs once per census run in the finalized-proof capture,
            # again in verify_origin, and once per group in terminals().
            # Serializing every projection inside each call made the capture
            # O(selections x projections). The projections are never mutated
            # after construction, so serialize each one once.
            index: dict[str, list[str]] = {}
            for event_id, projected in self._projections:
                index.setdefault(_admission_event_identity(projected), []).append(event_id)
            self._projection_event_ids = index
        terminals = set(self._projection_event_ids.get(identity, ()))
        if len(terminals) != 1:
            raise ValueError("committed validation terminal has ambiguous original events")
        event_id = next(iter(terminals))
        event = self._events[event_id]
        digest = hashlib.sha256(_admission_event_identity(event).encode()).hexdigest()
        team = _ledger_team(row["repo"])
        if team not in {"hermit", "reverie"}:
            raise ValueError("committed validation terminal has no canonical repository")
        terminal = {"repository": f"rrnewton/{team}", "run_id": row["run_id"],
                    "record_id": row["record_id"], "event_id": event_id,
                    "event_sha256": digest, "hermit_commit": row["commit"]}
        return copy.deepcopy({"row": row, "terminal": terminal,
                              "parent": self._occurrences[digest][0]})

    def verify_origin(self, parent: dict, terminal: dict) -> None:
        """Revalidate an original carrier occurrence against this current graph."""
        parent_fields = {"repository", "commit", "shard", "blob_oid", "event_line_sha256"}
        terminal_fields = {"repository", "run_id", "record_id", "event_id", "event_sha256", "hermit_commit"}
        if not isinstance(parent, dict) or set(parent) != parent_fields:
            raise ValueError("committed validation parent descriptor has unknown or missing fields")
        if not isinstance(terminal, dict) or set(terminal) != terminal_fields:
            raise ValueError("committed validation terminal descriptor has unknown or missing fields")
        if parent["repository"] != CANONICAL_PARENT_REPOSITORY:
            raise ValueError("committed validation parent descriptor has the wrong repository")
        commit = _committed_sha(parent["commit"], "original source commit")
        blob = _committed_sha(parent["blob_oid"], "original blob")
        line_digest = _committed_sha(parent["event_line_sha256"], "original line", 64)
        _committed_sha(terminal["event_sha256"], "original event", 64)
        _committed_sha(terminal["hermit_commit"], "measured commit")
        shard = _committed_path(parent["shard"])
        if not _committed_shard(shard):
            raise ValueError("committed validation original occurrence is not a canonical shard")
        candidates = [row for event_id, row in self._projections if event_id == terminal["event_id"]]
        if len(candidates) != 1:
            raise ValueError("committed validation original event is absent or superseded")
        row = candidates[0]
        selector = {key: row.get(key) for key in ("repo", "run_id", "record_id", "commit", "cwd", "started_at")}
        if self.select(**selector)["terminal"] != terminal:
            raise ValueError("committed validation original event is not the current terminal")
        key = (commit, shard, blob)
        if key not in self._origin_cache:
            _committed_git(self._root, "merge-base", "--is-ancestor", commit, self._commit)
            entry = _committed_git(self._root, "ls-tree", "-z", commit, "--", shard)
            expected = f"100644 blob {blob}\t{shard}\0".encode()
            executable = f"100755 blob {blob}\t{shard}\0".encode()
            if entry not in (expected, executable):
                raise ValueError("committed validation original shard/blob identity differs")
            self._origin_cache[key] = _committed_git(self._root, "cat-file", "blob", blob)
        for line in self._origin_cache[key].splitlines(keepends=True):
            if hashlib.sha256(line).hexdigest() != line_digest:
                continue
            event = parse_line(line.decode("utf-8"),
                               object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
            if (event.get("event_id") == terminal["event_id"]
                    and hashlib.sha256(_admission_event_identity(event).encode()).hexdigest()
                    == terminal["event_sha256"]):
                return
        raise ValueError("committed validation original terminal line is absent or changed")


def capture_committed_validation_namespace(
    source_root: Path, source_commit: str, *, state_root: Path | None = None,
) -> CommittedValidationNamespace:
    return CommittedValidationNamespace(source_root, source_commit, state_root=state_root)


def canonical_finalized_row_at_commit(
    source_root: Path, source_commit: str, *, state_root: Path | None = None, **selector,
) -> dict:
    with capture_committed_validation_namespace(source_root, source_commit, state_root=state_root) as snapshot:
        return snapshot.select(**selector)


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
    row_command = commands.add_parser("rows", description="emit the canonical HistoryRow compatibility view", help="emit the canonical HistoryRow compatibility view")
    status_command = commands.add_parser(
        "status-input",
        description="emit projected rows and raw record identities from one event-union read",
        help="emit one-read input for validation status",
    )
    for command in (row_command, status_command):
        command.add_argument("--preserve-admission", action="store_true",
                             help="preserve original JSON fields for admission-aware receipt readers")
    producer_command = commands.add_parser(
        "producer-row",
        description="select one exact terminal producer row from the canonical ledger; "
        "refuse incomplete or ambiguous run corrections; preserve admission JSON "
        "without qualifying the result or census",
        help="read one exact canonical terminal producer row",
    )
    for name in ("repo", "run-id", "record-id", "commit", "cwd", "started-at"):
        producer_command.add_argument("--" + name, required=True,
                                      help="exact producer " + name + " (no latest-row fallback)")
    commands.add_parser("events", description="emit the canonical event union", help="emit the canonical event union")
    commands.add_parser("record", description="read one HistoryRow JSON object from stdin and journal it", help="read one HistoryRow JSON object from stdin and journal it")
    source_command = commands.add_parser("reverie-source", help="bind one exact raw Reverie producer event")
    for name in ("target", "checkout", "log", "started"):
        source_command.add_argument("--" + name, required=True)
    commands.add_parser("record-reverie-finalization", help="append finalizer provenance for one retained Reverie source event")
    final_command = commands.add_parser("reverie-finalization", help="read one exact validated Reverie finalization chain")
    for name in ("source-id", "final-id", "run-id"):
        final_command.add_argument("--" + name, required=True)
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
    if args.command == "producer-row":
        try:
            value = canonical_finalized_row(
                repo=args.repo, run_id=args.run_id, record_id=args.record_id,
                commit=args.commit, cwd=args.cwd, started_at=args.started_at,
            )
            print(qualifying_receipt.admission_json_dumps(value, sort_keys=True))
            return 0
        except (LedgerError, OSError, ValueError) as error:
            print(f"validate-ledger: REFUSED: {error}; inspect canonical rows and "
                  "supply the exact terminal producer identity", file=sys.stderr)
            return 2
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
    if args.command in {"reverie-source", "reverie-finalization"}:
        try:
            value = (reverie_source(args.target, args.checkout, args.log, args.started)
                     if args.command == "reverie-source" else
                     reverie_finalization(args.source_id, args.final_id, args.run_id))
            print(qualifying_receipt.admission_json_dumps(value, sort_keys=True))
            return 0
        except (OSError, ValueError) as error:
            print(f"validate-ledger: REFUSED: {error}", file=sys.stderr)
            return 2
    if args.command in {"record", "record-reverie-finalization"}:
        try:
            value = json.load(sys.stdin, object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
            if not isinstance(value, dict):
                raise ValueError("input is not a JSON object")
            if args.command == "record-reverie-finalization":
                if set(value) != {"source", "row"}:
                    raise ValueError("Reverie finalization requires exactly source and row")
                receipt = record_reverie_finalization(value["source"], value["row"])
            else:
                receipt = record(value)
            print(json.dumps(receipt, sort_keys=True))
            return 0
        except (OSError, ValueError, json.JSONDecodeError) as error:
            print(f"validate-ledger: REFUSED: {error}", file=sys.stderr)
            return 2
    if args.command == "status-input":
        value = status_input(preserve_admission=args.preserve_admission)
        if args.preserve_admission:
            print(qualifying_receipt.admission_json_dumps(value, sort_keys=True))
        else:
            print(json.dumps(value, sort_keys=True, separators=(",", ":")))
        return 0
    if args.command == "rows" and args.preserve_admission:
        for row in rows(preserve_admission=True):
            print(qualifying_receipt.admission_json_dumps(row, sort_keys=True))
        return 0
    emit_jsonl(events() if args.command == "events" else rows())
    return 0


if __name__ == "__main__":
    sys.exit(main())
