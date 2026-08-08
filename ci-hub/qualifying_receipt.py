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
import json
import os
import re
import subprocess
import sys
from enum import Enum
from pathlib import Path
from typing import Any


U64_MAX = (1 << 64) - 1

PREDICATE_ENV = "QUALIFYING_RECEIPT_PREDICATE"
# The canonical file lives beside this module's `validate/` sibling. The literal
# repo-relative path lives in the Rust module's PREDICATE_REL; here we resolve
# against this file so a Python consumer needs no repo-root argument.
PREDICATE_REL = "validate/qualifying-receipt.json"

_ACTIVE: dict[str, Any] | None = None


class ProducerDefinitionError(RuntimeError):
    """The validated commit cannot be bound to one registered whole map."""


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
    return subprocess.run(
        [str(producer_definition_verifier_path()), *arguments],
        input=input_body,
        text=True,
        capture_output=True,
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
    candidates.append(Path(__file__).resolve().parents[1] / "hermit")
    seen: set[str] = set()
    ordered: list[Path] = []
    for candidate in candidates:
        key = str(candidate)
        if key in seen or not candidate.is_dir():
            continue
        seen.add(key)
        ordered.append(candidate)
    return ordered


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
    attempted: list[str] = []
    failures: list[str] = []
    for checkout in producer_definition_repositories(row, repositories):
        attempted.append(str(checkout))
        result = _run_producer_definition_verifier(
            "--producer-definition-resolve",
            "--sha",
            sha,
            "--repo-checkout",
            str(checkout),
        )
        if result.returncode == 0:
            try:
                producer = json.loads(result.stdout)
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
                    not isinstance(commit, str)
                    or _SHA40.fullmatch(commit) is None
                    for commit in valid_commits
                )
                or len(set(valid_commits)) != len(valid_commits)
                or sha not in valid_commits
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


# The green CLASS (hard vs inherited/soft) is derived from the row's provenance by
# ci-hub/validate/green_class.py. It is imported rather than restated: a second copy
# of the derivation is exactly the drift this shared predicate exists to remove.
sys.path.insert(0, str(Path(__file__).resolve().parent / "validate"))
import green_class as _green_class  # noqa: E402


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
    for key in ("counts_schema", "require", "coverage", "base", "admission"):
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


def _git_output(checkout: str, *args: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", checkout, *args], capture_output=True, check=False
    )
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip() or f"exit {result.returncode}"
        raise RuntimeError(f"git -C {checkout} {' '.join(args)}: {detail}")
    return result.stdout


def git_tree(checkout: str, sha: str) -> str:
    value = _git_output(checkout, "rev-parse", "--verify", f"{sha}^{{tree}}").decode().strip()
    if _SHA40.fullmatch(value) is None:
        raise RuntimeError(f"{checkout}: {sha} has no exact tree identity")
    return value


def git_is_ancestor(checkout: str, ancestor: str, descendant: str) -> bool:
    result = subprocess.run(
        ["git", "-C", checkout, "merge-base", "--is-ancestor", ancestor, descendant],
        capture_output=True,
        check=False,
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = result.stderr.decode(errors="replace").strip() or f"exit {result.returncode}"
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
    """Assert strict mutable-tip currency exactly once before merge."""
    static = base_evidence_verdict(row, pred)
    if static is not BaseVerdict.SATISFIED:
        return static
    if _SHA40.fullmatch(current_base) is None or _SHA40.fullmatch(current_reverie_base) is None:
        return BaseVerdict.POLICY_UNAVAILABLE
    if row["base_sha"] != current_base:
        return BaseVerdict.BASE_NOT_CURRENT
    try:
        if row["base_tree"] != git_tree(repo_checkout, current_base):
            return BaseVerdict.BASE_TREE_MISMATCH
        receipt_commit = row.get("commit")
        if not isinstance(receipt_commit, str) or _SHA40.fullmatch(receipt_commit) is None:
            return BaseVerdict.BASE_NOT_CONTAINED
        if not git_is_ancestor(repo_checkout, current_base, receipt_commit):
            return BaseVerdict.BASE_NOT_CONTAINED
    except RuntimeError:
        return BaseVerdict.POLICY_UNAVAILABLE
    if row["reverie_base_sha"] != current_reverie_base:
        return BaseVerdict.REVERIE_BASE_NOT_CURRENT
    try:
        current_reverie_tree = git_tree(reverie_checkout, current_reverie_base)
    except RuntimeError:
        return BaseVerdict.POLICY_UNAVAILABLE
    if row["reverie_base_tree"] != current_reverie_tree:
        return BaseVerdict.REVERIE_TREE_MISMATCH
    return BaseVerdict.SATISFIED


def admission_verdict(row: dict[str, Any], pred: dict[str, Any]) -> AdmissionVerdict:
    """Verify canonical admission, zero concurrency, and owner ancestry.

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
    concurrent = row.get("concurrent_validates")
    if concurrent is None:
        return AdmissionVerdict.CONCURRENCY_MISSING
    if (
        not isinstance(required_concurrent, int)
        or isinstance(required_concurrent, bool)
        or required_concurrent < 0
    ):
        return AdmissionVerdict.POLICY_UNAVAILABLE
    if (
        not isinstance(concurrent, int)
        or isinstance(concurrent, bool)
        or concurrent != required_concurrent
    ):
        return AdmissionVerdict.CONCURRENCY_INVALID

    required_proof = clause.get("required_concurrency_proof")
    proof = row.get("concurrency_proof")
    if not isinstance(proof, str) or not proof:
        return AdmissionVerdict.OWNER_ANCESTRY_MISSING
    if not isinstance(required_proof, str) or not required_proof:
        return AdmissionVerdict.POLICY_UNAVAILABLE
    if proof != required_proof:
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


def _row_qualification_without_class(
    row: dict[str, Any], sha: str, pred: dict[str, Any]
) -> tuple[bool, str]:
    """THE qualifying-receipt predicate, mirroring the Rust `row_qualifies`
    clause for clause. Completeness for a count-capable receipt is decided by
    per-node COVERAGE, not the executed_tests count. The reason names the first
    failing clause so downstream consumers do not need a diagnostic copy."""
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
    if pred.get("gate_filtered_tests") and row.get("filtered_tests") != 0:
        return False, "filtered_tests!=0"
    schema = row.get("schema_version") or 0
    coverage_schema = coverage_schema_verdict(row, pred)
    if not coverage_schema_accepted(coverage_schema):
        return False, f"coverage schema {coverage_schema.value}"
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
    base = base_evidence_verdict(row, pred)
    if not base_accepted(base):
        return False, f"base {base.value}"
    admission = admission_verdict(row, pred)
    if not admission_accepted(admission):
        return False, f"admission {admission.value}"
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
    """Qualify one green row and bind it to an allowed producer whole map.

    ``row_qualification`` remains the one semantic clause authority and is
    intentionally useful to focused predicate tests.  Any consumer that turns
    its answer into green authority must use this stricter boundary: semantic
    green without an exact registered producer definition is not corroboration.
    Registry/verifier deploy defects propagate instead of becoming an ordinary
    negative; an unregistered or unresolvable map is a clean refusal.
    """
    accepted, reason = row_qualification(row, sha, pred)
    if not accepted:
        return False, reason
    try:
        resolve_producer_definition(row, sha, repositories=repositories)
    except ProducerDefinitionError as error:
        return False, f"producer_definition unregistered-or-unresolvable: {error}"
    return True, "qualifies"


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
        except ProducerDefinitionError as error:
            print(
                json.dumps(
                    {
                        "accepted": False,
                        "producer_definition_status": "unregistered-or-unresolvable",
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
            }
            verdict = admission_verdict(request["row"], pred)
            base = base_evidence_verdict(request["row"], pred)
            coverage_schema = coverage_schema_verdict(request["row"], pred)
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
        )
        print(json.dumps({
            "accepted": accepted,
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
        print(json.dumps({
            "accepted": accepted,
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
