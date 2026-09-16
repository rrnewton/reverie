#!/usr/bin/env python3
"""The SINGLE qualifying-receipt predicate for the Python consumers.

Python twin of `ci-hub/lib/qualifying_receipt.rs`. "Does this ledger row qualify
as a full green?" was answered by independent inline certifiers across three
languages; the original sweep found five and follow-up review found two more in
anchor selection and receipt finalization. Each was its own floor and drifted
(see task
`one-shared-qualifying-receipt-predicate-five-consumers-bypass-the-registry`,
source sweep `ai_docs/2026-08-04-floor-consumer-sweep.md`). The fix is ONE data
artifact -- `ci-hub/validate/qualifying-receipt.json` -- that every consumer
READS rather than restating inline. Admission provenance is stricter: its
semantic verifier is implemented exactly once here in :func:`admission_verdict`.
The Rust predicate delegates that clause to this module, and the shell consumer
reaches it through Rust's ``receipt-digest --require-qualifying`` path. This
keeps canonical lock identity and owner-ancestry semantics out of parallel
Python/Rust/Bash implementations.

Resolution order (mirrors the Rust module):
  1. `$QUALIFYING_RECEIPT_PREDICATE` -- an explicit override path (the mutation
     test points every consumer here; NEVER at the live file).
  2. the on-disk `ci-hub/validate/qualifying-receipt.json` beside this file.
A malformed override / on-disk file is a deploy defect and raises (loud), never a
silent fallback that would mask the very drift this module exists to prevent.

COMPLETENESS IS A COVERAGE QUESTION, NOT A COUNT QUESTION: a count-capable
receipt is held to per-node coverage, and `executed_tests` survives ONLY as the
zero-execution floor. See the JSON `_completeness_is_coverage_not_count` note.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tempfile
import sys
from enum import Enum
from pathlib import Path
from typing import Any
_CI_HUB_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_CI_HUB_DIR / "lib"))
sys.path.insert(0, str(_CI_HUB_DIR / "validate"))
import bounded_capture  # noqa: E402
from gate_completeness import gates_complete  # noqa: E402


PASSED_TESTS_REFUSAL_REASON = "passed_tests != executed_tests for passing row"
FILTERED_TESTS_I64_REFUSAL_REASON = (
    "filtered_tests does not fit signed 64-bit integer"
)
COUNT_REPRESENTATION = {
    "applies_at_schema_min": 4,
    "fields": ["executed_tests", "passed_tests", "filtered_tests"],
    "signed_bits": 64,
}
I64_MIN = -(1 << 63)
I64_MAX = (1 << 63) - 1
U64_MAX = (1 << 64) - 1

PREDICATE_ENV = "QUALIFYING_RECEIPT_PREDICATE"
# The canonical file lives beside this module's `validate/` sibling. The literal
# repo-relative path lives in the Rust module's PREDICATE_REL; here we resolve
# against this file so a Python consumer needs no repo-root argument.
PREDICATE_REL = "validate/qualifying-receipt.json"

_ACTIVE: dict[str, Any] | None = None


class ProducerDefinitionError(RuntimeError):
    """The validated commit cannot be bound to one registered whole map."""


class ProducerDefinitionUndereferenceable(ProducerDefinitionError):
    """No candidate checkout on THIS host can even hold the validated commit.

    This is a statement about the READER, not about the evidence, and the two
    must never share an outcome.  "The receipt does not qualify" blames the
    fleet's evidence; "I cannot read the definition" blames my own checkout.
    Measured 2026-08-11: an actor holding the identical 1423-row shard, with
    both rows for the SHA present, reported `NOT-VALIDATED, 1 non-qualifying`
    purely because `hermit/` was an uninitialized submodule -- an empty but
    EXISTING directory, so it passed the `is_dir()` candidate filter, failed
    resolution, and was reported as unresolvable evidence.  Initializing the
    submodule flipped the same receipt to VALIDATED with no other change.  An
    actor in that state silently disbelieves every valid receipt in the fleet.

    Subclasses ProducerDefinitionError deliberately: every existing consumer
    that already fails closed on that type keeps doing so unchanged.  Only
    consumers that opt in can tell the two apart.
    """


def producer_definition_verifier_path() -> Path:
    """The one semantic producer-map verifier.

    Registry shape, transition provenance, expiry, and exact whole-map
    membership live in ``validation/verify_receipt.sh``.  Ledger consumers call
    it through this adapter instead of reimplementing those rules.
    """
    override = os.environ.get("PRODUCER_DEFINITION_VERIFIER")
    if override:
        return Path(override)
    return Path(__file__).resolve().parent / "validation" / "verify_receipt.sh"


def _run_producer_definition_verifier(
    *arguments: str, input_body: str | None = None
) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(
        [str(producer_definition_verifier_path()), *arguments],
        input=input_body,
        env=os.environ.copy(),
    )


def registered_producer_definition() -> dict[str, str]:
    """Return the validated primary map from the one semantic verifier."""
    result = _run_producer_definition_verifier("--producer-definition-primary")
    if result.returncode != 0:
        raise RuntimeError(
            "producer-definition verifier could not return the primary map: "
            f"rc={result.returncode}: {result.stderr.strip()}"
        )
    try:
        primary = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"producer-definition verifier returned invalid JSON: {error}"
        ) from error
    # Defensive parsing before using trusted-verifier output in `git rev-parse`.
    # This is path safety, not a second definition-membership decision.
    if not isinstance(primary, dict) or not primary:
        raise RuntimeError("producer-definition verifier returned no primary map")
    for relative, blob in primary.items():
        if (
            not isinstance(relative, str)
            or relative.startswith("/")
            or any(part in ("", ".", "..") for part in relative.split("/"))
            or not isinstance(blob, str)
            or _SHA40.fullmatch(blob) is None
        ):
            raise RuntimeError(
                "producer-definition verifier returned an unsafe primary map"
            )
    return primary


def producer_definition_allowed(
    definition: dict[str, str], *, sha: str | None = None
) -> bool:
    """Ask whether ``definition`` is allowed, including any exact commit bound."""
    arguments = ["--producer-definition-check", "-"]
    if sha is not None:
        arguments.extend(("--sha", sha))
    result = _run_producer_definition_verifier(
        *arguments, input_body=json.dumps(definition)
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    raise RuntimeError(
        "producer-definition verifier could not evaluate the resolved map: "
        f"rc={result.returncode}: {result.stderr.strip()}"
    )


def producer_definition_repository(row: dict[str, Any]) -> str:
    """Choose the product object store, never a different receipt policy."""
    repo = row.get("repo")
    if repo in (None, "hermit", "rrnewton/hermit"):
        return "rrnewton/hermit"
    if repo in ("reverie", "rrnewton/reverie"):
        return "rrnewton/reverie"
    raise ProducerDefinitionError(f"unsupported producer repository {repo!r}")


def producer_definition_repositories(
    row: dict[str, Any], repositories: list[str | Path] | None = None
) -> list[Path]:
    """Object stores that may durably resolve the validated commit.

    The recorded cwd is the most specific source but is routinely reclaimed.
    Explicit caller repositories and the canonical Hermit primary are durable
    fallbacks.  A repository is evidence only if git can resolve the exact
    commit and every registered path within it.
    """
    candidates: list[Path] = []
    recorded = row.get("cwd")
    if isinstance(recorded, str) and recorded:
        candidates.append(Path(recorded))
    if repositories:
        candidates.extend(Path(item) for item in repositories)
    override = os.environ.get("PRODUCER_DEFINITION_REPO")
    if override:
        candidates.append(Path(override))
    product = producer_definition_repository(row).rsplit("/", 1)[1]
    candidates.append(Path(__file__).resolve().parents[1] / product)
    seen: set[str] = set()
    ordered: list[Path] = []
    for candidate in candidates:
        key = str(candidate)
        if key in seen or not candidate.is_dir():
            continue
        seen.add(key)
        ordered.append(candidate)
    return ordered


def _checkout_holds_commit(checkout: Path, sha: str) -> bool:
    """Can git in ``checkout`` durably resolve ``sha`` as a commit object?

    Commit existence is the minimum identity primitive; a directory that merely
    EXISTS proves nothing. An uninitialized submodule is a real directory with
    no object store, which is exactly why the candidate filter alone cannot
    detect it.
    """
    try:
        return (
            _capture(
                ["git", "-C", str(checkout), "cat-file", "-e", f"{sha}^{{commit}}"],
                text=True,
                check=False,
            ).returncode
            == 0
        )
    except OSError:
        return False


def _checkout_defect(checkout: Path) -> str:
    """Why this candidate could not hold the commit, in the reader's terms."""
    if not checkout.is_dir():
        return "no such directory"
    if not any(checkout.iterdir()):
        return "EMPTY DIRECTORY -- uninitialized submodule"
    inside = _capture(
        ["git", "-C", str(checkout), "rev-parse", "--is-inside-work-tree"],
        text=True,
        check=False,
    )
    if inside.returncode != 0 or inside.stdout.strip() != "true":
        return "not a git work tree"
    return "git repository, but it does not contain that commit"


def _decoded_producer_definition(
    output: str, *, checkout: Path
) -> dict[str, Any]:
    try:
        producer = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"producer-definition verifier returned invalid JSON: {error}"
        ) from error
    if (
        not isinstance(producer, dict)
        or not isinstance(producer.get("definition"), dict)
        or not producer["definition"]
        or producer.get("coverage_status")
        not in ("legacy-selected-paths", "complete")
        or producer.get("paths") != sorted(producer["definition"])
        or producer.get("resolved_from") != str(checkout)
    ):
        raise RuntimeError(
            "producer-definition verifier returned malformed resolved evidence"
        )
    valid_commits = producer.get("valid_commits")
    if valid_commits is not None and (
        not isinstance(valid_commits, list)
        or not valid_commits
        or any(
            not isinstance(commit, str) or _SHA40.fullmatch(commit) is None
            for commit in valid_commits
        )
        or len(set(valid_commits)) != len(valid_commits)
    ):
        raise RuntimeError(
            "producer-definition verifier returned an invalid commit bound"
        )
    return producer


def resolve_producer_definition(
    row: dict[str, Any],
    sha: str,
    *,
    repositories: list[str | Path] | None = None,
) -> dict[str, Any]:
    """Derive and authorize the producer map at the exact validated commit.

    Nothing in the ledger row may self-report the map.  Each blob id is derived
    by git from ``sha:path`` and the complete map is then submitted to the one
    semantic verifier.  Crossed primary/candidate maps therefore fail exactly
    like any other unregistered map.
    """
    if _SHA40.fullmatch(sha) is None:
        raise ProducerDefinitionError("validated commit is not 40 lowercase hex")
    candidates = producer_definition_repositories(row, repositories)
    # Separate "my checkout cannot hold this commit" from "the map is not one
    # currently allowed" BEFORE attempting resolution, because the verifier
    # reports both as rc 1 and the caller cannot then tell them apart. The
    # identity primitive is the one named in policy: resolve the object itself.
    holders = [
        checkout for checkout in candidates if _checkout_holds_commit(checkout, sha)
    ]
    if not holders:
        raise ProducerDefinitionUndereferenceable(
            f"cannot dereference the producer definition for {sha}: no candidate "
            "checkout on this host holds that commit, so the producing check "
            "definition could not be read AT ALL. This is a defect in THIS "
            "CHECKOUT, not in the receipt -- the receipt has not been judged. "
            "Most often an uninitialized submodule: run `git submodule update "
            "--init --recursive` and retry. Candidates inspected: "
            + (
                "; ".join(
                    f"{checkout} ({_checkout_defect(checkout)})"
                    for checkout in candidates
                )
                or "(none existed)"
            )
        )
    attempted: list[str] = []
    failures: list[str] = []
    for checkout in holders:
        attempted.append(str(checkout))
        result = _run_producer_definition_verifier(
            "--producer-definition-resolve",
            "--repo",
            producer_definition_repository(row),
            "--sha",
            sha,
            "--repo-checkout",
            str(checkout),
        )
        if result.returncode == 0:
            producer = _decoded_producer_definition(result.stdout, checkout=checkout)
            valid_commits = producer.get("valid_commits")
            if valid_commits is not None and (
                sha not in valid_commits
            ):
                raise RuntimeError(
                    "producer-definition verifier returned an invalid commit bound"
                )
            return producer
        if result.returncode == 1:
            failures.append(
                f"{checkout}: exact producer definition at {sha} is unresolvable "
                "or not one currently allowed whole map"
            )
            continue
        raise RuntimeError(
            "producer-definition verifier could not resolve the commit map: "
            f"rc={result.returncode}: {result.stderr.strip()}"
        )
    detail = "; ".join(failures) if failures else "no candidate repository existed"
    raise ProducerDefinitionError(
        f"cannot resolve the producing check definition for {sha} in any repository "
        f"that durably holds it. Tried: {', '.join(attempted) or '(none)'}. {detail}"
    )


def observe_producer_definition(
    row: dict[str, Any],
    sha: str,
    *,
    repositories: list[str | Path] | None = None,
) -> dict[str, Any]:
    """Derive current producer blobs without making their registration authoritative.

    Hermit's path set comes from the checked registry shape; Reverie uses its
    validate.sh entrypoint. Exact blob hashes come from the commit being
    published. The result deliberately reports
    ``legacy-selected-paths``: the registry's former ``complete`` claim is not
    reusable when the map omits modules compiled by the validation entrypoint.
    This keeps useful producer provenance in an immutable receipt without
    reviving the retired rule that every validator edit needs a second
    repository blessing.
    """
    if _SHA40.fullmatch(sha) is None:
        raise ProducerDefinitionError("validated commit is not 40 lowercase hex")
    candidates = producer_definition_repositories(row, repositories)
    holders = [
        checkout for checkout in candidates if _checkout_holds_commit(checkout, sha)
    ]
    if not holders:
        raise ProducerDefinitionUndereferenceable(
            f"cannot dereference the producer definition for {sha}: no candidate "
            "checkout on this host holds that commit"
        )
    attempted: list[str] = []
    for checkout in holders:
        attempted.append(str(checkout))
        result = _run_producer_definition_verifier(
            "--producer-definition-observe",
            "--repo",
            producer_definition_repository(row),
            "--sha",
            sha,
            "--repo-checkout",
            str(checkout),
        )
        if result.returncode == 0:
            producer = _decoded_producer_definition(result.stdout, checkout=checkout)
            if "valid_commits" in producer:
                raise RuntimeError(
                    "observed producer definition unexpectedly carries a commit allowlist"
                )
            return producer
        if result.returncode != 1:
            raise RuntimeError(
                "producer-definition verifier could not observe the commit map: "
                f"rc={result.returncode}: {result.stderr.strip()}"
            )
    raise ProducerDefinitionError(
        f"cannot observe the producing check definition for {sha}. Tried: "
        f"{', '.join(attempted) or '(none)'}"
    )


# The green CLASS (hard vs inherited/soft) is derived from the row's provenance by
# ci-hub/validate/green_class.py. It is imported rather than restated: a second copy
# of the derivation is exactly the drift this shared predicate exists to remove.
sys.path.insert(0, str(Path(__file__).resolve().parent / "validate"))
import green_class as _green_class  # noqa: E402

def _capture(command, **kwargs):
    """Deprecated local alias for lib/bounded_capture.capture.

    Kept so this file's existing call sites read unchanged. The implementation
    moved to ci-hub/lib/bounded_capture.py because the same helper had been
    written independently in three files, and three copies of a subtle fix
    drift -- this copy had already drifted, having grown `check=` support that
    the other two lacked. The shared helper implements `check` too, so nothing
    is lost here.

    Capture output while waiting for THE CHILD, not for EOF on the pipe:
    `capture_output=True` reads a pipe to EOF, and EOF arrives when the last
    holder closes it, not when the child exits.
    """
    return bounded_capture.capture(command, **kwargs)


def predicate_path() -> Path:
    """On-disk location of the canonical predicate (beside this module)."""
    return Path(__file__).resolve().parent / PREDICATE_REL


def load() -> dict[str, Any]:
    """Load the predicate honouring the env override, then the on-disk file.

    A malformed / unreadable source raises RuntimeError -- a landing consumer
    must not run on a guessed predicate.
    """
    override = os.environ.get(PREDICATE_ENV)
    path = Path(override) if override else predicate_path()
    try:
        text = path.read_text()
    except OSError as error:
        raise RuntimeError(f"{path}: cannot read qualifying predicate: {error}")
    try:
        pred = json.loads(text)
    except ValueError as error:
        raise RuntimeError(f"{path}: malformed qualifying predicate: {error}")
    # Fail loud if a required key is missing -- a partial predicate would let a
    # consumer become quietly more lenient than its peers.
    for key in (
        "counts_schema",
        "require",
        "passed_tests",
        "count_representation",
        "coverage",
        "cell_results",
        "base",
        "admission",
    ):
        if key not in pred:
            raise RuntimeError(f"{path}: qualifying predicate missing key '{key}'")
    for key in (
        "commit_anchored",
        "tree_dirty",
        "profile",
        "selection_mode",
        "result",
        "failures_max",
        "executed_tests_min",
    ):
        if key not in pred["require"]:
            raise RuntimeError(f"{path}: qualifying predicate missing require.{key}")
    for key in ("applies_at_schema_min", "per_node"):
        if key not in pred["coverage"]:
            raise RuntimeError(f"{path}: qualifying predicate missing coverage.{key}")
    if "applies_at_schema_min" not in pred["passed_tests"]:
        raise RuntimeError(
            f"{path}: qualifying predicate missing passed_tests.applies_at_schema_min"
        )
    if pred["count_representation"] != COUNT_REPRESENTATION:
        raise RuntimeError(
            f"{path}: qualifying predicate count_representation must be "
            f"{COUNT_REPRESENTATION!r}"
        )
    for key in ("applies_at_schema_min", "supported_schema_max"):
        if key not in pred["cell_results"]:
            raise RuntimeError(f"{path}: qualifying predicate missing cell_results.{key}")
    for key in ("applies_at_schema_min", "branch"):
        if key not in pred["base"]:
            raise RuntimeError(f"{path}: qualifying predicate missing base.{key}")
    for key in (
        "applies_at_schema_min",
        "required_admission",
        "required_concurrent_validates",
        "required_concurrency_proof",
        "require_registered_producer",
    ):
        if key not in pred["admission"]:
            raise RuntimeError(f"{path}: qualifying predicate missing admission.{key}")
    return pred


def active() -> dict[str, Any]:
    """Process-wide cached predicate. Resolved once; re-read only across runs."""
    global _ACTIVE
    if _ACTIVE is None:
        _ACTIVE = load()
    return _ACTIVE


class CoverageVerdict(Enum):
    """Typed outcome of the per-node coverage obligation."""

    SATISFIED = "satisfied"
    UNSATISFIED = "unsatisfied"
    UNAVAILABLE = "unavailable"


def coverage_verdict(cov: Any) -> CoverageVerdict:
    """Decide coverage from the receipt fields alone.

    Both failure lists must be present as lists. An omitted or malformed list is
    UNKNOWN (`UNAVAILABLE`), never an empty-list success. A reported nonempty
    list is a distinct `UNSATISFIED` outcome. Mirrors the Rust
    `coverage_verdict` authority.
    """
    if not isinstance(cov, dict):
        return CoverageVerdict.UNAVAILABLE
    planned = cov.get("planned_test_nodes")
    executed = cov.get("executed_test_nodes")
    zero_executed = cov.get("zero_executed_nodes")
    absent = cov.get("absent_nodes")
    if (
        not isinstance(planned, int)
        or isinstance(planned, bool)
        or not 0 < planned <= U64_MAX
        # Rust's CoverageRow defaults an omitted diagnostic count to zero, but
        # serde refuses a present value that cannot be represented as u64.
        or (
            "executed_test_nodes" in cov
            and (
                not isinstance(executed, int)
                or isinstance(executed, bool)
                or not 0 <= executed <= U64_MAX
            )
        )
        or not isinstance(zero_executed, list)
        or not isinstance(absent, list)
        or any(not isinstance(name, str) for name in zero_executed)
        or any(not isinstance(name, str) for name in absent)
    ):
        return CoverageVerdict.UNAVAILABLE
    if zero_executed or absent:
        return CoverageVerdict.UNSATISFIED
    return CoverageVerdict.SATISFIED


def coverage_satisfied(cov: Any) -> bool:
    """True only for an explicitly reported, satisfied coverage obligation."""
    return coverage_verdict(cov) is CoverageVerdict.SATISFIED


class CoverageSchemaVerdict(Enum):
    """Whether a row claims coverage permitted by its schema.

    Schema-4 and older rows predate per-node coverage. They remain
    grandfathered only when coverage is absent or null; carrying a non-null
    object is an internally contradictory claim and is refused. Schema-5+
    activates the coverage contract, whose object contents are checked by
    :func:`coverage_verdict`.
    """

    SATISFIED = "satisfied"
    GRANDFATHERED_UNKNOWN = "grandfathered-unknown"
    SCHEMA_UNAVAILABLE = "schema-unavailable"
    POLICY_UNAVAILABLE = "policy-unavailable"
    LEGACY_CLAIMS_COVERAGE = "legacy-schema-claims-coverage"


def coverage_schema_verdict(
    row: dict[str, Any], pred: dict[str, Any]
) -> CoverageSchemaVerdict:
    """Enforce the version boundary for a receipt-carried coverage claim."""
    clause = pred.get("coverage")
    if not isinstance(clause, dict):
        return CoverageSchemaVerdict.POLICY_UNAVAILABLE
    per_node = clause.get("per_node")
    schema_min = clause.get("applies_at_schema_min")
    if not isinstance(per_node, bool):
        return CoverageSchemaVerdict.POLICY_UNAVAILABLE
    if not per_node:
        return CoverageSchemaVerdict.SATISFIED
    if (
        not isinstance(schema_min, int)
        or isinstance(schema_min, bool)
        or schema_min < 0
    ):
        return CoverageSchemaVerdict.POLICY_UNAVAILABLE
    schema = row.get("schema_version")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema < 0:
        return CoverageSchemaVerdict.SCHEMA_UNAVAILABLE
    if schema < schema_min:
        if row.get("coverage") is not None:
            return CoverageSchemaVerdict.LEGACY_CLAIMS_COVERAGE
        return CoverageSchemaVerdict.GRANDFATHERED_UNKNOWN
    return CoverageSchemaVerdict.SATISFIED


def coverage_schema_accepted(verdict: CoverageSchemaVerdict) -> bool:
    """Whether the schema/coverage combination is internally consistent."""
    return verdict in (
        CoverageSchemaVerdict.SATISFIED,
        CoverageSchemaVerdict.GRANDFATHERED_UNKNOWN,
    )


class CellResultsVerdict(Enum):
    """Typed outcome of the receipt-bound selected-cell evidence obligation."""

    SATISFIED = "satisfied"
    GRANDFATHERED_UNKNOWN = "grandfathered-unknown"
    SCHEMA_UNAVAILABLE = "schema-unavailable"
    POLICY_UNAVAILABLE = "policy-unavailable"
    LEGACY_CLAIMS_CELL_RESULTS = "legacy-schema-claims-cell-results"
    MISSING = "missing"
    MALFORMED = "malformed"
    EMPTY = "empty"
    RUN_MISMATCH = "run-mismatch"
    WRONG_SHA = "wrong-sha"
    DIRTY_SOURCE = "dirty-source"
    POPULATION_MISMATCH = "population-mismatch"
    VERDICT_INVALID = "cell-verdict-invalid"


_CELL_IDENTITY_KEYS = ("lane", "category", "test", "mode", "backend")
_COMPARISON_KEYS = {
    "strictness",
    "display_name",
    "compare_logs",
    "compare_io_buffers",
    "log_scope",
    "record_envelope",
    "virtualize_time",
    "strip_lines",
    "canonicalize_addresses",
    "full_trace",
    "exact_remainder",
    "stripped_prefixes",
    "canonicalizations",
    "ignore_lines",
    "skip_commit",
    "skip_detlog",
}
_COMPARISON_DISPLAY_NAMES = {
    ("stripped", "deterministic"): "Stripped",
    ("canonical", "info"): "BitwiseInfoV1",
    ("canonical", "full_trace"): "BitwiseFullTraceV1",
}
_RECORD_ENVELOPES = {
    "all_records_v1",
    "dbt_evidence_transport_v1",
    "caller_defined",
}
_PARITY_STRIPPED_PREFIXES = ["real-wall-clock-prefix/v1"]
_PARITY_CANONICALIZATIONS = ["host-address-to-first-appearance-ordinal/v1"]
_COMPARED_STATES = {"compared-and-matched", "compared-and-diverged"}
_REASONED_STATES = {
    "performs-no-comparison-by-design",
    "unavailable-with-reason",
}
_KNOWN_COMPARISON_TIERS = {
    "canonical-bitwise",
    "exit-and-stream-equality",
    "execution-only-self-consistent",
    "declared-but-unverifiable",
}


def _nonblank(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _u64(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= U64_MAX


def _i64(value: Any) -> bool:
    return (
        isinstance(value, int)
        and not isinstance(value, bool)
        and I64_MIN <= value <= I64_MAX
    )


def _sha256(value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _cell_identity(value: Any) -> tuple[str, ...] | None:
    if not isinstance(value, dict) or set(value) != set(_CELL_IDENTITY_KEYS):
        return None
    parts = tuple(value[key] for key in _CELL_IDENTITY_KEYS)
    if not all(_nonblank(part) for part in parts):
        return None
    return parts


def cell_population_sha256(selected: list[dict[str, Any]]) -> str:
    """Digest the sorted selected identities using one canonical JSON shape."""
    ordered = sorted(selected, key=lambda item: tuple(item[key] for key in _CELL_IDENTITY_KEYS))
    body = json.dumps(ordered, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(body).hexdigest()


def _comparison_spec_valid(value: Any) -> bool:
    if not isinstance(value, dict) or set(value) != _COMPARISON_KEYS:
        return False
    if value["strictness"] not in {"canonical", "stripped"}:
        return False
    if value["log_scope"] not in {"deterministic", "info", "full_trace"}:
        return False
    if value.get("display_name") != _COMPARISON_DISPLAY_NAMES.get(
        (value["strictness"], value["log_scope"])
    ):
        return False
    if value.get("record_envelope") not in _RECORD_ENVELOPES:
        return False
    for key in (
        "compare_logs",
        "compare_io_buffers",
        "virtualize_time",
        "strip_lines",
        "canonicalize_addresses",
        "full_trace",
        "exact_remainder",
        "ignore_lines",
        "skip_commit",
        "skip_detlog",
    ):
        if not isinstance(value[key], bool):
            return False
    for key in ("stripped_prefixes", "canonicalizations"):
        if not isinstance(value[key], list) or not all(_nonblank(item) for item in value[key]):
            return False
    return True


def cell_verdict_valid(value: Any, mode: Any) -> bool:
    """Validate one of the four closed states without consulting process exit."""
    if not isinstance(value, dict):
        return False
    if mode not in ("verify", "replay", "chaos", "naked", "custom"):
        return False
    state = value.get("state")
    if state in _REASONED_STATES:
        return (
            set(value) == {"state", "comparison_tier", "reason"}
            and value.get("comparison_tier") == "declared-but-unverifiable"
            and _nonblank(value.get("reason"))
        )
    if state not in _COMPARED_STATES:
        return False
    if set(value) != {
        "state",
        "comparison_tier",
        "comparison",
        "bitwise_parity",
        "compared_log_messages",
    }:
        return False
    if value.get("comparison_tier") not in _KNOWN_COMPARISON_TIERS:
        return False
    tier = value["comparison_tier"]
    if tier == "declared-but-unverifiable":
        return False
    comparison = value.get("comparison")
    if not _comparison_spec_valid(comparison) or not isinstance(value.get("bitwise_parity"), bool):
        return False
    counts = value.get("compared_log_messages")
    if counts is not None:
        if (
            not isinstance(counts, dict)
            or set(counts) != {"left", "right"}
            or not _u64(counts.get("left"))
            or not _u64(counts.get("right"))
        ):
            return False
    if comparison["compare_logs"] and counts is None:
        return False
    if state == "compared-and-diverged" and value["bitwise_parity"]:
        return False
    expected_virtualize_time = {
        "replay": False,
        "verify": True,
        "chaos": True,
    }.get(mode)
    canonical_evidence_shape = (
        expected_virtualize_time is not None
        and comparison["strictness"] == "canonical"
        and comparison["display_name"] == "BitwiseInfoV1"
        and comparison["compare_logs"] is True
        and comparison["compare_io_buffers"] is True
        and comparison["log_scope"] == "info"
        and comparison["record_envelope"] == "all_records_v1"
        and comparison["virtualize_time"] is expected_virtualize_time
        and comparison["strip_lines"] is False
        and comparison["canonicalize_addresses"] is True
        and comparison["full_trace"] is True
        and comparison["exact_remainder"] is True
        and comparison["stripped_prefixes"] == _PARITY_STRIPPED_PREFIXES
        and comparison["canonicalizations"] == _PARITY_CANONICALIZATIONS
        and comparison["ignore_lines"] is False
        and comparison["skip_commit"] is False
        and comparison["skip_detlog"] is False
        and counts is not None
        and counts["left"] > 0
        and counts["right"] > 0
    )
    if tier == "canonical-bitwise":
        return canonical_evidence_shape and value["bitwise_parity"] == (
            state == "compared-and-matched"
        )
    # WITHOUT CONVERTING EITHER WEAK TIER INTO CANONICAL COMPARISON EVIDENCE.
    return value["bitwise_parity"] is False and not canonical_evidence_shape


def cell_results_verdict(
    row: dict[str, Any], pred: dict[str, Any]
) -> CellResultsVerdict:
    """Require complete evidence at supported schemas; older rows make no claim."""
    clause = pred.get("cell_results")
    if not isinstance(clause, dict):
        return CellResultsVerdict.POLICY_UNAVAILABLE
    schema_min = clause.get("applies_at_schema_min")
    schema_max = clause.get("supported_schema_max")
    if (
        not isinstance(schema_min, int)
        or isinstance(schema_min, bool)
        or schema_min < 0
        or not isinstance(schema_max, int)
        or isinstance(schema_max, bool)
        or schema_max < schema_min
    ):
        return CellResultsVerdict.POLICY_UNAVAILABLE
    schema = row.get("schema_version")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema < 0:
        return CellResultsVerdict.SCHEMA_UNAVAILABLE
    evidence = row.get("cell_results")
    if schema < schema_min:
        return (
            CellResultsVerdict.GRANDFATHERED_UNKNOWN
            if evidence is None
            else CellResultsVerdict.LEGACY_CLAIMS_CELL_RESULTS
        )
    if schema > schema_max:
        return CellResultsVerdict.SCHEMA_UNAVAILABLE
    if evidence is None:
        return CellResultsVerdict.MISSING
    if not isinstance(evidence, dict) or set(evidence) != {
        "run_id",
        "hermit_sha",
        "source_tree_dirty",
        "selected_count",
        "recorded_count",
        "population_sha256",
        "artifact",
        "selected",
        "cells",
    }:
        return CellResultsVerdict.MALFORMED
    if (
        not _nonblank(row.get("run_id"))
        or not _nonblank(evidence.get("run_id"))
        or evidence["run_id"] != row["run_id"]
    ):
        return CellResultsVerdict.RUN_MISMATCH
    if evidence.get("hermit_sha") != row.get("commit"):
        return CellResultsVerdict.WRONG_SHA
    if evidence.get("source_tree_dirty") is not False:
        return CellResultsVerdict.DIRTY_SOURCE
    selected_count = evidence.get("selected_count")
    recorded_count = evidence.get("recorded_count")
    selected = evidence.get("selected")
    cells = evidence.get("cells")
    if (
        not _u64(selected_count)
        or not _u64(recorded_count)
        or selected_count == 0
        or recorded_count == 0
        or not isinstance(selected, list)
        or not isinstance(cells, list)
    ):
        return CellResultsVerdict.EMPTY
    artifact = evidence.get("artifact")
    if (
        not isinstance(artifact, dict)
        or set(artifact) != {"path", "sha256", "row_count"}
        or not _nonblank(artifact.get("path"))
        or not _sha256(artifact.get("sha256"))
        or artifact.get("row_count") != recorded_count
    ):
        return CellResultsVerdict.MALFORMED
    identities = [_cell_identity(item) for item in selected]
    cell_identities = []
    for cell in cells:
        if not isinstance(cell, dict) or set(cell) != set(_CELL_IDENTITY_KEYS) | {"cell_verdict"}:
            return CellResultsVerdict.MALFORMED
        identity = _cell_identity({key: cell.get(key) for key in _CELL_IDENTITY_KEYS})
        if identity is None or not cell_verdict_valid(
            cell.get("cell_verdict"), cell.get("mode")
        ):
            return CellResultsVerdict.VERDICT_INVALID
        cell_identities.append(identity)
    if any(identity is None for identity in identities):
        return CellResultsVerdict.MALFORMED
    if (
        len(selected) != selected_count
        or len(cells) != recorded_count
        or len(set(identities)) != len(identities)
        or len(set(cell_identities)) != len(cell_identities)
        or sorted(identities) != sorted(cell_identities)
        or not _sha256(evidence.get("population_sha256"))
        or cell_population_sha256(selected) != evidence["population_sha256"]
    ):
        return CellResultsVerdict.POPULATION_MISMATCH
    return CellResultsVerdict.SATISFIED


def cell_results_accepted(verdict: CellResultsVerdict) -> bool:
    return verdict in (
        CellResultsVerdict.SATISFIED,
        CellResultsVerdict.GRANDFATHERED_UNKNOWN,
    )


_SCORECARD_GREEN_TIERS = {
    "canonical-bitwise",
}


def cell_scorecard_green(cell: Any) -> bool:
    """Project one admitted cell without treating process success as parity."""
    if not isinstance(cell, dict) or not cell_verdict_valid(
        cell.get("cell_verdict"), cell.get("mode")
    ):
        return False
    verdict = cell["cell_verdict"]
    return (
        verdict["state"] == "compared-and-matched"
        and verdict["comparison_tier"] in _SCORECARD_GREEN_TIERS
        and verdict["bitwise_parity"] is True
    )


def cell_scorecard_projection(
    row: dict[str, Any], pred: dict[str, Any]
) -> dict[str, Any]:
    """Expose per-cell eligibility separately from whole-run acceptance.

    Historical whole-run rows deliberately return ``cells: null`` and
    ``green_count: null``. An empty list would let an unavailable historical
    population masquerade as a measured population containing zero greens.
    """
    status = cell_results_verdict(row, pred)
    if status is not CellResultsVerdict.SATISFIED:
        return {
            "eligible": False,
            "cell_results_satisfied": (
                None if status is CellResultsVerdict.GRANDFATHERED_UNKNOWN else False
            ),
            "status": status.value,
            "green_count": None,
            "cells": None,
        }
    cells = [
        {
            **{key: cell[key] for key in _CELL_IDENTITY_KEYS},
            "state": cell["cell_verdict"]["state"],
            "comparison_tier": cell["cell_verdict"]["comparison_tier"],
            "scorecard_green": cell_scorecard_green(cell),
        }
        for cell in row["cell_results"]["cells"]
    ]
    return {
        "eligible": True,
        "cell_results_satisfied": True,
        "status": status.value,
        "green_count": sum(cell["scorecard_green"] for cell in cells),
        "cells": cells,
    }


class AdmissionVerdict(Enum):
    """Typed result from the one admission-provenance verifier.

    Schema-4 receipts predate the obligation, so their result is unknown rather
    than false. Schema-5+ receipts must carry every exact condition; missing and
    malformed values are refusals, never defaults.
    """

    SATISFIED = "satisfied"
    GRANDFATHERED_UNKNOWN = "grandfathered-unknown"
    SCHEMA_UNAVAILABLE = "schema-unavailable"
    PRODUCER_MISSING = "producer-missing"
    PRODUCER_UNKNOWN = "producer-unknown"
    ADMISSION_MISSING = "admission-missing"
    ADMISSION_NONCANONICAL = "admission-noncanonical"
    CONCURRENCY_MISSING = "concurrent-validates-missing"
    CONCURRENCY_INVALID = "concurrent-validates-invalid"
    OWNER_ANCESTRY_MISSING = "owner-ancestry-missing"
    OWNER_ANCESTRY_UNPROVEN = "owner-ancestry-unproven"
    POLICY_UNAVAILABLE = "policy-unavailable"


class BaseVerdict(Enum):
    """Typed outcome of recorded base evidence and final-boundary currency."""

    SATISFIED = "satisfied"
    GRANDFATHERED_UNKNOWN = "grandfathered-unknown"
    SCHEMA_UNAVAILABLE = "schema-unavailable"
    BASE_SHA_MISSING = "base-sha-missing"
    BASE_SHA_INVALID = "base-sha-invalid"
    BASE_TREE_MISSING = "base-tree-missing"
    BASE_TREE_INVALID = "base-tree-invalid"
    REVERIE_BASE_MISSING = "reverie-base-sha-missing"
    REVERIE_BASE_INVALID = "reverie-base-sha-invalid"
    REVERIE_TREE_MISSING = "reverie-base-tree-missing"
    REVERIE_TREE_INVALID = "reverie-base-tree-invalid"
    BASE_NOT_CURRENT = "base-not-current"
    BASE_TOO_STALE = "base-too-stale"
    BASE_NOT_CONTAINED = "base-not-contained"
    BASE_TREE_MISMATCH = "base-tree-mismatch"
    REVERIE_BASE_NOT_CURRENT = "reverie-base-not-current"
    REVERIE_TREE_MISMATCH = "reverie-base-tree-mismatch"
    POLICY_UNAVAILABLE = "policy-unavailable"


_SHA40 = re.compile(r"[0-9a-f]{40}")
def base_evidence_verdict(row: dict[str, Any], pred: dict[str, Any]) -> BaseVerdict:
    """Require exact Hermit and Reverie base identities on schema-5+ rows.

    This is deliberately separate from the moving-tip comparison: qualification
    proves the receipt *carries* the evidence, while :func:`base_boundary_verdict`
    dereferences it once at the final merge boundary.
    """
    clause = pred.get("base")
    if not isinstance(clause, dict):
        return BaseVerdict.POLICY_UNAVAILABLE
    schema_min = clause.get("applies_at_schema_min")
    if not isinstance(schema_min, int) or isinstance(schema_min, bool) or schema_min < 0:
        return BaseVerdict.POLICY_UNAVAILABLE
    schema = row.get("schema_version")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema < 0:
        return BaseVerdict.SCHEMA_UNAVAILABLE
    if schema < schema_min:
        return BaseVerdict.GRANDFATHERED_UNKNOWN

    base_sha = row.get("base_sha")
    if not isinstance(base_sha, str) or not base_sha:
        return BaseVerdict.BASE_SHA_MISSING
    if _SHA40.fullmatch(base_sha) is None:
        return BaseVerdict.BASE_SHA_INVALID
    base_tree = row.get("base_tree")
    if not isinstance(base_tree, str) or not base_tree:
        return BaseVerdict.BASE_TREE_MISSING
    if _SHA40.fullmatch(base_tree) is None:
        return BaseVerdict.BASE_TREE_INVALID
    reverie_base = row.get("reverie_base_sha")
    if not isinstance(reverie_base, str) or not reverie_base:
        return BaseVerdict.REVERIE_BASE_MISSING
    if _SHA40.fullmatch(reverie_base) is None:
        return BaseVerdict.REVERIE_BASE_INVALID
    reverie_tree = row.get("reverie_base_tree")
    if not isinstance(reverie_tree, str) or not reverie_tree:
        return BaseVerdict.REVERIE_TREE_MISSING
    if _SHA40.fullmatch(reverie_tree) is None:
        return BaseVerdict.REVERIE_TREE_INVALID
    return BaseVerdict.SATISFIED


def base_accepted(verdict: BaseVerdict) -> bool:
    return verdict in (BaseVerdict.SATISFIED, BaseVerdict.GRANDFATHERED_UNKNOWN)


def _git_output(checkout: str, *args: str) -> str:
    result = _capture(["git", "-C", checkout, *args])
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"git -C {checkout} {' '.join(args)}: {detail}")
    return result.stdout


def git_tree(checkout: str, sha: str) -> str:
    value = _git_output(checkout, "rev-parse", "--verify", f"{sha}^{{tree}}").strip()
    if _SHA40.fullmatch(value) is None:
        raise RuntimeError(f"{checkout}: {sha} has no exact tree identity")
    return value


# Default lag bound, reused from `scripts/check-gitlink-currency.sh`'s FAIL
# threshold rather than invented here. That file derived it by REVIEWABILITY:
# past this many commits "a bump stops being reviewable and becomes a leap of
# faith". Like its source this is calibration, not law; re-derive the churn
# before trusting it. `0` disables the bound entirely, which restores the pure
# ancestry rule for anyone who wants it.
RECEIPT_BASE_MAX_LAG_COMMITS_DEFAULT = 200


def _receipt_base_max_lag_commits() -> "int | None":
    """The bound, or None when explicitly disabled. A malformed value is fatal.

    A silently ignored override is how a gate ends up measuring something other
    than what its operator asked for, so an unparseable value raises rather than
    falling back to the default.
    """
    raw = os.environ.get("RECEIPT_BASE_MAX_LAG_COMMITS")
    if raw is None or raw.strip() == "":
        return RECEIPT_BASE_MAX_LAG_COMMITS_DEFAULT
    try:
        value = int(raw.strip())
    except ValueError:
        raise RuntimeError(
            f"RECEIPT_BASE_MAX_LAG_COMMITS is not an integer: {raw!r}"
        ) from None
    if value < 0:
        raise RuntimeError(
            f"RECEIPT_BASE_MAX_LAG_COMMITS must not be negative: {value}"
        )
    return None if value == 0 else value


def git_commit_count(checkout: str, start: str, end: str) -> int:
    """How many commits `end` has that `start` does not."""
    result = _capture(
        ["git", "-C", checkout, "rev-list", "--count", f"{start}..{end}"],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise RuntimeError(
            f"git -C {checkout} rev-list --count {start}..{end}: {detail}"
        )
    try:
        return int(result.stdout.strip())
    except ValueError:
        raise RuntimeError(
            f"git -C {checkout} rev-list --count {start}..{end}: "
            f"non-numeric output {result.stdout.strip()!r}"
        ) from None


def git_is_ancestor(checkout: str, ancestor: str, descendant: str) -> bool:
    result = _capture(
        ["git", "-C", checkout, "merge-base", "--is-ancestor", ancestor, descendant],
        check=False,
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = result.stderr.strip() or f"exit {result.returncode}"
    raise RuntimeError(
        f"git -C {checkout} merge-base --is-ancestor {ancestor} {descendant}: {detail}"
    )


def base_boundary_verdict(
    row: dict[str, Any],
    pred: dict[str, Any],
    *,
    current_base: str,
    current_reverie_base: str,
    repo_checkout: str,
    reverie_checkout: str,
) -> BaseVerdict:
    """Assert final-boundary base identity and Reverie ancestry before merge."""
    static = base_evidence_verdict(row, pred)
    if static is not BaseVerdict.SATISFIED:
        return static
    if _SHA40.fullmatch(current_base) is None or _SHA40.fullmatch(current_reverie_base) is None:
        return BaseVerdict.POLICY_UNAVAILABLE
    recorded_base = row["base_sha"]
    try:
        # The receipt's base names the historical commit against which the run
        # was admitted.  Preserve that evidence by checking its tree at that
        # exact commit; a later main tip must not rewrite what the receipt says.
        if row["base_tree"] != git_tree(repo_checkout, recorded_base):
            return BaseVerdict.BASE_TREE_MISMATCH
        receipt_commit = row.get("commit")
        if not isinstance(receipt_commit, str) or _SHA40.fullmatch(receipt_commit) is None:
            return BaseVerdict.BASE_NOT_CONTAINED
        if not git_is_ancestor(repo_checkout, recorded_base, receipt_commit):
            return BaseVerdict.BASE_NOT_CONTAINED
        # CURRENCY IS ANCESTRY, NOT CONTAINMENT OF THE TIP.  The recorded base
        # must be real `main` history.  The validated commit need NOT contain
        # whatever tip `main` has reached by the time this runs.
        #
        # The rule is stated once, with its reasoning, in Hermit's
        # `scripts/check-reverie-pin.rs` OWNER-APPROVED RULE block (2026-08-08,
        # replacing equality-to-the-tip).  Read it there rather than relying on
        # a restatement here.  The Reverie leg below already cites that block
        # and already asks this question in this direction; until now the two
        # legs of this one function applied OPPOSITE rules, which is the defect
        # this closes.
        #
        # Requiring the tip made the verdict a property of WHEN it was checked.
        # `main` advances continuously, so a receipt taken minutes earlier
        # failed for a reason having nothing to do with the tree it measured,
        # and landing serialised behind re-validation that proved nothing new.
        # Admission is a SEPARATE question with the opposite answer, and it is
        # already enforced elsewhere: `scripts/validate.rs` refuses to START a
        # run whose head is behind `origin/main`, because a run must measure a
        # commit containing everything available when it began.  Nothing here
        # weakens that; this decides only whether an existing receipt still
        # speaks for its tree at the landing boundary.
        if not git_is_ancestor(repo_checkout, recorded_base, current_base):
            return BaseVerdict.BASE_NOT_CURRENT
        # ⚠️ ANCESTRY ALONE IS NOT THE RULE, AND STOPPING HERE WOULD BE HALF OF IT.
        #
        # Clause 2 of the block cited above says the pin "may only advance",
        # because "ancestry ALONE would accept a pin walked backwards, because an
        # ancient commit is also an ancestor".  The same gap exists here in the
        # forward direction: every base ever merged stays an ancestor forever, so
        # ancestry admits a receipt of ANY age.  Measured on a fixture while this
        # was being written: recorded base at the repository ROOT, head 58 commits
        # behind, verdict `satisfied`.
        #
        # This project has already applied the same owner ruling to a non-pin case
        # and reached the same conclusion independently.  `scripts/check-gitlink-currency.sh`
        # records it: "ancestry ALONE is what permitted 374 commits. So the rule is
        # ancestry PLUS a bounded lag."  Its FAIL threshold of 200 commits is
        # borrowed here for its ORDER OF MAGNITUDE, and the borrowing is partial on
        # purpose:
        #
        #   ⚠️ ITS STATED RATIONALE DOES NOT TRANSFER.  That bound was calibrated by
        #   REVIEWABILITY -- past 200 "a bump stops being reviewable and becomes a
        #   leap of faith", because a human reviewer must be able to say what they
        #   are accepting.  THERE IS NO REVIEWER AT THIS BOUNDARY; the consumer is an
        #   automated gate.  The hazard here is instead "did `main` change something
        #   this receipt's evidence depended on", and a commit count is a BLUNT PROXY
        #   for that rather than a measurement of it.
        #
        # A sharper measure was looked for and is not cheaply available.  Wall-clock
        # receipt age is no less arbitrary and is gameable.  Intersecting the pull
        # request's paths with `recorded_base..current_base` covers only the TEXTUAL
        # half, which the conflict graph and a rebase merge already catch; the
        # semantic half is not measurable without re-running the tests, which is the
        # very thing being lifted.  So: keep the axis, borrow the magnitude, and do
        # not pretend the original reason carries over.
        #
        # Like its source this is CALIBRATION, NOT LAW -- re-derive the churn before
        # trusting it, and override with RECEIPT_BASE_MAX_LAG_COMMITS.
        #
        # This is deliberately far looser than the tip-equality gate it replaces.
        # Tip equality was unsatisfiable because `main` moves continuously; 200
        # commits of lag is roughly two days of this repository's throughput and
        # refuses only the receipts nobody could review the delta of.
        lag_bound = _receipt_base_max_lag_commits()
        if lag_bound is not None:
            lag = git_commit_count(repo_checkout, recorded_base, current_base)
            if lag > lag_bound:
                return BaseVerdict.BASE_TOO_STALE
    except RuntimeError:
        return BaseVerdict.POLICY_UNAVAILABLE
    try:
        # Match Hermit's owner-approved scripts/check-reverie-pin.rs policy:
        # a pin remains valid while it is on Reverie main history. Equality to
        # the moving tip would make the verdict depend on when it is checked.
        if not git_is_ancestor(
            reverie_checkout, row["reverie_base_sha"], current_reverie_base
        ):
            return BaseVerdict.REVERIE_BASE_NOT_CURRENT
        recorded_reverie_tree = git_tree(
            reverie_checkout, row["reverie_base_sha"]
        )
    except RuntimeError:
        return BaseVerdict.POLICY_UNAVAILABLE
    if row["reverie_base_tree"] != recorded_reverie_tree:
        return BaseVerdict.REVERIE_TREE_MISMATCH
    return BaseVerdict.SATISFIED


def admission_verdict(row: dict[str, Any], pred: dict[str, Any]) -> AdmissionVerdict:
    """Verify canonical admission, bounded concurrency, and owner ancestry.

    This is the only semantic implementation of the admission clause. Rust
    sends the row plus the relevant predicate clauses to ``--evidence-only``
    and consumes this typed result; it does not restate these comparisons.
    """
    clause = pred.get("admission")
    if not isinstance(clause, dict):
        return AdmissionVerdict.POLICY_UNAVAILABLE
    schema_min = clause.get("applies_at_schema_min")
    if not isinstance(schema_min, int) or isinstance(schema_min, bool) or schema_min < 0:
        return AdmissionVerdict.POLICY_UNAVAILABLE
    schema = row.get("schema_version")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema < 0:
        return AdmissionVerdict.SCHEMA_UNAVAILABLE
    if schema < schema_min:
        return AdmissionVerdict.GRANDFATHERED_UNKNOWN

    if clause.get("require_registered_producer") is True:
        registered = producer_clause(pred)
        producer = row.get("producer")
        if not isinstance(producer, str) or not producer:
            return AdmissionVerdict.PRODUCER_MISSING
        if producer not in registered["known"]:
            return AdmissionVerdict.PRODUCER_UNKNOWN

    required_admission = clause.get("required_admission")
    admission = row.get("admission")
    if not isinstance(admission, str) or not admission:
        return AdmissionVerdict.ADMISSION_MISSING
    if not isinstance(required_admission, str) or not required_admission:
        return AdmissionVerdict.POLICY_UNAVAILABLE
    if admission != required_admission:
        return AdmissionVerdict.ADMISSION_NONCANONICAL

    required_concurrent = clause.get("required_concurrent_validates")
    maximum_concurrent = clause.get("maximum_concurrent_validates")
    concurrent = row.get("concurrent_validates")
    if concurrent is None:
        return AdmissionVerdict.CONCURRENCY_MISSING
    if (
        not isinstance(required_concurrent, int)
        or isinstance(required_concurrent, bool)
        or required_concurrent < 0
    ):
        return AdmissionVerdict.POLICY_UNAVAILABLE
    if not isinstance(concurrent, int) or isinstance(concurrent, bool):
        return AdmissionVerdict.CONCURRENCY_INVALID
    if maximum_concurrent is None:
        if concurrent != required_concurrent:
            return AdmissionVerdict.CONCURRENCY_INVALID
    elif (
        not isinstance(maximum_concurrent, int)
        or isinstance(maximum_concurrent, bool)
        or maximum_concurrent < required_concurrent
    ):
        return AdmissionVerdict.POLICY_UNAVAILABLE
    elif not required_concurrent <= concurrent <= maximum_concurrent:
        return AdmissionVerdict.CONCURRENCY_INVALID

    required_proof = clause.get("required_concurrency_proof")
    proof = row.get("concurrency_proof")
    if not isinstance(proof, str) or not proof:
        return AdmissionVerdict.OWNER_ANCESTRY_MISSING
    if not isinstance(required_proof, str) or not required_proof:
        return AdmissionVerdict.POLICY_UNAVAILABLE
    accepted_proofs = clause.get("accepted_concurrency_proofs")
    if accepted_proofs is None:
        accepted_proofs = [required_proof]
    if (
        not isinstance(accepted_proofs, list)
        or not accepted_proofs
        or any(not isinstance(value, str) or not value for value in accepted_proofs)
    ):
        return AdmissionVerdict.POLICY_UNAVAILABLE
    if proof not in accepted_proofs:
        return AdmissionVerdict.OWNER_ANCESTRY_UNPROVEN
    return AdmissionVerdict.SATISFIED


def admission_accepted(verdict: AdmissionVerdict) -> bool:
    """Whether the admission clause permits the receipt."""
    return verdict in (
        AdmissionVerdict.SATISFIED,
        AdmissionVerdict.GRANDFATHERED_UNKNOWN,
    )


class ProducerVerdict(Enum):
    """Typed outcome of the `producer` provenance obligation.

    `GRANDFATHERED` deliberately reuses the wording of the grandfathered
    schema-4 coverage rule: the row does not CLAIM a writer and is not asked to,
    so `producer_ok` is null rather than false. It is not a pass.
    """

    OK = "ok"
    GRANDFATHERED = "grandfathered-unknown"
    MISSING = "missing"
    UNKNOWN_WRITER = "unknown-writer"


#: Inert clause used when a predicate (e.g. an older mutation fixture) omits
#: `producer` entirely. Mirrors the Rust `#[serde(default)]`; the canonical
#: on-disk file is separately asserted to DECLARE the key, so the default can
#: never silently disarm the live predicate.
_PRODUCER_INERT: dict[str, Any] = {
    "required": False,
    "applies_from_finished_at": None,
    "known": [],
}

#: `finished_at` is fixed-width RFC3339 UTC (`2026-08-07T20:15:31Z`), a format in
#: which lexicographic order IS chronological order, so the epoch comparison needs
#: no date parsing. Anything not of this exact shape is not ordered against the
#: epoch by guesswork -- see `producer_verdict`.
_RFC3339_Z = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")


def producer_clause(pred: dict[str, Any]) -> dict[str, Any]:
    """The `producer` clause, defaulted inert when the predicate omits it."""
    raw = pred.get("producer")
    if not isinstance(raw, dict):
        return dict(_PRODUCER_INERT)
    known = raw.get("known")
    return {
        "required": bool(raw.get("required", False)),
        "applies_from_finished_at": raw.get("applies_from_finished_at"),
        "known": [w for w in (known or []) if isinstance(w, str)],
    }


def producer_enforced_for(row: dict[str, Any], pred: dict[str, Any]) -> bool:
    """Whether the provenance obligation is LIVE for this particular row.

    Enforcement needs all three: the clause is required, an epoch is declared,
    and the row is at/after it. A row whose `finished_at` is absent or malformed
    while an epoch IS declared is enforced (fail-closed) -- otherwise dropping
    the timestamp would be a trivial way to opt out of the check.
    """
    clause = producer_clause(pred)
    if not clause["required"]:
        return False
    epoch = clause["applies_from_finished_at"]
    if not isinstance(epoch, str) or not epoch:
        return False
    finished = row.get("finished_at")
    if not isinstance(finished, str) or not _RFC3339_Z.fullmatch(finished):
        return True
    return finished >= epoch


def producer_verdict(row: dict[str, Any], pred: dict[str, Any]) -> ProducerVerdict:
    """Decide the provenance obligation from the row and the shared clause.

    A row that NAMES a registered writer is `OK` whether or not the epoch has
    been flipped -- crediting it early means rows written by already-deployed
    writers stop being grandfathered the moment they exist, rather than at
    activation. Mirrors the Rust `producer_verdict` authority.
    """
    clause = producer_clause(pred)
    named = row.get("producer")
    if isinstance(named, str) and named:
        if not clause["known"] or named in clause["known"]:
            return ProducerVerdict.OK
        return ProducerVerdict.UNKNOWN_WRITER
    if producer_enforced_for(row, pred):
        return ProducerVerdict.MISSING
    return ProducerVerdict.GRANDFATHERED


def shared_evidence_reason(
    coverage_schema: CoverageSchemaVerdict,
    base: BaseVerdict | None = None,
    admission: AdmissionVerdict | None = None,
    cell_results: CellResultsVerdict | None = None,
) -> str:
    """Return the first refusal judged by the shared-evidence boundary.

    This is deliberately scoped to the four clauses carried by
    ``--evidence-only``.  Value, producer-epoch, and green-class refusals remain
    ordered by :func:`row_qualification`; this helper must not imply that the
    narrower bridge judged the whole row.
    """
    verdicts = [
        ("coverage schema", coverage_schema, coverage_schema_accepted(coverage_schema))
    ]
    if base is not None:
        verdicts.append(("base", base, base_accepted(base)))
    if admission is not None:
        verdicts.append(("admission", admission, admission_accepted(admission)))
    if cell_results is not None:
        verdicts.append(("cell_results", cell_results, cell_results_accepted(cell_results)))
    for prefix, verdict, accepted in verdicts:
        if not accepted:
            return f"{prefix} {verdict.value}"
    return "qualifies"


def _row_qualification_without_class(
    row: dict[str, Any], sha: str, pred: dict[str, Any]
) -> tuple[bool, str]:
    """THE qualifying-receipt predicate, mirroring the Rust `row_qualifies`
    clause for clause. A count-capable receipt must account for every planned
    outer gate and satisfy per-test-node COVERAGE; ``executed_tests`` remains
    only the zero-execution floor. The reason names the first failing clause so
    downstream consumers do not need a diagnostic copy."""
    req = pred["require"]
    if row.get("commit") in (None, "", "unknown"):
        return False, "no-commit"
    if row.get("commit") != sha:
        return False, f"commit={row.get('commit')!r}"
    if row.get("commit_anchored") is not req["commit_anchored"]:
        return False, "commit_anchored"
    if row.get("tree_dirty") is not req["tree_dirty"]:
        return False, "tree_dirty"
    if row.get("profile") != req["profile"]:
        return False, f"profile={row.get('profile')!r}"
    if row.get("selection_mode") != req["selection_mode"]:
        return False, f"selection_mode={row.get('selection_mode')!r}(1-hop)"
    if row.get("result") != req["result"]:
        return False, f"result={row.get('result')!r}"
    # `pass` with a positive failure count is malformed; an absent count on a
    # pass row is treated as zero (old rows predate the field).
    if (row.get("failures") or 0) > req["failures_max"]:
        return False, f"failures={row.get('failures')}"
    # A demonstrated zero-test run is never a full green, at any schema.
    if row.get("executed_tests") == 0:
        return False, "executed_tests==0"
    schema = row.get("schema_version") or 0
    executed = row.get("executed_tests")
    passed = row.get("passed_tests")
    if schema >= pred["count_representation"]["applies_at_schema_min"]:
        for field in pred["count_representation"]["fields"]:
            value = row.get(field)
            if value is not None and not _i64(value):
                if field == "filtered_tests":
                    return False, FILTERED_TESTS_I64_REFUSAL_REASON
                return False, PASSED_TESTS_REFUSAL_REASON
    current_passed_tests_match = (
        _i64(executed)
        and _i64(passed)
        and passed == executed
    )
    legacy_passed_tests_mismatch = passed is not None and (
        not isinstance(passed, int)
        or isinstance(passed, bool)
        or passed != executed
    )
    if (
        schema >= pred["passed_tests"]["applies_at_schema_min"]
        and not current_passed_tests_match
    ) or (
        schema < pred["passed_tests"]["applies_at_schema_min"]
        and legacy_passed_tests_mismatch
    ):
        return False, PASSED_TESTS_REFUSAL_REASON
    if pred.get("gate_filtered_tests") and row.get("filtered_tests") != 0:
        return False, "filtered_tests!=0"
    if schema >= pred["counts_schema"]:
        gates_ok, gates_reason = gates_complete(row)
        if not gates_ok:
            return False, f"gate accounting incomplete: {gates_reason}"
    coverage_schema = coverage_schema_verdict(row, pred)
    if not coverage_schema_accepted(coverage_schema):
        return False, shared_evidence_reason(coverage_schema)
    count_capable = schema >= pred["counts_schema"]
    counts_present = (
        row.get("executed_tests") is not None and row.get("filtered_tests") is not None
    )
    exec_val = row.get("executed_tests")
    # The surviving zero-execution floor; NOT a completeness discriminator.
    executed_ok = isinstance(exec_val, int) and exec_val >= req["executed_tests_min"]
    cov = pred["coverage"]
    if count_capable:
        if not executed_ok:
            return False, "count-capable receipt missing executed_tests"
        if cov["per_node"] and schema >= cov["applies_at_schema_min"]:
            verdict = coverage_verdict(row.get("coverage"))
            if verdict is not CoverageVerdict.SATISFIED:
                return False, f"count-capable receipt coverage {verdict.value}"
        return True, "qualifies"
    if counts_present:
        # Old-schema writer that carried counts but predates per-node coverage:
        # hold it to the strongest thing it can prove -- nonzero execution.
        if executed_ok:
            return True, "qualifies"
    # Neither count present: an uncounted receipt is UNVERIFIED, not green.
    return False, "pre-count receipt cannot prove nonzero execution"


def _row_qualifies_without_class(
    row: dict[str, Any], sha: str, pred: dict[str, Any]
) -> bool:
    """Compatibility boolean wrapper around the canonical diagnostic result."""
    return _row_qualification_without_class(row, sha, pred)[0]


def green_class_of(row: dict[str, Any]) -> str:
    """The row's derived green class. Delegates; never re-derives."""
    return _green_class.derive_class(row)[0]


def row_qualification(
    row: dict[str, Any], sha: str, pred: dict[str, Any]
) -> tuple[bool, str]:
    """THE qualifying-receipt predicate, plus the green-CLASS clause.

    The class clause is applied LAST and can only NARROW: a row that already
    failed the value clauses stays refused, and a row that passed them must
    additionally be of a class the predicate accepts. Ordering matters -- putting
    it first would let a class check mask a value failure and change which reason
    a refusal reports.

    Behaviour is unchanged for every row that exists today: `accepts_green_class`
    defaults to ["hard"], and a row with no `validated_head_sha` derives its class
    as hard (measured: 585/585 live ledger rows).
    """
    value_ok, reason = _row_qualification_without_class(row, sha, pred)
    if not value_ok:
        return False, reason
    # PROVENANCE, applied after the value clauses and before the class clause.
    # Like the class clause it can only NARROW, and ordering it here keeps every
    # existing refusal reason byte-identical: a row that already failed on value
    # still reports its value reason, so turning this on cannot silently
    # re-attribute an unrelated failure to a missing writer.
    verdict = producer_verdict(row, pred)
    if verdict in (ProducerVerdict.MISSING, ProducerVerdict.UNKNOWN_WRITER):
        if producer_enforced_for(row, pred):
            return False, f"producer {verdict.value}"
    coverage_schema = coverage_schema_verdict(row, pred)
    base = base_evidence_verdict(row, pred)
    admission = admission_verdict(row, pred)
    cell_results = cell_results_verdict(row, pred)
    evidence_reason = shared_evidence_reason(
        coverage_schema, base, admission, cell_results
    )
    if evidence_reason != "qualifies":
        return False, evidence_reason
    green_class, class_reason = _green_class.derive_class(row)
    if green_class not in _green_class.accepted_classes(pred):
        return False, f"green_class={green_class!r}: {class_reason}"
    return True, "qualifies"


def row_qualifies(row: dict[str, Any], sha: str, pred: dict[str, Any]) -> bool:
    """Boolean compatibility wrapper around :func:`row_qualification`."""
    return row_qualification(row, sha, pred)[0]


def authoritative_row_qualification(
    row: dict[str, Any],
    sha: str,
    pred: dict[str, Any],
    *,
    repositories: list[str | Path] | None = None,
) -> tuple[bool, str]:
    """Qualify one row using the validation result carried by that row.

    ``repositories`` remains accepted so existing callers do not break while
    the former validator-file allowlist is disabled.  A completed clean,
    exact-commit full run decides validation; changing validation code no
    longer requires a second repository to register its blob hashes first.
    """
    del repositories
    return row_qualification(row, sha, pred)


def authoritative_row_qualifies(
    row: dict[str, Any],
    sha: str,
    pred: dict[str, Any],
    *,
    repositories: list[str | Path] | None = None,
) -> bool:
    """Boolean wrapper for authority-bearing ledger consumers."""
    return authoritative_row_qualification(
        row, sha, pred, repositories=repositories
    )[0]


def main(argv: list[str] | None = None) -> int:
    """Semantic row-verifier CLI used by non-Python authority consumers."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha")
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--evidence-only",
        "--admission-only",
        dest="evidence_only",
        action="store_true",
        help="verify shared coverage-schema, admission, producer, and base evidence",
    )
    parser.add_argument(
        "--merge-boundary",
        action="store_true",
        help="verify recorded base evidence against freshly resolved merge-boundary tips",
    )
    parser.add_argument(
        "--producer-definition-row",
        action="store_true",
        help="derive and verify the exact producer map for one ledger row",
    )
    parser.add_argument("--current-base")
    parser.add_argument("--current-reverie-base")
    parser.add_argument("--repo-checkout")
    parser.add_argument("--reverie-checkout")
    args = parser.parse_args(argv)
    if args.producer_definition_row:
        if args.sha is None or _SHA40.fullmatch(args.sha) is None:
            parser.error("--producer-definition-row requires --sha as 40 lowercase hex")
        try:
            row = json.load(sys.stdin)
            if not isinstance(row, dict):
                raise ValueError("ledger row is not an object")
            repositories = [args.repo_checkout] if args.repo_checkout else None
            producer = resolve_producer_definition(
                row, args.sha, repositories=repositories
            )
        except ProducerDefinitionUndereferenceable as error:
            # rc 3, NOT rc 1. rc 1 means "this receipt does not qualify"; this
            # means "I could not judge it". Collapsing them makes an actor with
            # an incomplete checkout disbelieve every valid receipt in the fleet
            # and blame the evidence for its own missing submodule.
            print(
                json.dumps(
                    {
                        "accepted": False,
                        "producer_definition_status": "undereferenceable-reader-incomplete",
                        "receipt_judged": False,
                        "detail": str(error),
                    },
                    sort_keys=True,
                )
            )
            return 3
        except ProducerDefinitionError as error:
            print(
                json.dumps(
                    {
                        "accepted": False,
                        "producer_definition_status": "unregistered-or-unresolvable",
                        "receipt_judged": True,
                        "detail": str(error),
                    },
                    sort_keys=True,
                )
            )
            return 1
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            print(f"qualifying-receipt: {error}", file=sys.stderr)
            return 2
        print(
            json.dumps(
                {
                    "accepted": True,
                    "producer_definition_status": "satisfied",
                    "producer": producer,
                },
                sort_keys=True,
            )
        )
        return 0
    if args.evidence_only:
        try:
            request = json.load(sys.stdin)
            if not isinstance(request, dict) or not isinstance(request.get("row"), dict):
                raise ValueError("evidence request must contain an object-valued row")
            pred = {
                "admission": request.get("admission"),
                "producer": request.get("producer"),
                "base": request.get("base"),
                "coverage": request.get("coverage"),
                "cell_results": request.get("cell_results"),
            }
            verdict = admission_verdict(request["row"], pred)
            base = base_evidence_verdict(request["row"], pred)
            coverage_schema = coverage_schema_verdict(request["row"], pred)
            cell_results = cell_results_verdict(request["row"], pred)
        except (ValueError, json.JSONDecodeError) as error:
            print(f"qualifying-receipt: {error}", file=sys.stderr)
            return 2
        satisfied = (
            True if verdict is AdmissionVerdict.SATISFIED else
            None if verdict is AdmissionVerdict.GRANDFATHERED_UNKNOWN else False
        )
        accepted = (
            admission_accepted(verdict)
            and base_accepted(base)
            and coverage_schema_accepted(coverage_schema)
            and cell_results_accepted(cell_results)
        )
        print(json.dumps({
            "accepted": accepted,
            "evidence_reason": shared_evidence_reason(
                coverage_schema, base, verdict, cell_results
            ),
            "admission_satisfied": satisfied,
            "admission_status": verdict.value,
            "base_satisfied": (
                True if base is BaseVerdict.SATISFIED else
                None if base is BaseVerdict.GRANDFATHERED_UNKNOWN else False
            ),
            "base_status": base.value,
            "coverage_schema_satisfied": (
                True if coverage_schema is CoverageSchemaVerdict.SATISFIED else
                None if coverage_schema is CoverageSchemaVerdict.GRANDFATHERED_UNKNOWN else False
            ),
            "coverage_schema_status": coverage_schema.value,
            "cell_results_satisfied": (
                True if cell_results is CellResultsVerdict.SATISFIED else
                None if cell_results is CellResultsVerdict.GRANDFATHERED_UNKNOWN else False
            ),
            "cell_results_status": cell_results.value,
            "cell_scorecard": cell_scorecard_projection(request["row"], pred),
        }, sort_keys=True))
        return 0 if accepted else 1
    if args.merge_boundary:
        required = {
            "--current-base": args.current_base,
            "--current-reverie-base": args.current_reverie_base,
            "--repo-checkout": args.repo_checkout,
            "--reverie-checkout": args.reverie_checkout,
        }
        missing = [name for name, value in required.items() if not value]
        if missing:
            parser.error(f"--merge-boundary requires {', '.join(missing)}")
        try:
            row = json.load(sys.stdin)
            if not isinstance(row, dict):
                raise ValueError("ledger row is not an object")
            verdict = base_boundary_verdict(
                row,
                active(),
                current_base=args.current_base,
                current_reverie_base=args.current_reverie_base,
                repo_checkout=args.repo_checkout,
                reverie_checkout=args.reverie_checkout,
            )
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            print(f"qualifying-receipt: {error}", file=sys.stderr)
            return 2
        accepted = base_accepted(verdict)
        # RECORD THE BOUND AND THE LAG IT WAS MEASURED AGAINST, not just the
        # verdict. RECEIPT_BASE_MAX_LAG_COMMITS propagates by plain environment
        # inheritance all the way down the landing path, so without these two
        # fields a run with the bound disabled is indistinguishable in the
        # artifact from one where the lag was simply small. An auditor holding
        # the checkout could recompute it from the two shas; an auditor holding
        # only the report could not. `check-gitlink-currency.sh` prints its
        # observed count for the same reason.
        try:
            base_max_lag = _receipt_base_max_lag_commits()
        except RuntimeError:
            base_max_lag = None
        base_lag = None
        try:
            if _SHA40.fullmatch(str(row.get("base_sha") or "")):
                base_lag = git_commit_count(
                    args.repo_checkout, row["base_sha"], args.current_base
                )
        except (OSError, RuntimeError):
            base_lag = None
        print(json.dumps({
            "accepted": accepted,
            "base_lag": base_lag,
            "base_max_lag": base_max_lag,
            "base_satisfied": (
                True if verdict is BaseVerdict.SATISFIED else
                None if verdict is BaseVerdict.GRANDFATHERED_UNKNOWN else False
            ),
            "base_status": verdict.value,
            "recorded_base": row.get("base_sha"),
            "current_base": args.current_base,
            "recorded_reverie_base": row.get("reverie_base_sha"),
            "current_reverie_base": args.current_reverie_base,
        }, sort_keys=True))
        return 0 if accepted else 1
    if args.sha is None:
        parser.error("--sha is required unless --evidence-only is used")
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        print("qualifying-receipt: --sha must be 40 lowercase hex", file=sys.stderr)
        return 2
    try:
        row = json.load(sys.stdin)
        if not isinstance(row, dict):
            raise ValueError("ledger row is not an object")
        pred = active()
        green_class, reason = _green_class.derive_class(row)
        accepted = row_qualifies(row, args.sha, pred)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"qualifying-receipt: {error}", file=sys.stderr)
        return 2
    prod = producer_verdict(row, pred)
    admission = admission_verdict(row, pred)
    base = base_evidence_verdict(row, pred)
    coverage_schema = coverage_schema_verdict(row, pred)
    cell_results = cell_results_verdict(row, pred)
    report = {
        "schema_version": 1,
        "sha": args.sha,
        "accepted": accepted,
        "green_class": green_class,
        "reason": reason,
        "accepts_green_class": _green_class.accepted_classes(pred),
        # Reported as null rather than false when the row predates the
        # obligation, so a grandfathered row never CLAIMS a provenance it does
        # not carry -- the same discipline as coverage_satisfied.
        "producer": row.get("producer"),
        "producer_ok": True if prod is ProducerVerdict.OK else (
            None if prod is ProducerVerdict.GRANDFATHERED else False
        ),
        "producer_status": prod.value,
        "admission_satisfied": (
            True if admission is AdmissionVerdict.SATISFIED else
            None if admission is AdmissionVerdict.GRANDFATHERED_UNKNOWN else False
        ),
        "admission_status": admission.value,
        "base_satisfied": (
            True if base is BaseVerdict.SATISFIED else
            None if base is BaseVerdict.GRANDFATHERED_UNKNOWN else False
        ),
        "base_status": base.value,
        "coverage_schema_satisfied": (
            True if coverage_schema is CoverageSchemaVerdict.SATISFIED else
            None if coverage_schema is CoverageSchemaVerdict.GRANDFATHERED_UNKNOWN else False
        ),
        "coverage_schema_status": coverage_schema.value,
        "cell_results_satisfied": (
            True if cell_results is CellResultsVerdict.SATISFIED else
            None if cell_results is CellResultsVerdict.GRANDFATHERED_UNKNOWN else False
        ),
        "cell_results_status": cell_results.value,
        "cell_scorecard": cell_scorecard_projection(row, pred),
    }
    if args.json:
        print(json.dumps(report, sort_keys=True))
    elif accepted:
        print(f"QUALIFIED {args.sha} class={green_class}")
    else:
        print(f"REFUSED {args.sha} class={green_class}: {reason}")
    return 0 if accepted else 1


if __name__ == "__main__":
    raise SystemExit(main())
