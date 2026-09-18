#!/usr/bin/env python3
"""MAIN-REACHABLE validation-receipt COUNTS + per-node COVERAGE finalizer.

Produces or preserves the schema-5 `coverage{}` obligation (plus the aggregate
`executed_tests` / `filtered_tests` diagnostics) and binds it to merge-boundary
base evidence. Historical count-less rows are reconstructed from two things that
already exist on `main`:

  1. the full safe-ci-dag-runner log (`[<node.tag>] `-prefixed child stdout +
     `[<node.tag>] ✓ PASS/✗ FAIL` terminal lines), and
  2. the PLANNED test-node set, read from the sole canonical DAG AT THE EXACT
     COMMIT via `git show <sha>:ci/dag/validate.json` and selected with its
     committed `full` label.

Reading the manifest at the commit — WITHOUT running that branch's validation driver
— is what makes this main-reachable and version-independent: the planned set is
the branch's own declared intent, but the finalizer that judges it lives on main
and cannot be weakened by an older PR-branch YAML.

Design source: ai_docs/bind-validation-counts-per-node-coverage-design_20260804.md.

The obligation (computed here, ENFORCED by the Rust consumer from the receipt):
  * ran/absent   : every PLANNED `test.*` node must have a terminal PASS/FAIL line;
                   a planned node with none -> `absent_nodes` -> NOT satisfied.
  * zero-executed: a planned node that emitted >=1 libtest banner must have
                   passed-sum > 0; a banner-emitting node summing to 0 ->
                   `zero_executed_nodes` -> NOT satisfied. A node with ZERO
                   banners is EXEMPT (legit shell/e2e/nextest node).
  * `filtered_tests` is a DIAGNOSTIC ONLY -- there is NO `filtered == 0` predicate.

The consumer's `coverage_satisfied` == `planned_test_nodes > 0 &&
zero_executed_nodes == [] && absent_nodes == []`. This finalizer does NOT decide
validated/not; it only WRITES the qualified row (carry the condition with the
value) so the Rust consumer can re-derive the verdict from receipt fields alone.

Usage:
  finalize_receipt.py --log LOG --sha SHA --hermit-checkout DIR --emit-only
  finalize_receipt.py --scan --sha SHA \
    --selected-row-sha256 DIGEST --hermit-checkout DIR
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

# The finalizer is a Python consumer of the same semantic receipt authority as
# history, publishing, and anchor selection. Do not restate schema-5 success.
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import qualifying_receipt  # noqa: E402
from admission_paths import AdmissionPaths  # noqa: E402

# ONE extractor, not a second regex copy that could drift: per-node counting
# lives solely in the remediation `nonzero_result` module (also imported by
# aggregate.py and protocol.py). Mirror aggregate.py's sys.path setup.
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "remediation"))
from nonzero_result import per_node_counts  # noqa: E402
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "ledger"))
import validate_rows as canonical_ledger  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

SCHEMA_VERSION = 5
RECEIPT_CANONICALIZATION = "serde_json::to_vec(HistoryRow)-v1"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
#: This writer's registered slug for the ledger `producer` column. Must appear in
#: `producer.known` of ci-hub/validate/qualifying-receipt.json -- an unregistered
#: slug is refused exactly like an absent one once the epoch is flipped.
PRODUCER = "ci-hub-finalize-receipt"
CANONICAL_MANIFEST = "ci/dag/validate.json"
FULL_LABEL = "full"
STRUCTURED_TEST_RESULTS_KIND = "structured-test-results"
STRUCTURED_TEST_RESULTS_KEYS = frozenset({"kind", "owner", "path_env", "schema"})
# dagrun's retained and current structured-result schemas are both 2 at the
# Hermit revision this parent pins. A future schema must update both authorities.
STRUCTURED_TEST_RESULTS_SCHEMAS = frozenset({2})
TEST_COUNTS_PATH_ENV = "DAGRUN_TEST_COUNTS_PATH"
REVERIE_SOURCE_RE = re.compile(
    r"git\+https://github\.com/(?:rrnewton|facebookexperimental)/reverie\.git"
    r"\?rev=([0-9a-f]{40})(?:#[0-9a-f]{40})?"
)


class SelectedRowError(RuntimeError):
    """An exact-row identity could not be resolved safely."""

    def __init__(self, reason: str, detail: str):
        super().__init__(detail)
        self.reason = reason


def _git(checkout: str, *args: str) -> str:
    result = bounded_capture.git(args, cwd=checkout)
    if result.returncode != 0:
        raise RuntimeError(
            result.stderr.strip() or f"git -C {checkout} {' '.join(args)} exited {result.returncode}"
        )
    return result.stdout.strip()


def canonical_row_sha256(
    row: dict, sha: str, *, require_canonical_qualifying: bool = False
) -> str:
    """Return the Rust ``HistoryRow`` canonical digest for exactly ``row``.

    JSON key sorting is not this authority: Rust serializes typed ``HistoryRow``
    fields in struct order and flattened extension fields in ``BTreeMap`` order.
    Route both identity verification and the post-clone qualification preflight
    through that one canonical implementation.
    """
    ci_hub = str(Path(__file__).resolve().parents[1] / "ci-hub")
    command = [ci_hub, "receipt-digest", "--sha", sha]
    if require_canonical_qualifying:
        command.append("--require-canonical-qualifying")
    result = bounded_capture.capture(
        command,
        input=qualifying_receipt.admission_json_dumps(row),
    )
    digest = result.stdout.strip()
    if result.returncode != 0 or DIGEST_RE.fullmatch(digest) is None:
        detail = result.stderr.strip() or result.stdout.strip() or (
            f"{' '.join(command)} exited {result.returncode}"
        )
        raise RuntimeError(detail)
    return digest


def _load_rows(handle) -> list[dict]:
    """Read object-valued JSONL rows; malformed lines remain untouched."""
    handle.seek(0)
    rows: list[dict] = []
    for line in handle:
        if not line.strip():
            continue
        try:
            value = json.loads(line, object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            rows.append(value)
    return rows


@contextmanager
def _locked_ledger(ledger_path: str, *, exclusive: bool):
    """Lock the ledger inode using the same ``flock`` domain as scripts/validate.rs."""
    mode = "a+" if exclusive else "r"
    with open(ledger_path, mode, errors="replace") as handle:
        fcntl.flock(
            handle.fileno(), fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        )
        try:
            yield handle
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _resolve_selected_row(
    rows: list[dict], sha: str, selected_row_sha256: str
) -> dict:
    """Resolve one exact base row, filtering by SHA before any status logic."""
    if SHA_RE.fullmatch(sha) is None:
        raise SelectedRowError("invalid-sha", "--sha must be exactly 40 lowercase hex")
    if DIGEST_RE.fullmatch(selected_row_sha256) is None:
        raise SelectedRowError(
            "invalid-selected-row-sha256",
            "--selected-row-sha256 must be exactly 64 lowercase hex",
        )

    # Proxy binding: rows for every other SHA are outside the decision before
    # idempotency, eligibility, or any other handled/already logic can run.
    exact_sha_rows = [row for row in rows if row.get("commit") == sha]
    matches: list[dict] = []
    for row in exact_sha_rows:
        try:
            digest = canonical_row_sha256(row, sha)
        except RuntimeError:
            # A value that Rust cannot parse as HistoryRow has no canonical row
            # identity and therefore cannot be the caller's selected row.
            continue
        if digest == selected_row_sha256:
            matches.append(row)
    if not matches:
        raise SelectedRowError(
            "selected-row-missing",
            f"no canonical row for {sha} has digest {selected_row_sha256}",
        )
    if len(matches) != 1:
        raise SelectedRowError(
            "ambiguous-selected-row",
            f"{len(matches)} rows for {sha} have digest {selected_row_sha256}",
        )
    return matches[0]


def build_base_evidence(
    hermit_checkout: str, sha: str, reverie_checkout: str
) -> dict:
    """Record exact Hermit and Reverie base SHAs and trees.

    `base_sha` is the actual merge-base of the validated head and the locally
    fetched Hermit origin/main. It is historical evidence, not a mutable
    `current=true` assertion. Currency is checked only at the merge boundary.
    """
    observed_main = _git(hermit_checkout, "rev-parse", "refs/remotes/origin/main^{commit}")
    if SHA_RE.fullmatch(observed_main) is None:
        raise RuntimeError("local Hermit main is not an exact lowercase 40-hex SHA")
    observed_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    base_sha = _git(hermit_checkout, "merge-base", sha, observed_main)
    if re.fullmatch(r"[0-9a-f]{40}", base_sha) is None:
        raise RuntimeError("Hermit merge-base is not an exact lowercase 40-hex SHA")
    ancestor = subprocess.run(
        ["git", "-C", hermit_checkout, "merge-base", "--is-ancestor", base_sha, sha]
    )
    if ancestor.returncode != 0:
        raise RuntimeError(f"recorded Hermit base {base_sha} is not contained by {sha}")
    base_tree = qualifying_receipt.git_tree(hermit_checkout, base_sha)

    cargo_lock = _git(hermit_checkout, "show", f"{sha}:Cargo.lock")
    reverie_pins = set(REVERIE_SOURCE_RE.findall(cargo_lock))
    if len(reverie_pins) != 1:
        raise RuntimeError(
            f"expected exactly one Reverie pin in {sha}:Cargo.lock, found {sorted(reverie_pins)}"
        )
    reverie_base_sha = next(iter(reverie_pins))
    reverie_base_tree = qualifying_receipt.git_tree(
        reverie_checkout, reverie_base_sha
    )
    return {
        "base_sha": base_sha,
        "base_tree": base_tree,
        "reverie_base_sha": reverie_base_sha,
        "reverie_base_tree": reverie_base_tree,
        "base_observation": {
            "contract": "post-run-local-main-merge-base/v1",
            "observed_at": observed_at,
            "observed_main_sha": observed_main,
        },
    }


def read_admission_context(path: str, expected_sha256: str, *, state_root=None) -> dict:
    """Read one bounded regular file and retain its exact versioned bytes."""
    if not isinstance(expected_sha256, str) or DIGEST_RE.fullmatch(expected_sha256) is None:
        raise ValueError("admission context digest must be 64 lowercase hex")

    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"admission context duplicate key: {key}")
            result[key] = value
        return result

    def refuse_number(value):
        raise ValueError(f"admission context forbids floating/nonfinite number: {value}")

    try:
        if state_root is not None:
            with AdmissionPaths(state_root) as paths:
                locator = paths.locator(path)
                raw = paths.read(locator, maximum=qualifying_receipt.ADMISSION_CONTEXT_MAX_BYTES)
        else:
            fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
            with os.fdopen(fd, "rb") as handle:
                before = os.fstat(handle.fileno())
                if not stat.S_ISREG(before.st_mode):
                    raise ValueError("admission context is not a regular file")
                raw = handle.read(qualifying_receipt.ADMISSION_CONTEXT_MAX_BYTES + 1)
                after = os.fstat(handle.fileno())
            if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
                    after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
                raise ValueError("admission context changed while reading")
    except OSError as error:
        raise ValueError(f"cannot read admission context: {error}") from error
    if len(raw) > qualifying_receipt.ADMISSION_CONTEXT_MAX_BYTES:
        raise ValueError("admission context exceeds reader byte capacity")
    if hashlib.sha256(raw).hexdigest() != expected_sha256:
        raise ValueError("admission context file digest mismatch")
    try:
        context = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object,
                             parse_float=refuse_number, parse_constant=refuse_number)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError(f"malformed admission context JSON: {error}") from error
    if qualifying_receipt.admission_context_bytes(context) != raw:
        version = context["contract"].rsplit("/", 1)[-1]
        raise ValueError(f"admission context bytes are not canonical {version} JSON")
    v2 = context["contract"] == qualifying_receipt.ADMISSION_CONTEXT_V2
    if v2 and state_root is None:
        raise ValueError("v2 admission context requires an explicit state root")
    if not v2 and state_root is not None:
        raise ValueError("admission state root is only a v2 input")
    evidence = {
        "context": context,
        "artifact": {**({"locator": locator} if v2 else {"path": path}),
                     "sha256": expected_sha256, "bytes": len(raw)},
    }
    qualifying_receipt.validate_admission_floor_evidence(evidence)
    return evidence


def verify_admission_context_git(evidence: dict, hermit_checkout: str, sha: str, log: str,
                                 *, state_root=None) -> None:
    """Check available CLI/Git facts, without inventing host/run/start authority.

    The driver must compare the returned structure with its original in-memory
    proof. SHA/log/object checks here do not independently authenticate the
    context's run identity or rederive its execution plan.
    """
    # Only this new contract uses immutable object reads. Do not incidentally
    # alter the older base observation or landing policy helpers.
    def git(*args):
        return bounded_capture.git(
            ["--no-replace-objects", *args],
            cwd=hermit_checkout,
            # Git reads GIT_GRAFT_FILE; core.graftFile is not its control.
            # Scope this immutable view to the child, without changing ambient
            # configuration or the older base/landing readers.
            env={**os.environ, "GIT_GRAFT_FILE": os.devnull},
        )

    def tree(commit):
        kind = git("cat-file", "-t", commit)
        if kind.returncode != 0 or kind.stdout.strip() != "commit":
            raise ValueError("admission object is not a stored commit")
        result = git("rev-parse", "--verify", f"{commit}^{{tree}}")
        if result.returncode != 0 or SHA_RE.fullmatch(result.stdout.strip()) is None:
            raise ValueError("admission commit tree is unavailable")
        return result.stdout.strip()

    context = evidence["context"]
    if context["target_sha"] != sha:
        raise ValueError("admission context target/log differs from selected invocation")
    if context["contract"] == qualifying_receipt.ADMISSION_CONTEXT_V2:
        if state_root is None:
            raise ValueError("v2 admission context requires an explicit state root")
        with AdmissionPaths(state_root) as paths:
            paths.require_log(context, log)
    elif context["log_file"] != log:
        raise ValueError("admission context target/log differs from selected invocation")
    if tree(sha) != context["target_tree"]:
        raise ValueError("admission context target tree mismatch")
    floor = context["floor"]
    if tree(floor["sha"]) != floor["tree"]:
        raise ValueError("admission context floor tree mismatch")
    ancestry = git("merge-base", "--is-ancestor", floor["sha"], sha)
    if ancestry.returncode == 1:
        raise ValueError("admission floor is not contained in target")
    if ancestry.returncode != 0:
        raise ValueError(f"admission ancestry unavailable: {ancestry.stderr.strip()}")


def _canonical_full_test_producers(data: object) -> set[str]:
    """Return the canonical full plan's structured test-result producers.

    This is the small read-only subset of dagrun's selection contract needed by
    the historical receipt finalizer: select every step carrying the ``full``
    label, include its dependency ancestry, then apply Hermit's established
    test-group naming rule.  Every selected test node must own exactly one
    structured result declaration for its own tag.  Missing, malformed, or
    ambiguous declarations are evidence defects, not an empty planned set.
    """
    if not isinstance(data, dict) or not isinstance(data.get("steps"), list):
        raise RuntimeError(
            "canonical validation DAG must be an object with a steps list"
        )

    steps_by_tag: dict[str, dict] = {}
    selected: set[str] = set()
    for index, step in enumerate(data["steps"]):
        if not isinstance(step, dict):
            raise RuntimeError(
                f"canonical validation DAG step {index} is not an object"
            )
        group = step.get("group")
        job = step.get("job")
        if (
            not isinstance(group, str)
            or not group
            or not isinstance(job, str)
            or not job
        ):
            raise RuntimeError(
                f"canonical validation DAG step {index} has a missing or malformed group/job"
            )
        tag = f"{group}.{job}"
        if tag in steps_by_tag:
            raise RuntimeError(f"canonical validation DAG has duplicate step tag {tag}")
        labels = step.get("labels", [])
        deps = step.get("deps", [])
        if (
            not isinstance(labels, list)
            or any(not isinstance(label, str) or not label for label in labels)
            or not isinstance(deps, list)
            or any(not isinstance(dep, str) or not dep for dep in deps)
        ):
            raise RuntimeError(
                f"canonical validation DAG step {tag} has malformed labels/deps"
            )
        steps_by_tag[tag] = step
        if FULL_LABEL in labels:
            selected.add(tag)

    if not selected:
        raise RuntimeError(
            f"canonical validation DAG has no {FULL_LABEL!r}-labelled steps"
        )

    pending = list(selected)
    while pending:
        tag = pending.pop()
        for dependency in steps_by_tag[tag].get("deps", []):
            if dependency not in steps_by_tag:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has unknown dependency {dependency}"
                )
            if dependency not in selected:
                selected.add(dependency)
                pending.append(dependency)

    structured_by_tag: dict[str, list[dict]] = {}
    for tag in sorted(selected):
        manifests = steps_by_tag[tag].get("result_manifests")
        if manifests is None:
            structured_by_tag[tag] = []
            continue
        if not isinstance(manifests, list):
            raise RuntimeError(
                f"canonical validation DAG step {tag} has malformed result_manifests"
            )
        if any(not isinstance(manifest, dict) for manifest in manifests):
            raise RuntimeError(
                f"canonical validation DAG step {tag} has a malformed result manifest"
            )
        structured: list[dict] = []
        for declaration in manifests:
            if "kind" not in declaration:
                continue
            if set(declaration) != STRUCTURED_TEST_RESULTS_KEYS:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has a structured result manifest "
                    "with missing or unknown keys"
                )
            if declaration["kind"] != STRUCTURED_TEST_RESULTS_KIND:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has unknown result-manifest kind "
                    f"{declaration['kind']!r}"
                )
            schema = declaration["schema"]
            if (
                not isinstance(schema, int)
                or isinstance(schema, bool)
                or schema not in STRUCTURED_TEST_RESULTS_SCHEMAS
            ):
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has unsupported structured "
                    f"result schema {schema!r}"
                )
            if declaration["path_env"] != TEST_COUNTS_PATH_ENV:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} structured result path_env must be "
                    f"{TEST_COUNTS_PATH_ENV!r}"
                )
            owner = declaration["owner"]
            if not isinstance(owner, str) or not owner:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has an empty or malformed "
                    "structured result owner"
                )
            if owner != tag:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has mismatched result owner "
                    f"{owner!r}"
                )
            if declaration in structured:
                raise RuntimeError(
                    f"canonical validation DAG step {tag} has a duplicate structured "
                    "result manifest"
                )
            structured.append(declaration)
        structured_by_tag[tag] = structured

    planned: set[str] = set()
    for tag in sorted(selected):
        group = steps_by_tag[tag]["group"]
        if not (group == "test" or group.endswith("-test") or group.endswith(":test")):
            continue
        manifests = steps_by_tag[tag].get("result_manifests")
        if not isinstance(manifests, list):
            raise RuntimeError(
                f"canonical validation DAG test producer {tag} has no result_manifests list"
            )
        structured = structured_by_tag[tag]
        if len(structured) != 1:
            raise RuntimeError(
                f"canonical validation DAG test producer {tag} must declare exactly one "
                "structured-test-results manifest"
            )
        planned.add(tag)

    if not planned:
        raise RuntimeError("canonical full validation plan has no test producers")
    return planned


def planned_test_nodes(hermit_checkout: str, sha: str) -> set[str]:
    """Read canonical ``ci/dag/validate.json`` at ``sha`` and select full tests."""
    try:
        proc = bounded_capture.git(
            ["show", f"{sha}:{CANONICAL_MANIFEST}"], cwd=hermit_checkout
        )
    except OSError as error:
        raise RuntimeError(
            f"cannot read canonical validation DAG {CANONICAL_MANIFEST}: {error}"
        ) from error
    if proc.returncode != 0:
        detail = proc.stderr.strip() or f"git show exited {proc.returncode}"
        raise RuntimeError(
            f"cannot read canonical validation DAG {CANONICAL_MANIFEST} at {sha}: {detail}"
        )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"canonical validation DAG {CANONICAL_MANIFEST} at {sha} is malformed JSON: {error}"
        ) from error
    return _canonical_full_test_producers(data)


def build_coverage(log_text: str, planned: set[str]) -> dict:
    """Compute the schema-5 `coverage{}` object + aggregate counts from a log.

    `planned` is the manifest-derived planned test-node set. The two coverage
    lists (`zero_executed_nodes`, `absent_nodes`) are subsets of `planned`: the
    obligation is about the set the profile PLANNED, so a stray unplanned node is
    never counted, and a planned node the log never mentions is `absent`.

    Returns a dict with `schema_version`, `executed_tests`, `filtered_tests`, and
    `coverage`. `executed_tests`/`filtered_tests` are the aggregate sum of
    `passed`/`filtered out` across ALL banners in the log; both are `None`
    (JSON null = UNKNOWN) iff the log carried NO libtest banner at all, distinct
    from `0` (banners present, summing to zero).
    """
    nodes = per_node_counts(log_text)

    absent = sorted(
        tag for tag in planned
        if (n := nodes.get(tag)) is None or n["terminal"] is None
    )
    zero_executed = sorted(
        tag for tag in planned
        if (n := nodes.get(tag)) is not None
        and n["banner_count"] >= 1
        and n["executed"] == 0
    )
    # A planned node is "executed" iff it ran (has a terminal line) and is not
    # inert (either emitted no banner -> exempt, or its banners sum positive).
    executed_test_nodes = sum(
        1 for tag in planned
        if (n := nodes.get(tag)) is not None
        and n["terminal"] is not None
        and (n["banner_count"] == 0 or n["executed"] > 0)
    )

    total_banner = sum(n["banner_count"] for n in nodes.values())
    if total_banner == 0:
        executed_tests: int | None = None
        filtered_tests: int | None = None
    else:
        executed_tests = sum(n["executed"] for n in nodes.values())
        filtered_tests = sum(n["filtered"] for n in nodes.values())

    return {
        "schema_version": SCHEMA_VERSION,
        "executed_tests": executed_tests,
        "filtered_tests": filtered_tests,
        "coverage": {
            "planned_test_nodes": len(planned),
            "executed_test_nodes": executed_test_nodes,
            "zero_executed_nodes": zero_executed,
            "absent_nodes": absent,
        },
    }


# --- race-safe exact-row append minting (the auto-wired path) ---------------
#
# The finalizer NEVER rewrites: it derives schema-5 fields from one caller-bound
# source row and appends at most one clone while holding the ledger's flock. The
# original row and every unrelated row therefore remain byte-for-byte intact.


def _clone_upgraded(
    base_row: dict, fields: dict, selected_row_sha256: str
) -> dict:
    """A schema-5 clone of `base_row` with the derived count/coverage `fields`
    merged in. Every base field (commit, anchoring, cleanliness, profile,
    selection, result, log_file, ...) is preserved so the appended row still
    satisfies `is_clean_full_coverage` on the consumer side."""
    row = dict(base_row)
    base_schema = base_row.get("schema_version")
    row.update(fields)
    # Coverage finalization may enrich a newer row. Never downgrade a schema-6
    # row to this module's schema-5 coverage version: doing so would preserve
    # `cell_results` bytes while reclassifying them as an illegal legacy claim.
    if (
        isinstance(base_schema, int)
        and not isinstance(base_schema, bool)
        and base_schema > row.get("schema_version", 0)
    ):
        row["schema_version"] = base_schema
    # Carry the source identity and claimed writer into the clone. The clone's
    # own producer must truthfully name this finalizer, while ``finalized_from``
    # preserves which exact producer row supplied every inherited condition.
    finalized_from = {
        "digest_algorithm": "sha256",
        "canonicalization": RECEIPT_CANONICALIZATION,
        "digest": selected_row_sha256,
    }
    if "producer" in base_row:
        finalized_from["producer"] = base_row.get("producer")
    row["finalized_from"] = finalized_from
    # Provenance names the writer of THIS row, so it is assigned last and
    # unconditionally. A clone inherits every base field, including any
    # `producer` the ORIGINAL writer stamped; leaving that in place would make a
    # finalizer-minted row claim to have been produced by the original driver, which is
    # precisely the misattribution the column exists to prevent.
    row["producer"] = PRODUCER
    row["corrects"] = base_row.get("record_id")
    row["record_id"] = f"finalized-{selected_row_sha256}"
    return row


def _has_complete_schema5_measurement(rec: dict) -> bool:
    """A producer-carried measurement that needs no replay from its log."""
    executed = rec.get("executed_tests")
    return (
        isinstance(rec.get("schema_version"), int)
        and not isinstance(rec.get("schema_version"), bool)
        and rec["schema_version"] >= SCHEMA_VERSION
        and isinstance(executed, int)
        and not isinstance(executed, bool)
        and executed > 0
        and qualifying_receipt.coverage_satisfied(rec.get("coverage"))
    )


def _is_finalizable_clean_full_pass(rec: dict) -> bool:
    """A clean/full/full/pass source whose missing evidence can be derived.

    Historical count-less rows need coverage replayed from their durable log.
    Current producers already carry a complete schema-5 measurement; those rows
    need only merge-boundary base evidence and must retain their measured counts
    and coverage byte-for-value rather than recomputing them.
    """
    return (
        rec.get("commit_anchored") is True
        and rec.get("tree_dirty") is False
        and rec.get("selection_mode") == "full"
        and rec.get("profile") == "full"
        and rec.get("result") == "pass"
        and (
            rec.get("executed_tests") is None
            or _has_complete_schema5_measurement(rec)
        )
    )


def _has_satisfied_schema5(rec: dict) -> bool:
    """Idempotency guard backed by the canonical receipt authority.

    Only an already-qualifying schema-5 row suppresses re-minting. Missing or
    tampered conditions must not look like falsey empty-list success.
    """
    if (rec.get("schema_version") or 0) < SCHEMA_VERSION:
        return False
    sha = rec.get("commit")
    return isinstance(sha, str) and qualifying_receipt.authoritative_row_qualifies(
        rec, sha, qualifying_receipt.active()
    )


def _existing_finalization(
    rows: list[dict], sha: str, selected_row_sha256: str
) -> dict | None:
    """Return one already-qualified clone of this exact source, or refuse."""
    matches = [
        row
        for row in rows
        if row.get("commit") == sha
        and isinstance(row.get("finalized_from"), dict)
        and row["finalized_from"].get("digest_algorithm") == "sha256"
        and row["finalized_from"].get("digest") == selected_row_sha256
        and row["finalized_from"].get("canonicalization")
        == RECEIPT_CANONICALIZATION
    ]
    if len(matches) > 1:
        raise SelectedRowError(
            "ambiguous-finalization",
            f"{len(matches)} finalized rows claim source {selected_row_sha256}",
        )
    if not matches:
        return None
    try:
        canonical_row_sha256(
            matches[0], sha, require_canonical_qualifying=True
        )
    except RuntimeError as error:
        raise SelectedRowError(
            "invalid-existing-finalization",
            f"existing clone of {selected_row_sha256} is not canonical: {error}",
        ) from error
    return matches[0]


def select_candidate_sha256(ledger_path: str | None, sha: str) -> str:
    """Select the newest exact-SHA finalizable source and return its canonical ID.

    This is a read-only convenience for trusted callers. Mutation still requires
    passing the returned digest back explicitly, and the mutating path re-resolves
    it under an exclusive lock.
    """
    if SHA_RE.fullmatch(sha) is None:
        raise SelectedRowError("invalid-sha", "--sha must be exactly 40 lowercase hex")
    if ledger_path is None:
        rows = canonical_ledger.rows(preserve_admission=True)
    else:
        with _locked_ledger(ledger_path, exclusive=False) as handle:
            rows = _load_rows(handle)
    candidates = [
        row
        for row in rows
        if row.get("commit") == sha and _is_finalizable_clean_full_pass(row)
    ]
    if not candidates:
        raise SelectedRowError(
            "selected-row-missing", f"no finalizable clean/full/pass row for {sha}"
        )
    canonical_candidates: list[tuple[dict, str]] = []
    for row in candidates:
        try:
            canonical_candidates.append((row, canonical_row_sha256(row, sha)))
        except RuntimeError:
            continue
    if not canonical_candidates:
        raise SelectedRowError(
            "selected-row-missing", f"no canonical source row for {sha}"
        )
    # Event time is the semantic order; the remaining recorded run-identity
    # fields make a stable tie-breaker without consulting append position.
    _selected, digest = max(
        canonical_candidates,
        key=lambda item: (
            str(item[0].get("finished_at") or ""),
            str(item[0].get("started_at") or ""),
            str(item[0].get("host") or ""),
            str(item[0].get("slot") or ""),
            str(item[0].get("log_file") or ""),
        ),
    )
    duplicate_count = sum(
        1
        for _row, candidate_digest in canonical_candidates
        if candidate_digest == digest
    )
    if duplicate_count != 1:
        raise SelectedRowError(
            "ambiguous-selected-row",
            f"{duplicate_count} candidate rows for {sha} have digest {digest}",
        )
    return digest


def scan_and_finalize(
    ledger_path: str | None,
    hermit_checkout: str,
    sha: str,
    selected_row_sha256: str,
    reverie_checkout: str | None = None,
    dry_run: bool = False,
) -> dict:
    """Append one complete canonical clone of one exact selected source row.

    Identity failures raise :class:`SelectedRowError`. Evidence failures return
    a nonzero ``exit_code`` and append nothing. Success appends at most one row;
    a repeated request for the same source is idempotent through the carried
    ``finalized_from`` identity.
    """
    if reverie_checkout is None:
        reverie_checkout = str(
            Path(os.environ.get("DEV_HERMIT_PARENT", Path(__file__).resolve().parents[2]))
            / "reverie"
        )
    if ledger_path is None:
        rows = canonical_ledger.rows(preserve_admission=True)
        selected = dict(_resolve_selected_row(rows, sha, selected_row_sha256))
    else:
        with _locked_ledger(ledger_path, exclusive=True) as handle:
            rows = _load_rows(handle)
            selected = dict(_resolve_selected_row(rows, sha, selected_row_sha256))

    if not _is_finalizable_clean_full_pass(selected):
        return {
            "sha": sha,
            "satisfied": False,
            "reason": "insufficient-source-row",
            "detail": "selected row is not a finalizable clean/full/pass source",
            "exit_code": 1,
            "appended": 0,
        }
    if selected.get("repo") not in (None, "hermit", "rrnewton/hermit"):
        return {
            "sha": sha,
            "satisfied": False,
            "reason": "insufficient-source-row",
            "detail": f"selected row names non-Hermit repo {selected.get('repo')!r}",
            "exit_code": 1,
            "appended": 0,
        }

    if "admission_floor_evidence" in selected:
        try:
            evidence = selected["admission_floor_evidence"]
            qualifying_receipt.validate_admission_floor_evidence(evidence, selected)
            artifact = evidence["artifact"]
            admission_root = None
            artifact_path = artifact.get("path")
            if qualifying_receipt.admission_is_v2(selected):
                admission_root = canonical_ledger.repository_root().resolve()
                with AdmissionPaths(admission_root) as paths:
                    artifact_path = str(paths.path(artifact["locator"]))
            retained = read_admission_context(artifact_path, artifact["sha256"],
                                              state_root=admission_root)
            if retained != evidence:
                raise ValueError("selected admission evidence differs from retained artifact")
            verify_admission_context_git(evidence, hermit_checkout, sha, selected["log_file"],
                                         state_root=admission_root)
        except (ValueError, RuntimeError, OSError) as error:
            return {
                "sha": sha, "satisfied": False, "reason": "invalid-admission-floor",
                "detail": str(error), "exit_code": 1, "appended": 0,
            }

    fields: dict = {}
    if not _has_complete_schema5_measurement(selected):
        log = selected.get("log_file")
        if not isinstance(log, str) or not os.path.isfile(log):
            return {
                "sha": sha,
                "satisfied": False,
                "reason": "no-log",
                "exit_code": 1,
                "appended": 0,
            }
        if qualifying_receipt.admission_is_v2(selected):
            try:
                with AdmissionPaths(admission_root) as paths:
                    log_text = paths.read(evidence["context"]["log_identity"]).decode(
                        "utf-8", errors="replace")
            except (ValueError, OSError) as error:
                return {"sha": sha, "satisfied": False, "reason": "no-log",
                        "detail": str(error), "exit_code": 1, "appended": 0}
        else:
            with open(log, errors="replace") as log_handle:
                log_text = log_handle.read()
        try:
            planned = planned_test_nodes(hermit_checkout, sha)
        except RuntimeError as error:
            return {
                "sha": sha,
                "satisfied": False,
                "reason": "no-manifest",
                "detail": str(error),
                "exit_code": 1,
                "appended": 0,
            }
        if not planned:
            return {
                "sha": sha,
                "satisfied": False,
                "reason": "no-manifest",
                "detail": "canonical full validation plan has no test producers",
                "exit_code": 1,
                "appended": 0,
            }
        fields.update(build_coverage(log_text, planned))
    # This finalizer is Hermit-specific (Hermit manifests, Hermit commit, Hermit
    # Cargo.lock). Older Hermit rows predate the repo column, so carry that
    # already-bound scope into the schema-5 clone rather than manufacturing it
    # from an unchecked self-declaration.
    fields["repo"] = "hermit"
    try:
        fields.update(build_base_evidence(hermit_checkout, sha, reverie_checkout))
    except RuntimeError as error:
        return {
            "sha": sha,
            "satisfied": False,
            "reason": "no-base",
            "detail": str(error),
            "exit_code": 1,
            "appended": 0,
        }
    coverage = fields.get("coverage", selected.get("coverage"))
    executed_tests = fields.get("executed_tests", selected.get("executed_tests"))
    if not qualifying_receipt.coverage_satisfied(coverage):
        return {
            "sha": sha,
            "satisfied": False,
            "reason": "unsatisfied-coverage",
            "executed_tests": executed_tests,
            "planned_test_nodes": coverage["planned_test_nodes"],
            "exit_code": 1,
            "appended": 0,
        }

    clone = _clone_upgraded(selected, fields, selected_row_sha256)
    try:
        # This invokes the COMPLETE Rust canonical receipt predicate, which in
        # turn invokes the shared policy. Coverage alone cannot mint authority:
        # tree/raw_result/gates/admission/concurrency and every other canonical
        # condition must already be carried by the selected source or derived
        # exactly above.
        canonical_row_sha256(clone, sha, require_canonical_qualifying=True)
    except RuntimeError as error:
        return {
            "sha": sha,
            "satisfied": False,
            "reason": "insufficient-source-row",
            "detail": str(error),
            "executed_tests": executed_tests,
            "planned_test_nodes": coverage["planned_test_nodes"],
            "exit_code": 1,
            "appended": 0,
        }

    # Re-resolve the caller's identity and idempotency state at the exact
    # decision/append boundary. Existing-finalization handling deliberately
    # follows the complete source-row preflight above: a qualifying row that
    # merely claims an incomplete source can never launder that source into an
    # `already-finalized` success. A concurrent append can neither redirect the
    # request to another same-SHA row nor create a second clone of this source.
    if ledger_path is None:
        rows = canonical_ledger.rows(preserve_admission=True)
        _resolve_selected_row(rows, sha, selected_row_sha256)
        if _existing_finalization(rows, sha, selected_row_sha256) is not None:
            return {
                "sha": sha,
                "satisfied": True,
                "reason": "already-finalized",
                "exit_code": 0,
                "appended": 0,
            }
        if dry_run:
            return {
                "sha": sha,
                "satisfied": True,
                "reason": "would-mint",
                "executed_tests": executed_tests,
                "planned_test_nodes": coverage["planned_test_nodes"],
                "exit_code": 0,
                "appended": 0,
            }
        canonical_ledger.record(clone)
    else:
        with _locked_ledger(ledger_path, exclusive=True) as handle:
            rows = _load_rows(handle)
            _resolve_selected_row(rows, sha, selected_row_sha256)
            if _existing_finalization(rows, sha, selected_row_sha256) is not None:
                return {
                    "sha": sha,
                    "satisfied": True,
                    "reason": "already-finalized",
                    "exit_code": 0,
                    "appended": 0,
                }
            if dry_run:
                return {
                    "sha": sha,
                    "satisfied": True,
                    "reason": "would-mint",
                    "executed_tests": executed_tests,
                    "planned_test_nodes": coverage["planned_test_nodes"],
                    "exit_code": 0,
                    "appended": 0,
                }
            handle.seek(0, os.SEEK_END)
            handle.write(json.dumps(clone, separators=(",", ":")) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
    return {
        "sha": sha,
        "satisfied": True,
        "reason": "minted",
        "executed_tests": executed_tests,
        "planned_test_nodes": coverage["planned_test_nodes"],
        "exit_code": 0,
        "appended": 1,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--log", help="full safe-ci-dag-runner log ([node]-prefixed)")
    ap.add_argument("--sha", help="the exact 40-hex Hermit commit validated")
    ap.add_argument("--hermit-checkout", required=True,
                    help="hermit checkout to read ci/dag/*.json at --sha via git show")
    ap.add_argument(
        "--reverie-checkout",
        default=str(Path(os.environ.get("DEV_HERMIT_PARENT", Path(__file__).resolve().parents[2])) / "reverie"),
        help="Reverie checkout/object store used to bind the pinned commit tree",
    )
    ap.add_argument("--ledger", help=argparse.SUPPRESS)
    ap.add_argument(
        "--selected-row-sha256",
        help="Rust-canonical SHA-256 identity of the one source row to finalize",
    )
    ap.add_argument(
        "--select-candidate-sha256",
        action="store_true",
        help="read-only: print the newest exact-SHA finalizable source-row digest",
    )
    ap.add_argument("--emit-only", action="store_true",
                    help="print the schema-5 fields to stdout; do NOT touch a ledger")
    ap.add_argument("--admission-context", help="retained versioned context file (emit-only)")
    ap.add_argument("--admission-context-sha256", help="exact context byte digest (emit-only)")
    ap.add_argument("--admission-state-root", help="explicit v2 state root (emit-only)")
    ap.add_argument("--scan", action="store_true",
                    help="APPEND-safe mint of one --sha/--selected-row-sha256 source")
    ap.add_argument("--dry-run", action="store_true",
                    help="with --scan: report what would be minted; do NOT write")
    args = ap.parse_args(argv)

    if (args.admission_context is not None or args.admission_context_sha256 is not None
            or args.admission_state_root is not None):
        if (not args.emit_only or args.scan or args.select_candidate_sha256
                or args.admission_context is None or args.admission_context_sha256 is None):
            print("finalize_receipt: admission context requires both context flags and "
                  "--emit-only, not scan/selection", file=sys.stderr)
            return 2

    if args.ledger and os.environ.get("CI_HUB_LEDGER_FIXTURE_MODE") != "1":
        print("finalize_receipt: --ledger is test-fixture-only", file=sys.stderr)
        return 2

    if args.select_candidate_sha256:
        if args.scan or not args.sha:
            print(
                "finalize_receipt: --select-candidate-sha256 requires --sha, not --scan",
                file=sys.stderr,
            )
            return 2
        if args.ledger and not os.path.isfile(args.ledger):
            print(f"finalize_receipt: ledger not found: {args.ledger}", file=sys.stderr)
            return 2
        try:
            print(select_candidate_sha256(args.ledger, args.sha))
        except SelectedRowError as error:
            print(f"finalize_receipt: {error.reason}: {error}", file=sys.stderr)
            return 2
        return 0

    if args.scan:
        missing = [
            name
            for name, value in (
                ("--sha", args.sha),
                ("--selected-row-sha256", args.selected_row_sha256),
            )
            if not value
        ]
        if missing:
            print(
                f"finalize_receipt: --scan requires {', '.join(missing)}",
                file=sys.stderr,
            )
            return 2
        if args.ledger and not os.path.isfile(args.ledger):
            print(f"finalize_receipt: ledger not found: {args.ledger}", file=sys.stderr)
            return 2
        try:
            result = scan_and_finalize(
                args.ledger,
                args.hermit_checkout,
                args.sha,
                args.selected_row_sha256,
                args.reverie_checkout,
                dry_run=args.dry_run,
            )
        except SelectedRowError as error:
            print(f"finalize_receipt: {error.reason}: {error}", file=sys.stderr)
            return 2
        detail = f" detail={result['detail']}" if result.get("detail") else ""
        print(
            f"finalize_receipt: {result['reason']} sha={args.sha} "
            f"selected_row_sha256={args.selected_row_sha256} "
            f"appended={result['appended']} satisfied={str(result['satisfied']).lower()}"
            f"{detail}"
        )
        return int(result["exit_code"])

    if args.ledger and not args.emit_only:
        print(
            "finalize_receipt: in-place ledger rewrites are disabled; use --scan with "
            "--sha and --selected-row-sha256",
            file=sys.stderr,
        )
        return 2

    if not (args.log and args.sha):
        print("finalize_receipt: --log and --sha are required (or use --scan)",
              file=sys.stderr)
        return 2

    admission_evidence = None
    if args.admission_context is not None:
        try:
            if args.admission_state_root is not None:
                # This is configuration agreement, not independent live
                # authority. The driver holds that proof and checks our echo.
                if Path(args.admission_state_root) != canonical_ledger.repository_root().resolve():
                    raise ValueError("admission state root differs from canonical ledger state root")
            admission_evidence = read_admission_context(
                args.admission_context, args.admission_context_sha256,
                state_root=args.admission_state_root,
            )
            verify_admission_context_git(
                admission_evidence, args.hermit_checkout, args.sha, args.log,
                state_root=args.admission_state_root,
            )
        except (ValueError, RuntimeError, OSError) as error:
            print(f"finalize_receipt: invalid admission floor: {error}", file=sys.stderr)
            return 2

    try:
        if admission_evidence and admission_evidence["context"]["contract"] == qualifying_receipt.ADMISSION_CONTEXT_V2:
            with AdmissionPaths(args.admission_state_root) as paths:
                log_text = paths.read(admission_evidence["context"]["log_identity"]).decode(
                    "utf-8", errors="replace")
        else:
            with open(args.log, errors="replace") as fh:
                log_text = fh.read()
    except (OSError, ValueError) as exc:
        print(f"finalize_receipt: cannot read log {args.log!r}: {exc}", file=sys.stderr)
        return 2

    try:
        planned = planned_test_nodes(args.hermit_checkout, args.sha)
    except RuntimeError as exc:
        print(
            f"finalize_receipt: cannot read canonical validation plan: {exc}",
            file=sys.stderr,
        )
        return 2
    fields = build_coverage(log_text, planned)
    try:
        fields.update(build_base_evidence(
            args.hermit_checkout, args.sha, args.reverie_checkout
        ))
    except RuntimeError as exc:
        print(f"finalize_receipt: cannot record base evidence: {exc}", file=sys.stderr)
        return 2

    if admission_evidence is not None:
        fields["admission_floor_evidence"] = admission_evidence

    print(json.dumps({"commit": args.sha, **fields}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
