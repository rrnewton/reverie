#!/usr/bin/env python3
"""Extract validate failure evidence without deciding the verdict.

The shared Rust ``validate_status`` reader is the sole verdict authority. This
producer helper only binds a raw ledger row to observable facts: failed DAG
substeps, exact failed test/mode/backend cells, membership in the measured-flake
registry, and whether this exact commit/cell is a solo ``-j 4`` reproduction of
an earlier rerun-required red.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path
import sys
from typing import (
    Literal,
    Mapping,
    NotRequired,
    TypedDict,
    cast,
    get_args,
    get_origin,
    get_type_hints,
)


CI_HUB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import flaky_cells as flaky_registry  # noqa: E402

sys.path.insert(0, str(CI_HUB / "remediation"))
from nonzero_result import per_node_counts  # noqa: E402

sys.path.insert(0, str(CI_HUB / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402


DEFAULT_REGISTRY = Path(__file__).with_name("flaky-cells.json")
FAILURE_CLASS_SCHEMA = (
    Path(__file__).resolve().parents[2]
    / "hermit"
    / "ci"
    / "manifest-plan"
    / "failure-class-schema.json"
)


def _shared_failure_classes() -> frozenset[str]:
    try:
        value = json.loads(FAILURE_CLASS_SCHEMA.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"cannot read shared failure class schema {FAILURE_CLASS_SCHEMA}: {error}"
        ) from error
    if not isinstance(value, Mapping) or set(value) != {"schema", "values"}:
        raise RuntimeError("shared failure class schema has inexact fields")
    if value["schema"] != 1:
        raise RuntimeError(
            f"shared failure class schema has unsupported schema {value['schema']!r}"
        )
    values = value["values"]
    if (
        not isinstance(values, list)
        or not values
        or any(not isinstance(item, str) or not item for item in values)
        or len(set(values)) != len(values)
    ):
        raise RuntimeError("shared failure class schema values must be unique strings")
    return frozenset(values)


_SHARED_FAILURE_CLASSES = _shared_failure_classes()
_SHARED_TO_FAULT_CLASS = {
    "product_failure": "code",
    "understood_infrastructure_failure": "infrastructure",
    "understood_prerequisite_failure": "host-environment",
    "no_result": "no-result",
}
if frozenset(_SHARED_TO_FAULT_CLASS) != _SHARED_FAILURE_CLASSES:
    raise RuntimeError(
        "failure class mapping does not cover the shared failure class schema"
    )

# A timed-out node burning a full core for its whole allowance is a LIVELOCK; one
# that spent the allowance waiting is CONTENTION. They demand opposite fixes.
# These thresholds remain the pure classification policy, but node_timing does
# NOT apply them until the profile producer carries an observable run identity
# end to end. A checkout-local row selected only by {step, git_sha} can belong to
# an earlier run of the same commit, so using its elapsed/user/sys values would
# manufacture a verdict for the current run. The eventual producer must also
# carry the raw cgroup-v2 cpu.stat usage_usec counter; user_s + sys_s is not
# presented here as though it were that raw counter.
_LIVELOCK_CPU_PER_WALL = 0.9  # >= this: at least one core continuously busy
_CONTENTION_CPU_PER_WALL = 0.5  # <= this: mostly off-CPU, i.e. waiting
# cgroup throttling is DIRECT evidence of contention and outranks the ratio: a
# step can be throttled into a timeout while still showing a busy ratio.
_THROTTLED_SECONDS_FLOOR = 1.0


class FailedSubstepTiming(TypedDict):
    wall_seconds: float | None
    cpu_seconds: float | None
    cpu_user_seconds: float | None
    cpu_sys_seconds: float | None
    cpu_usage_usec: float | None
    cpu_source: str
    cpu_per_wall: float | None
    timed_out: bool | None
    cpu_timed_out: bool | None
    throttled_seconds: float | None
    quota_utilization_pct: float | None
    co_tenants_end: float | None
    timing_verdict: Literal["livelock", "contention", "inconclusive"] | None
    timing_source: str
    profile_rows_matched: int
    profile_candidate_rows: int
    profile_row_timestamp: str | None


class FailedSubstepClass(TypedDict):
    node: str
    group: str
    sub_step_class: Literal["dependency-build", "lane-run"]
    fault_class: Literal[
        "infrastructure", "host-environment", "code", "no-result"
    ]
    infra_signature: str | None
    first_error_line: str | None
    known_flaky: bool
    # Historical rows predate each additive field. Missing remains readable;
    # present values must satisfy the complete current shape.
    failure_class: NotRequired[
        Literal[
            "product_failure",
            "understood_infrastructure_failure",
            "understood_prerequisite_failure",
            "no_result",
        ]
    ]
    host_env_signature: NotRequired[str | None]
    timing: NotRequired[FailedSubstepTiming]


_FAILED_SUBSTEP_HINTS = get_type_hints(
    FailedSubstepClass,
    globalns=globals(),
    localns=globals(),
    include_extras=True,
)
_FAILED_SUBSTEP_FIELDS = frozenset(_FAILED_SUBSTEP_HINTS)
_FAILED_SUBSTEP_REQUIRED_FIELDS = frozenset(
    field
    for field, annotation in _FAILED_SUBSTEP_HINTS.items()
    if get_origin(annotation) is not NotRequired
)
_FAILED_SUBSTEP_TIMING_HINTS = get_type_hints(
    FailedSubstepTiming,
    globalns=globals(),
    localns=globals(),
)
_FAILED_SUBSTEP_TIMING_FIELDS = frozenset(_FAILED_SUBSTEP_TIMING_HINTS)
_SUB_STEP_CLASSES = frozenset(get_args(_FAILED_SUBSTEP_HINTS["sub_step_class"]))
_FAILED_SUBSTEP_FAULT_CLASSES = frozenset(
    get_args(_FAILED_SUBSTEP_HINTS["fault_class"])
)
_TIMING_VERDICTS = frozenset(
    value
    for member in get_args(_FAILED_SUBSTEP_TIMING_HINTS["timing_verdict"])
    for value in get_args(member)
)


def _optional_string(value: object) -> bool:
    return value is None or isinstance(value, str) and bool(value)


def _optional_number(value: object) -> bool:
    return value is None or (
        isinstance(value, (int, float)) and not isinstance(value, bool)
    )


def _validate_failed_substep_timing(
    value: object, index: int
) -> FailedSubstepTiming:
    if not isinstance(value, Mapping):
        raise ValueError(f"failed_substep_classes[{index}].timing must be an object")
    fields = frozenset(value)
    if fields != _FAILED_SUBSTEP_TIMING_FIELDS:
        missing = sorted(_FAILED_SUBSTEP_TIMING_FIELDS - fields)
        unknown = sorted(fields - _FAILED_SUBSTEP_TIMING_FIELDS)
        raise ValueError(
            f"failed_substep_classes[{index}].timing has inexact fields; "
            f"missing={missing} unknown={unknown}"
        )
    for field in (
        "wall_seconds",
        "cpu_seconds",
        "cpu_user_seconds",
        "cpu_sys_seconds",
        "cpu_usage_usec",
        "cpu_per_wall",
        "throttled_seconds",
        "quota_utilization_pct",
        "co_tenants_end",
    ):
        if not _optional_number(value[field]):
            raise ValueError(
                f"failed_substep_classes[{index}].timing.{field} "
                "must be a number or null"
            )
    for field in ("timed_out", "cpu_timed_out"):
        if value[field] is not None and not isinstance(value[field], bool):
            raise ValueError(
                f"failed_substep_classes[{index}].timing.{field} "
                "must be boolean or null"
            )
    for field in ("cpu_source", "timing_source"):
        if not isinstance(value[field], str) or not value[field]:
            raise ValueError(
                f"failed_substep_classes[{index}].timing.{field} "
                "must be a non-empty string"
            )
    verdict = value["timing_verdict"]
    if verdict is not None and verdict not in _TIMING_VERDICTS:
        raise ValueError(
            f"failed_substep_classes[{index}].timing.timing_verdict has unknown value "
            f"{verdict!r}"
        )
    for field in ("profile_rows_matched", "profile_candidate_rows"):
        count = value[field]
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise ValueError(
                f"failed_substep_classes[{index}].timing.{field} "
                "must be a non-negative integer"
            )
    if not _optional_string(value["profile_row_timestamp"]):
        raise ValueError(
            f"failed_substep_classes[{index}].timing.profile_row_timestamp "
            "must be a non-empty string or null"
        )
    return cast(FailedSubstepTiming, dict(value))


def failed_substep_classes_from_row(
    row: Mapping[str, object],
) -> list[FailedSubstepClass] | None:
    """Read the producer-owned per-node evidence, or report that it is absent.

    Missing is the historical state and permits a log fallback. A present field
    is an assertion by the producer: malformed content refuses by the field name
    instead of disappearing into the same empty result as absent evidence.
    """
    if "failed_substep_classes" not in row:
        return None
    value = row["failed_substep_classes"]
    if not isinstance(value, list):
        raise ValueError("failed_substep_classes must be a list")
    result: list[FailedSubstepClass] = []
    for index, item in enumerate(value):
        if not isinstance(item, Mapping):
            raise ValueError(f"failed_substep_classes[{index}] must be an object")
        fields = frozenset(item)
        missing = sorted(_FAILED_SUBSTEP_REQUIRED_FIELDS - fields)
        unknown = sorted(fields - _FAILED_SUBSTEP_FIELDS)
        if missing or unknown:
            raise ValueError(
                f"failed_substep_classes[{index}] has inexact fields; "
                f"missing={missing} unknown={unknown}"
            )
        if not isinstance(item["node"], str) or not item["node"]:
            raise ValueError(
                f"failed_substep_classes[{index}].node must be a non-empty string"
            )
        if not isinstance(item["group"], str):
            raise ValueError(f"failed_substep_classes[{index}].group must be a string")
        if item["sub_step_class"] not in _SUB_STEP_CLASSES:
            raise ValueError(
                f"failed_substep_classes[{index}].sub_step_class has unknown value "
                f"{item['sub_step_class']!r}"
            )
        if item["fault_class"] not in _FAILED_SUBSTEP_FAULT_CLASSES:
            raise ValueError(
                f"failed_substep_classes[{index}].fault_class has unknown value "
                f"{item['fault_class']!r}"
            )
        if (
            "failure_class" in item
            and item["failure_class"] not in _SHARED_FAILURE_CLASSES
        ):
            raise ValueError(
                f"failed_substep_classes[{index}].failure_class has unknown value "
                f"{item['failure_class']!r}"
            )
        for field in ("infra_signature", "first_error_line", "host_env_signature"):
            if field in item and not _optional_string(item[field]):
                raise ValueError(
                    f"failed_substep_classes[{index}].{field} "
                    "must be a non-empty string or null"
                )
        if not isinstance(item["known_flaky"], bool):
            raise ValueError(
                f"failed_substep_classes[{index}].known_flaky must be boolean"
            )
        parsed = dict(item)
        if "timing" in item:
            parsed["timing"] = _validate_failed_substep_timing(item["timing"], index)
        result.append(cast(FailedSubstepClass, parsed))
    return result


def gate_failure_classes_from_row(
    row: Mapping[str, object],
) -> dict[str, tuple[str, str | None]] | None:
    """Read current producer-owned gate classes, or identify a historical row.

    No failed gate carrying the field means the row predates the contract and
    may use the historical log classifier. Once one failed gate carries it,
    every failed gate must carry a recognized value; partial current evidence
    refuses instead of letting an untyped sibling default to ``code``.
    """
    gates = row.get("gates")
    if not isinstance(gates, list):
        return None
    failed: list[Mapping[str, object]] = []
    for index, gate in enumerate(gates):
        if not isinstance(gate, Mapping):
            continue
        outcome = str(gate.get("result") or gate.get("kind") or "")
        if gate.get("aborted") is True or outcome not in {
            "fail",
            "failed",
            "timeout",
            "no_result",
            "no-result",
        }:
            continue
        name = gate.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError(f"gates[{index}].name must be a non-empty string")
        failed.append(gate)
    if not failed or not any("failure_class" in gate for gate in failed):
        return None

    classes: dict[str, tuple[str, str | None]] = {}
    for gate in failed:
        name = cast(str, gate["name"])
        failure_class = gate.get("failure_class")
        if failure_class not in _SHARED_FAILURE_CLASSES:
            raise ValueError(
                f"gate {name!r} failure_class has unknown value {failure_class!r}"
            )
        detail = gate.get("failure_detail")
        if detail is not None and (not isinstance(detail, str) or not detail):
            raise ValueError(
                f"gate {name!r} failure_detail must be a non-empty string or absent"
            )
        if name in classes:
            raise ValueError(f"duplicate failed gate failure_class for {name!r}")
        classes[name] = (cast(str, failure_class), cast(str | None, detail))
    return classes


def failed_substep_classes_from_gates(
    row: Mapping[str, object], *, flaky_registry: set[str] | None = None
) -> list[FailedSubstepClass] | None:
    """Project current typed gate classes without requiring a retained log."""
    classes = gate_failure_classes_from_row(row)
    if classes is None:
        return None
    return classify_failed_substeps(
        "",
        flaky_registry=flaky_registry,
        commit=str(row.get("commit") or ""),
        failed_nodes=sorted(classes),
        failure_classes=classes,
    )


def flaky_cells(path: Path, commit: str | None = None) -> set[str]:
    """Exact cell ids measured both passing and diverging at ``commit``."""
    registry = flaky_registry.load_registry(path)
    if commit is None:
        return {
            str(entry["cell"])
            for entry in registry["cells"]
            if isinstance(entry, Mapping)
        }
    return flaky_registry.cells_for_tree(registry, commit)


def failed_cells(cell_results: object) -> tuple[bool, list[str], list[str]]:
    """Return (evidence present, diverged cells, cells without a comparison)."""
    if not isinstance(cell_results, Mapping):
        return False, [], []
    cells = cell_results.get("cells")
    if not isinstance(cells, list):
        return False, [], []
    diverged: set[str] = set()
    unavailable: set[str] = set()
    for cell in cells:
        if not isinstance(cell, Mapping):
            continue
        values = [cell.get(key) for key in ("test", "mode", "backend")]
        if any(not isinstance(value, str) or not value for value in values):
            continue
        identity = "/".join(str(value) for value in values)
        verdict = cell.get("cell_verdict")
        state = verdict.get("state") if isinstance(verdict, Mapping) else None
        if state == "compared-and-diverged":
            diverged.add(identity)
        elif state == "unavailable-with-reason":
            unavailable.add(identity)
    return True, sorted(diverged), sorted(unavailable)


def observed_failed_cells(row: Mapping[str, object]) -> set[str]:
    """Exact failed cell ids persisted in a ledger row, or empty if absent."""
    cells = row.get("failed_cells")
    if not isinstance(cells, list):
        return set()
    return {str(cell) for cell in cells if isinstance(cell, str) and cell}


def _manifest_result_node(node: str) -> bool:
    group, separator, job = node.partition(".")
    return (
        separator == "."
        and group in {"e2e", "privileged-e2e"}
        and job.startswith("manifest_")
    )


def ledger_rows(path: Path | None) -> list[dict[str, object]]:
    if path is None or not path.is_file():
        return []
    rows = []
    with path.open(errors="replace") as handle:
        for line in handle:
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                rows.append(value)
    return rows


def failed_substeps(log_text: str) -> list[str]:
    return sorted(
        name
        for name, details in per_node_counts(log_text).items()
        if details.get("terminal") == "fail"
    )


# safe-ci-dag-runner tags every node ``<group>.<job>`` (see hermit/ci/dag/*.json).
# The GROUP already names WHICH sub-step failed, so a triager never has to read
# the aggregate lane name (`portable CI DAG manifest`) that today covers three
# unrelated causes. Groups whose failure is a BUILD/preparation step rather than a
# product test — a corrupt/missing artifact here is an INFRASTRUCTURE fault, not a
# code defect. (portable.json groups: check, setup, build, e2e, test, lint, doc.)
_BUILD_GROUPS = frozenset({"build", "setup"})

# Log signatures that make a failure an INFRASTRUCTURE fault (a corrupt/missing
# build artifact or a shared-cache fault), NOT a code defect. Each is a
# toolchain/linker/archive/cache message the PRODUCT under test cannot emit as a
# test assertion. To keep a guest that merely PRINTS these words from forging the
# class, a signature is only honored when it appears on a line attributed (by the
# ``[<node.tag>] `` stream prefix) to the SAME failing node — a test node's own
# stdout is never scanned for another node's infra signature.
_INFRA_SIGNATURES = (
    "in archive is not an object",   # corrupt static lib (e.g. libdynamorio_static.a)
    "archive has no index",          # ar/ranlib index corruption
    "malformed archive",             # truncated/garbled .a
    "file format not recognized",    # linker on a corrupt object
    "bad file descriptor while reading archive",
    "failed to verify the checksum", # cargo cache poisoning (specific, before generic)
    # CMakeCache relocation: a build dir/cache inherited from ANOTHER checkout
    # (e.g. a reflink/CoW seed) whose absolute paths no longer match this tree.
    # This is a stale-state INFRASTRUCTURE fault, not a source defect.
    "is different than the directory",  # "current CMakeCache.txt directory X is different than the directory Y"
    "does not match the source",        # CMAKE_HOME_DIRECTORY / cache dir mismatch
    "corrupted",                     # generic cargo/registry cache corruption tell
    # DynamoRIO third-party build/link fault: a stale / ABI-mismatched DynamoRIO
    # tree (reflink seed or partial rebuild) fails to link its drmemtrace op_*
    # symbols. Scoped to the `dynamorio::` namespace so a genuine PRODUCT link
    # error stays a code fault — a bare "undefined reference" is NOT a signature.
    "undefined reference to `dynamorio::",
    # Harness could not exec the target binary: GNU coreutils (`timeout`, `env`, …)
    # print "<prog>: failed to run command '<path>': No such file or directory"
    # (exit 127) when target/debug/hermit was never built or sits at a stale path.
    # An ENVIRONMENT/harness fault, not a product test assertion. Anchored to the
    # coreutils "<prog>: failed to run command" shape (leading ": ") so product
    # text merely containing "failed to run command" is less likely to match.
    ": failed to run command",
)

_NODE_PREFIX_RE = per_node_counts.__globals__["_NODE_PREFIX_RE"]

# The e2e harness's own PREPARATION-failure markers (hermit/ci/test_harness.sh).
# A prepare/compile step that fails prints one of these naming the GUEST, then
# `cat`s the guest's stderr as the next line(s). The marker is therefore the
# anchor for the CAUSAL line, and it is what a `build.manifest_guests` red has
# instead of a toolchain `error:` diagnostic. Without this, such a node falls all
# the way through to its own terminal `✗ FAIL <lane> (…, exit 1)` summary, which
# names no guest and no cause — the #1711 defect.
_PREPARE_FAILURE_MARKERS = (
    "prepare failed for ",
    "C program compilation failed for ",
    "Rust program compilation failed for ",
)

# Lines the harness emits for guests that SUCCEEDED, plus the markers themselves.
# Used to bound a prepare failure's reason: the reason is the guest stderr that
# the harness `cat`s immediately after the marker, so it ends at the next marker
# or the next per-guest progress line.
_PREPARE_PROGRESS_PREFIXES = ("BUILT ", "SKIP ", "PREBUILT ")

# Host/environment signatures that a PRODUCT test cannot plausibly emit, so they
# are honored anywhere on the failing node's own stream. Each names a provisioning
# gap — a package or command the HOST does not provide — which is fixed by
# provisioning the host, never by editing product code.
_HOST_ENV_STRONG_SIGNATURES = (
    ": command not found",            # shell: `foo: command not found`
    "unable to locate package",       # apt-get
    "has no installation candidate",  # apt-get
    "no match for argument",          # dnf/yum
    "executable file not found in $path",
)

# Weaker host/environment signatures. These shapes DO occur in ordinary product
# output ("expected key not found"), so they are honored ONLY on a prepare-failure
# REASON line — guest stderr the harness captured while preparing a fixture, where
# the subject can only be a host tool. This is the bias the module already
# declares, applied to the new class: a real code defect is never hidden as an
# environment fault, so a generic phrase never reclassifies a test node's own
# assertion output.
_HOST_ENV_REASON_SIGNATURES = (
    "not found",
    "no such file or directory",
    "command not found",
    # "no Lua interpreter on PATH (tried: lua5.4, lua)" — a reason that names
    # PATH is talking about host tool visibility by construction. Included
    # because a guest reworded from "lua5.4 not found" to the more accurate
    # PATH phrasing silently fell back to `code`: keying on prose means a
    # message edit can flip a class. See TaskGraph
    # `validate_classifier_keys_fault` for the durable structured-marker fix;
    # until then this list must track the phrasings guests actually use.
    " on path",
)

# An error-DIAGNOSTIC line, as emitted by rustc/cargo/gcc/clang/ld/collect2. Two
# shapes: a body that STARTS with ``error:`` / ``error[E0432]:`` (rustc, cargo),
# and a tool/file-prefixed ``<something>: error:`` (``collect2: error:``,
# ``ld: error:``, ``<file>:<line>: error:``). This is matched against a node's own
# stream line AFTER the ``[tag] `` prefix is stripped, so it is a FACT about the
# failing node, not a proxy — it records verbatim what the toolchain printed.
_ERROR_LINE_RE = re.compile(r"(?:^|[:\s])error(?:\[[^\]]*\])?:\s")

# Cargo's build-ORCHESTRATION envelopes: cargo prints one of these for EVERY
# build-script / compile / doc failure, then re-prints the real diagnostic the
# child emitted (under ``Caused by:`` / ``--- stderr``). The envelope names only
# the crate, never the cause, so two totally unrelated failures (a corrupt
# DynamoRIO archive vs a stale ``--locked`` Cargo.lock) both surface as
# ``error: failed to run custom build command for <crate>``. ``first_error_line``
# therefore skips these envelopes to reach the first SUBSTANTIVE error line,
# falling back to the envelope only when the node emitted nothing more specific.
# Harness ENVELOPES: the exact analogue of the cargo envelopes below, for the TEST
# RUNNER. ``error: test run failed`` is cargo-nextest's orchestration line. It is
# error-SHAPED, so ``_ERROR_LINE_RE`` matches it and — because a real compile/link
# ``error:`` outranks the harness fallback — it WINS, and the row is attributed to a
# string that names nothing. Measured 2026-08-25 on validate 4e168f2aa5b9: of ten
# failing nodes, the only two with an interpretable ``first_error_line`` were the two
# that do NOT run under nextest; the other EIGHT all read ``error: test run failed``.
# Classifying those eight as bar-raise or regression was blocked on exactly this.
# Treating it as an envelope lets ``_first_harness_failure_for`` reach the per-case
# ``FAIL [ 0.03s] crate::suite test_name`` line beneath it, which names the test.
_HARNESS_ENVELOPES = (
    "error: test run failed",
)

_CARGO_ENVELOPES = (
    "error: failed to run custom build command for ",
    "error: could not compile ",
    "error: failed to compile ",
    "error: could not document ",
    "error: build failed",
)

# Linker-driver ENVELOPES: the exact analogue of the cargo envelopes above for the
# LINK step. ``collect2: error: ld returned 1 exit status`` (and the bare
# ``error: ld returned N exit status``) is emitted for EVERY link failure
# regardless of cause, so two totally unrelated link failures share it verbatim —
# it names no symbol and no library. ``first_error_line`` therefore treats it as an
# envelope and skips it to reach the substantive ``undefined reference`` diagnostic
# below, falling back to it only when the node emitted nothing more specific.
_LINK_ENVELOPES = (
    "collect2: error:",
    "error: ld returned ",
)

# A linker symbol-resolution diagnostic. GNU ld prints
# ``<site>: undefined reference to `<symbol>'`` with NO ``error:`` token, so
# ``_ERROR_LINE_RE`` alone never sees it — yet it is the single most interpretable
# fact a link failure carries: WHICH symbol is unresolved, hence WHICH library.
# It is recognized as a substantive error line so it wins over the generic
# collect2/ld envelopes and lets two DynamoRIO link failures with different missing
# symbols be told apart from the ledger row alone (the task's "error string is a
# fact" for the fedc81ed DynamoRIO fault). ``undefined reference to`` in the log
# stream is a toolchain diagnostic, never product test output.
_LINKER_DIAG_SUBSTR = "undefined reference to "

# The safe-ci-dag runner's OWN per-node failure lines. A test/e2e/lint/doc node
# rarely emits a rustc/gcc ``error:`` line — it fails via the harness's verdict
# output instead, so ``_is_error_diagnostic`` sees nothing and the node's
# ``first_error_line`` was left None (25% of red nodes: e2e.metadata,
# detcore_misc, strict_compat, rustfmt, manifest buckets, dbi_parity …), making
# those reds gate-name-only once the /tmp log is evicted. Two verbatim shapes,
# both node-scoped stream text (never a proxy):
#   * a manifest/parity PER-CASE verdict — ``FAIL  portable custom liteinst
#     system-utils/clock-determinism - custom runs=5 failed_runs=1 distinct=2`` /
#     ``FAIL dbi/file_metadata: ... expected=...`` — naming the exact failing
#     case AND its detail; and
#   * the node's terminal SUMMARY — ``✗ FAIL <desc> (<time>, <disposition>)`` /
#     ``❌ <desc> (… exit 137: …)`` — whose disposition (``exit N`` / ``TIMEOUT`` /
#     ``CPU-TIMEOUT``) is the interpretable cause when a lane was KILLED (an
#     ``exit 137`` OOM-kill, a wall/cpu timeout) rather than asserting.
# Used ONLY as a fallback when the node emitted no toolchain diagnostic, so a real
# compile/link ``error:`` always wins; and ``^FAIL[ \t]`` deliberately excludes
# ``FAILED`` (no delimiter follows) so rust's ``test result: FAILED`` summary is
# not mistaken for a per-case verdict.
# ``^[ \t]*`` because cargo-nextest INDENTS its per-case verdicts
# (``        FAIL [   0.030s] crate::suite test_name``) while the manifest/parity
# harnesses emit theirs at column 0. Anchored at column 0 this pattern could not
# see a nextest failure at all. ``[ \t\[]`` keeps the original author's reason for
# requiring a delimiter: ``FAILED`` still does not match, so rust's
# ``test result: FAILED`` summary is still not mistaken for a per-case verdict.
_HARNESS_CASE_FAIL_RE = re.compile(r"^[ \t]*FAIL[ \t\[]")
_HARNESS_SUMMARY_MARKERS = ("✗ FAIL", "❌")

# Ledger rows must stay bounded; a path- or URL-bearing error line can be long.
_FIRST_ERROR_LINE_CAP = 500


def _bounded_error_line(line: str) -> str:
    if len(line) <= _FIRST_ERROR_LINE_CAP:
        return line
    return line[: _FIRST_ERROR_LINE_CAP - 1] + "…"


def _is_error_diagnostic(line: str) -> bool:
    """Whether ``line`` (already stream-prefix-stripped) is a toolchain error
    diagnostic worth surfacing: an ``error:``/``error[E…]:`` line from
    rustc/cargo/gcc/clang/ld, OR a GNU-ld ``undefined reference to `<symbol>'``
    line, which carries no ``error:`` token yet is the substantive cause of a link
    failure."""
    return bool(_ERROR_LINE_RE.search(line)) or _LINKER_DIAG_SUBSTR in line


def _is_generic_envelope(line: str) -> bool:
    """Whether ``line`` is a build-ORCHESTRATION envelope that names only the
    crate/step, never the cause — cargo's ``failed to run custom build command``
    family or the linker driver's ``collect2: error: ld returned N``. These are
    skipped so ``first_error_line`` reaches the substantive diagnostic beneath
    them, and used only as a last-resort fallback."""
    if any(line.startswith(env) for env in _CARGO_ENVELOPES):
        return True
    if _is_harness_envelope(line):
        return True
    return any(env in line for env in _LINK_ENVELOPES)


def _is_harness_envelope(line: str) -> bool:
    """Whether ``line`` is a TEST-RUNNER orchestration envelope.

    Distinguished from the cargo and linker envelopes because it is WEAKER STILL.
    A cargo envelope names the crate and a linker envelope names the link step --
    weak facts, but facts, so they are worth returning when nothing better exists.
    ``error: test run failed`` NAMES NOTHING AT ALL. Returning it is strictly worse
    than having emitted no toolchain diagnostic, because it OUTRANKS the per-case
    harness verdict sitting directly beneath it, which does name the failing test.
    So this predicate is used to keep such a line from becoming the weak fallback
    at all -- see ``_first_error_line_for``."""
    return any(line.startswith(env) for env in _HARNESS_ENVELOPES)


def _first_error_line_for(log_text: str, node: str) -> str | None:
    """The first SUBSTANTIVE error-diagnostic line on ``node``'s own stream, or
    None when the node emitted no error-shaped line at all.

    Order-preserving and node-scoped: iterate the node's lines in emission order,
    skip generic build-orchestration envelopes (cargo's ``failed to run custom
    build command`` family and the linker's ``collect2: error: ld returned N``),
    and return the first remaining substantive error line verbatim
    (leading/trailing stream whitespace trimmed, length-capped). A GNU-ld
    ``undefined reference to `<symbol>'`` line counts as substantive even though it
    lacks an ``error:`` token — it names the unresolved symbol (hence the library),
    the actual fact behind a link failure. If every error line the node emitted IS
    an envelope, the first envelope is returned rather than None — the crate name
    is still a fact, just a weaker one. Returns None only when the node emitted no
    error line: a triager then falls back to the sub-step/fault classification,
    never to a fabricated string. This is the interpretable "error string is a
    fact" field — it lets two failures sharing one gate name AND one failing node
    (``build.runtime_release`` dying on a truncated-object DynamoRIO link failure
    vs a stale ``--locked`` Cargo.lock) be told apart from the ledger row alone,
    without reopening the log. When the node emitted no toolchain diagnostic at all
    (a test/e2e/lint lane that failed via the harness verdict, not a compiler),
    fall back to the DAG runner's own verbatim failure line
    (``_first_harness_failure_for``) so the row is still attributable; only a node
    that emitted NEITHER a toolchain nor a harness failure line returns None."""
    fallback: str | None = None
    for body in _node_lines(log_text, node):
        line = body.strip()
        if not _is_error_diagnostic(line):
            continue
        # A harness envelope must not become the fallback: it names nothing, and
        # taking it would pre-empt `_first_harness_failure_for` below, which can
        # name the failing test. Cargo/link envelopes still qualify -- they name
        # a crate or the link step.
        if fallback is None and not _is_harness_envelope(line):
            fallback = line
        if not _is_generic_envelope(line):
            return _bounded_error_line(line)
    if fallback is not None:
        return _bounded_error_line(fallback)
    cause = _prepare_failure_line_for(log_text, node)
    if cause is not None:
        return _bounded_error_line(cause)
    return _first_harness_failure_for(log_text, node)


def _prepare_failures_for(log_text: str, node: str) -> list[tuple[str, str | None]]:
    """Every ``(marker_line, reason_line_or_None)`` preparation failure on
    ``node``'s own stream, in emission order.

    The e2e harness prints ``prepare failed for <guest>`` (or the C/Rust compile
    analogue) and then `cat`s the guest's captured stderr, so the CAUSE is the
    line immediately following the marker on the SAME node's stream. Both halves
    are verbatim node output — nothing is synthesized. The reason is taken only
    when the very next node line is neither another marker nor a per-guest
    progress line (``BUILT``/``SKIP``/``PREBUILT``), which is exactly the case
    where the guest wrote nothing to stderr: that failure is genuinely
    unattributable from the log and is reported as ``(marker, None)`` rather than
    silently borrowing the next guest's text."""
    lines = [body.strip() for body in _node_lines(log_text, node)]
    out: list[tuple[str, str | None]] = []
    for index, line in enumerate(lines):
        if not any(line.startswith(marker) for marker in _PREPARE_FAILURE_MARKERS):
            continue
        reason: str | None = None
        nxt = lines[index + 1] if index + 1 < len(lines) else None
        if (
            nxt
            and not any(nxt.startswith(m) for m in _PREPARE_FAILURE_MARKERS)
            and not any(nxt.startswith(p) for p in _PREPARE_PROGRESS_PREFIXES)
        ):
            reason = nxt
        out.append((line, reason))
    return out


def _prepare_failure_line_for(log_text: str, node: str) -> str | None:
    """The first preparation failure on ``node``, rendered as the causal line.

    ``"prepare failed for language-runtimes/lua-random.sh: lua5.4 not found"`` —
    the marker (WHICH guest) joined to the guest's own stderr (WHY), both
    verbatim, separated by ``": "``. When the guest wrote no stderr the marker is
    returned alone: it still names the failing guest, which the node's terminal
    ``✗ FAIL <lane> (…, exit 1)`` summary does not. Ranked ABOVE that summary in
    ``_first_error_line_for`` because a summary line carries no cause at all —
    keying a triage decision on it is the proxy this function exists to remove."""
    failures = _prepare_failures_for(log_text, node)
    if not failures:
        return None
    marker, reason = failures[0]
    return f"{marker}: {reason}" if reason else marker


def _first_harness_failure_for(log_text: str, node: str) -> str | None:
    """The DAG runner's own first failure line on ``node``'s stream, or None.

    Fallback for a node that failed without a toolchain ``error:`` line. Prefers a
    manifest/parity PER-CASE verdict (``FAIL  <…> <case> - <detail>`` /
    ``FAIL dbi/<case>: …``) — the most specific fact, naming the failing case —
    returning the FIRST one in emission order; otherwise the node's terminal
    ``✗ FAIL``/``❌`` SUMMARY, which at least carries the disposition (``exit N`` /
    ``TIMEOUT`` / ``CPU-TIMEOUT``). All matched text is verbatim node stream output,
    length-capped, never fabricated."""
    summary: str | None = None
    for body in _node_lines(log_text, node):
        line = body.strip()
        if _HARNESS_CASE_FAIL_RE.match(line):
            return _bounded_error_line(line)
        if summary is None and any(line.startswith(m) for m in _HARNESS_SUMMARY_MARKERS):
            summary = line
    return _bounded_error_line(summary) if summary is not None else None


def _node_lines(log_text: str, node: str) -> list[str]:
    """Lines of the merged DAG stream attributed to ``node`` by its ``[tag] ``
    prefix. Attribution is by the EXACT node tag, so one node's stdout can never
    be read as another node's evidence."""
    out = []
    for raw in log_text.splitlines():
        m = _NODE_PREFIX_RE.match(raw)
        if m and m.group(1) == node:
            out.append(m.group(2))
    return out


# === The retained structured fault marker ===
#
# A substring of a human-readable sentence is a PROXY for "this failure is a host
# provisioning gap". Nothing binds a guest's wording to this classifier's
# expectation, so two independently-correct edits can combine into a regression:
# rewording a guest's reason from "lua5.4 not found" to the more accurate "no Lua
# interpreter on PATH (tried: lua5.4, lua)" removed the substring `not found` and
# silently flipped that cell from `host-environment` to `code`. The producer-side
# improvement DEFEATED the consumer-side fix.
#
# Retained logs can carry this older contract:
#
#     FAULT <class>: <detail>
#
# on its own line within the failing node's stream. Current producers instead
# write `gates[].failure_class` and optional `failure_detail` directly into the
# ledger row. This marker remains only so retained logs written before that field
# stay attributable; it has no current write path.
#
# TRUST BOUNDARY, stated rather than glossed: this module cannot distinguish a
# historical harness marker from the same line printed by a guest. That is why
# the retained path earns attribution only for rows without the current typed
# field, and why no new producer should emit this text form.
_FAULT_CLASSES = tuple(sorted(_FAILED_SUBSTEP_FAULT_CLASSES))
_FAULT_MARKER_RE = re.compile(
    r"^FAULT\s+(" + "|".join(_FAULT_CLASSES) + r")\s*:\s*(.*)$"
)


def _fault_marker_for(log_text: str, node: str) -> tuple[str, str] | None:
    """The structured ``FAULT <class>: <detail>`` marker for ``node``, or None.

    Returns ``(class, detail)``. First marker wins, so a producer cannot change a
    verdict by appending a second one. Absence is the normal case for every log
    written before the marker existed, which is why the prose path below is
    retained as a legacy fallback rather than deleted."""
    for line in _node_lines(log_text, node):
        match = _FAULT_MARKER_RE.match(line.strip())
        if match:
            return match.group(1), match.group(2).strip()
    return None


def _infra_signature_for(log_text: str, node: str) -> str | None:
    """The first infrastructure signature seen on ``node``'s own lines, or None.
    Case-insensitive; returns the canonical signature string (not the raw line)."""
    hay = "\n".join(_node_lines(log_text, node)).lower()
    for sig in _INFRA_SIGNATURES:
        if sig in hay:
            return sig
    return None


def _host_env_signature_for(log_text: str, node: str) -> str | None:
    """The first host/environment signature for ``node``, or None.

    Two tiers, deliberately asymmetric. A STRONG signature (``: command not
    found``, apt/dnf "unable to locate package", …) is scanned across the node's
    whole stream: no product test emits those. A WEAK signature (``not found``,
    ``no such file or directory``) is honored ONLY on a preparation-failure REASON
    line — guest stderr the harness captured while preparing a fixture, where the
    missing subject can only be a host tool. A test node that prints "expected key
    not found" in its own assertion output therefore stays ``code``.

    Returns the canonical signature string, not the raw line; the raw line is
    already carried verbatim by ``first_error_line``."""
    hay = "\n".join(_node_lines(log_text, node)).lower()
    for sig in _HOST_ENV_STRONG_SIGNATURES:
        if sig in hay:
            return sig
    for _marker, reason in _prepare_failures_for(log_text, node):
        if reason is None:
            continue
        lowered = reason.lower()
        for sig in _HOST_ENV_REASON_SIGNATURES:
            if sig in lowered:
                return sig
    return None


def checkout_root(log_text: str) -> Path | None:
    """Recover the checkout the run executed in, from the log's own header.

    The runner's profile store is CWD-relative (``DEFAULT_PROFILE_DIR``), so it
    lives inside the checkout. Reading the root from the log means the timing
    lookup binds to THIS run's own store without threading a new argument
    through the validation driver.
    """
    match = re.search(r"^Root: (.+)$", log_text, re.M)
    if not match:
        return None
    root = match.group(1).strip()
    return Path(root) if root else None


# The runner's per-checkout profile store, in both spellings. agent-utils
# renamed it .safe-ci-dag-runner/profiles -> .dagrun/profiles, and this parent
# reads checkouts on both sides of that rename, so it must look in both. Reading
# only one name yields an empty row set, which is indistinguishable from "the run
# recorded no profiles" and quietly drops the evidence rather than failing.
PROFILE_STORE_DIRS = (".dagrun", ".safe-ci-dag-runner")


def _profile_rows(root: Path) -> list[dict[str, str]]:
    """Read every step-profile row in a checkout's own runner profile store."""
    rows: list[dict[str, str]] = []
    for store in PROFILE_STORE_DIRS:
        profiles = root / store / "profiles"
        if not profiles.is_dir():
            continue
        for path in sorted(profiles.glob("step_profiles_*.csv")):
            try:
                with path.open(newline="", errors="replace") as handle:
                    rows.extend(dict(r) for r in csv.DictReader(handle))
            except OSError:
                continue
    return rows


def _number(raw: object) -> float | None:
    text = str(raw or "").strip()
    if not text:
        return None
    try:
        return float(text)
    except ValueError:
        return None


def _flag(raw: object) -> bool | None:
    text = str(raw or "").strip().lower()
    if text in {"true", "1", "yes"}:
        return True
    if text in {"false", "0", "no"}:
        return False
    return None


def timing_verdict(
    *, timed_out: bool | None, cpu_per_wall: float | None, throttled_s: float | None
) -> str | None:
    """Name WHY a node ran out of time, or refuse to.

    Only meaningful for a node that actually timed out; a node that failed for
    any other reason gets ``None`` rather than a guess. Returns one of
    ``livelock`` / ``contention`` / ``inconclusive``.
    """
    if not timed_out:
        return None
    if throttled_s is not None and throttled_s >= _THROTTLED_SECONDS_FLOOR:
        # The cgroup measurably stopped it running. That is contention by direct
        # observation, not inferred from a ratio.
        return "contention"
    if cpu_per_wall is None:
        return "inconclusive"
    if cpu_per_wall >= _LIVELOCK_CPU_PER_WALL:
        return "livelock"
    if cpu_per_wall <= _CONTENTION_CPU_PER_WALL:
        return "contention"
    return "inconclusive"


def node_timing(rows: list[dict[str, str]], node: str, commit: str) -> dict[str, object]:
    """Return typed-unavailable timing until profiles carry a run identity.

    ``step`` plus ``git_sha`` is not a causal binding to the validation whose
    log is being classified: the same checkout can append many runs of one
    commit. Therefore even one apparent candidate is refused, and multiple
    candidates are never resolved by file order. ``profile_candidate_rows`` is
    diagnostic evidence of that ambiguity; ``profile_rows_matched`` stays zero
    because no row matched an observable end-to-end run identity.
    """
    candidates = [
        row
        for row in rows
        if (row.get("step") or "").strip() == node
        and (row.get("git_sha") or "").strip() == commit
    ]
    return {
        "wall_seconds": None,
        "cpu_seconds": None,
        "cpu_user_seconds": None,
        "cpu_sys_seconds": None,
        "cpu_usage_usec": None,
        "cpu_source": "unavailable:no-run-bound-cgroup-usage",
        "cpu_per_wall": None,
        "timed_out": None,
        "cpu_timed_out": None,
        "throttled_seconds": None,
        "quota_utilization_pct": None,
        "co_tenants_end": None,
        "timing_verdict": None,
        "timing_source": "unavailable:no-observable-run-identity",
        "profile_rows_matched": 0,
        "profile_candidate_rows": len(candidates),
        "profile_row_timestamp": None,
    }


def classify_failed_substeps(
    log_text: str,
    *,
    flaky_registry: set[str] | None = None,
    commit: str | None = None,
    failed_nodes: list[str] | None = None,
    failure_classes: Mapping[str, tuple[str, str | None]] | None = None,
) -> list[FailedSubstepClass]:
    """Turn an aggregate DAG failure into a per-node, triageable verdict list.

    For every node that terminated ``✗ FAIL`` this returns one record:
    ``{"node", "group", "sub_step_class", "fault_class", "infra_signature",
    "host_env_signature", "first_error_line", "known_flaky"}`` — sorted by node.
    This is the read-side answer to
    "one-gate-name-for-three-unrelated-causes": instead of a single lane name and
    exit=1, a consumer sees WHICH node failed, whether it is a build/prep step or
    a product test, and whether the failure is an INFRASTRUCTURE fault (corrupt
    artifact / poisoned cache), a HOST/ENVIRONMENT fault (a tool the host does not
    provide), or a CODE fault.

    When ``failure_classes`` is supplied, the producer's closed value decides
    the class and text contributes explanation only. The rules below are the
    historical fallback for rows without that value (biased so a real code
    defect is never hidden as not-our-fault), checked in this order:
      * ``infrastructure`` — an infra signature (corrupt archive, poisoned cargo
        cache, …) appears on THIS node's own stream lines. This wins regardless of
        group: a build node dying on a corrupt ``.a`` and a test node whose prep
        hit a poisoned cache are both infra.
      * ``host-environment`` — the host does not provide a tool the step needs
        (``lua5.4 not found``, ``: command not found``, apt/dnf "unable to locate
        package"). Fixed by provisioning the host or repairing the runner's PATH,
        never by editing product code, so it must not route to a product-debugging
        loop. Distinct from ``infrastructure``, which is a corrupt/stale artifact
        in the build tree and is fixed by clearing state — different owner,
        different remedy, so they are not merged. See
        ``_host_env_signature_for`` for the two-tier scoping that keeps a product
        test's own "… not found" assertion output out of this class.
      * ``code`` — otherwise. A failing build/setup node with no infra or
        host-environment signature is a genuine compile error in the change under
        test (still ``code``); a failing test/e2e/lint/doc node is a product
        failure.
      * ``no-result`` exists only in current typed rows. It means the producer
        completed no product verdict; the historical prose fallback never
        manufactures it.
    ``known_flaky`` is advisory only (a failing test node whose name is in the
    measured-flake registry); it never downgrades ``fault_class`` — a flake is
    still a code-domain result to be re-run, not an infra fault.

    Both signature fields are always present so no evidence is discarded: a node
    carrying BOTH a corrupt archive and a missing host tool records both, and only
    the headline ``fault_class`` is forced to pick one.
    """
    registry = flaky_registry or set()
    # Profile rows are read only to expose how many {step, git_sha} candidates
    # existed. They are never selected: the producer does not yet carry an
    # observable run identity, so every node records typed-unavailable timing.
    profile_rows: list[dict[str, str]] = []
    if commit:
        root = checkout_root(log_text)
        if root is not None:
            profile_rows = _profile_rows(root)
    records: list[dict[str, object]] = []
    nodes = (
        sorted(set(failed_nodes))
        if failed_nodes is not None
        else failed_substeps(log_text)
    )
    for node in nodes:
        group = node.split(".", 1)[0] if "." in node else ""
        signature = _infra_signature_for(log_text, node)
        host_env = _host_env_signature_for(log_text, node)
        marker = _fault_marker_for(log_text, node)
        shared = failure_classes.get(node) if failure_classes is not None else None
        if shared is not None:
            failure_class, detail = shared
            fault = _SHARED_TO_FAULT_CLASS[failure_class]
            if fault == "host-environment" and not host_env:
                host_env = detail
            elif fault == "infrastructure" and not signature:
                signature = detail
        elif marker is not None:
            # TYPED SIGNAL WINS. The class is read from the token, so any reword
            # of the human detail — or of any other prose in the node's stream —
            # leaves the class untouched. The prose signatures below are still
            # computed and still reported, because they are evidence; they just
            # no longer DECIDE.
            fault = marker[0]
            if fault == "host-environment" and not host_env:
                host_env = marker[1] or "FAULT marker"
            elif fault == "infrastructure" and not signature:
                signature = marker[1] or "FAULT marker"
        elif signature:
            fault = "infrastructure"
        elif host_env:
            fault = "host-environment"
        else:
            fault = "code"
        sub_step = "dependency-build" if group in _BUILD_GROUPS else "lane-run"
        cell = node if "." in node else f"test.{node}"
        record: dict[str, object] = {
            "node": node,
            "group": group,
            "sub_step_class": sub_step,
            "fault_class": fault,
            "infra_signature": signature,
            "host_env_signature": host_env,
            "first_error_line": _first_error_line_for(log_text, node),
            "known_flaky": fault == "code"
            and sub_step == "lane-run"
            and cell in registry,
            # WHY this node ran out of time, when it did. Additive: consumers
            # read the row with .get()/Option, and the driver inlines this
            # list wholesale, so no producer-side change is needed downstream.
            "timing": node_timing(profile_rows, node, commit or ""),
        }
        if shared is not None:
            record["failure_class"] = shared[0]
        records.append(record)
    # Keep the producer on the same complete contract that retained-row
    # consumers use. This is intentionally a runtime check as well as a
    # TypedDict: JSON can bypass static checking.
    validated = failed_substep_classes_from_row({"failed_substep_classes": records})
    assert validated is not None
    return validated


def row_failed_substeps(row: Mapping[str, object]) -> set[str]:
    result: set[str] = set()
    gates = row.get("gates")
    if not isinstance(gates, list):
        return result
    for gate in gates:
        if not isinstance(gate, Mapping):
            continue
        # A current Rust-driver row records each DAG node as an outer gate.  Its
        # name is therefore the failure identity; ``failed_substeps`` is only
        # populated for the older aggregate-lane shape. The explicit empty list
        # distinguishes a bound outer failure from missing attribution; do not
        # union contradictory combinations into a larger invented failure set.
        origin = gate.get("failure_origin")
        substeps = gate.get("failed_substeps")
        outcome = str(gate.get("result") or gate.get("kind") or "")
        if (
            origin == "outer_gate"
            and substeps == []
            and gate.get("aborted") is not True
            and outcome in {"fail", "failed", "timeout"}
            and gate.get("name")
        ):
            result.add(str(gate["name"]))
        elif (
            origin == "lane_substep"
            and gate.get("aborted") is not True
            and outcome in {"fail", "failed", "timeout"}
            and isinstance(substeps, list)
            and bool(substeps)
            and all(isinstance(value, str) and bool(value) for value in substeps)
        ):
            result.update(str(value) for value in substeps if value)
    return result


def observed_failed_substeps(row: Mapping[str, object]) -> set[str]:
    """Read bound failure identities, or recover them from a surviving log.

    Rows written before failure classification was connected to the canonical
    ledger have neither ``failure_origin`` nor ``failed_substeps``.  A readable
    row-bound log is still direct evidence for that exact run.  Using it here
    lets the first compliant solo rerun confirm an older wide/contended red;
    an absent or unreadable log remains unknown and cannot confirm anything.
    """
    bound = row_failed_substeps(row)
    if bound:
        return bound
    log_file = row.get("log_file")
    if not isinstance(log_file, str) or not log_file:
        return set()
    try:
        return set(failed_substeps(Path(log_file).read_text(errors="replace")))
    except OSError:
        return set()


def prior_requires_rerun(row: Mapping[str, object]) -> bool:
    concurrent = row.get("concurrent_validates")
    jobs = row.get("dag_jobs")
    return (
        row.get("known_flaky_failure") is True
        or isinstance(concurrent, int) and not isinstance(concurrent, bool) and concurrent > 0
        or isinstance(jobs, int) and not isinstance(jobs, bool) and jobs > 4
    )


def build_evidence(
    *,
    log_text: str,
    registry: set[str],
    prior: list[dict[str, object]],
    commit: str,
    dag_jobs: int,
    concurrent_validates: int | None,
    cell_results: object = None,
    failed_nodes: list[str] | None = None,
    failure_classes: Mapping[str, tuple[str, str | None]] | None = None,
) -> dict[str, object]:
    cells = (
        sorted(set(failed_nodes))
        if failed_nodes is not None
        else failed_substeps(log_text)
    )
    cell_set = set(cells)
    has_cell_results, exact_failed, unavailable = failed_cells(cell_results)
    exact_failed_set = set(exact_failed)
    flaky_failed_cells = sorted(exact_failed_set.intersection(registry))
    # Schema-1 registries named DAG nodes. Keep their read path for historical
    # fixtures, but generated schema-2 entries are exact test/mode/backend ids
    # and therefore cannot match an aggregate node by accident.
    flaky_failed = sorted(cell for cell in cells if cell in registry)
    classes = classify_failed_substeps(
        log_text,
        flaky_registry=registry,
        commit=commit,
        failed_nodes=cells,
        failure_classes=failure_classes,
    )
    # Top-level, standalone verbatim fault line (already bounded by
    # _first_error_line_for). This is the headline line the PRODUCER serializes
    # with one jq extraction (`.first_error_line`) as a peer of the other scalar
    # fields, so a red row is attributable to WHICH BUG — not just which bucket —
    # from the row alone, after the /tmp log is evicted. It is the first failing
    # node's first substantive error line (node-sorted, matching the classes
    # order); the per-node lines remain in ``failed_substep_classes`` for a
    # multi-node red. Deliberately NOT classified/parsed: a class is a lossy
    # summary and ``failed_substep_classes`` already carries it.
    first_error_line = next(
        (
            cls["first_error_line"]
            for cls in classes
            if cls.get("first_error_line")
        ),
        None,
    )
    confirmation_row: dict[str, object] | None = None
    if dag_jobs == 4 and concurrent_validates == 0 and cell_set:
        candidates = [
            row
            for row in prior
            if str(row.get("commit") or "") == commit
            and prior_requires_rerun(row)
            and not cell_set.isdisjoint(observed_failed_substeps(row))
            and (
                not exact_failed_set
                or not exact_failed_set.isdisjoint(observed_failed_cells(row))
            )
        ]
        if candidates:
            confirmation_row = max(
                candidates, key=lambda row: str(row.get("finished_at") or "")
            )
    # A generated exact-cell entry may trigger the rerun rule only when every
    # observed product failure is one of those cells. A sibling comparison
    # failure, a cell that produced no comparison, or a non-manifest DAG-node
    # failure keeps the row out of the measured-flake class.
    exact_flaky_failure = (
        has_cell_results
        and bool(exact_failed)
        and len(flaky_failed_cells) == len(exact_failed)
        and not unavailable
        and bool(cells)
        and all(_manifest_result_node(node) for node in cells)
    )
    result = {
        "failed_substeps": cells,
        "failed_substep_classes": classes,
        "first_error_line": first_error_line,
        "flaky_failed_substeps": flaky_failed,
        "known_flaky_failure": exact_flaky_failure
        or (not has_cell_results and bool(flaky_failed)),
        "solo_rerun_confirmation": confirmation_row is not None,
        "solo_rerun_of": (
            {
                "finished_at": confirmation_row.get("finished_at"),
                "log_file": confirmation_row.get("log_file"),
            }
            if confirmation_row is not None
            else None
        ),
    }
    if has_cell_results:
        result["failed_cells"] = exact_failed
        result["flaky_failed_cells"] = flaky_failed_cells
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--dag-jobs", required=True, type=int)
    parser.add_argument("--concurrent-validates", required=True)
    parser.add_argument("--registry", type=Path, default=DEFAULT_REGISTRY)
    args = parser.parse_args(argv)
    if args.concurrent_validates == "null":
        concurrent = None
    else:
        try:
            concurrent = int(args.concurrent_validates)
        except ValueError:
            print("failure_evidence: --concurrent-validates must be integer or null", file=sys.stderr)
            return 2
    try:
        log_text = args.log.read_text(errors="replace")
        registry = flaky_cells(args.registry, args.commit)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"failure_evidence: cannot load evidence: {error}", file=sys.stderr)
        return 2
    evidence = build_evidence(
        log_text=log_text,
        registry=registry,
        prior=canonical_ledger.rows(),
        commit=args.commit,
        dag_jobs=args.dag_jobs,
        concurrent_validates=concurrent,
    )
    print(json.dumps(evidence, separators=(",", ":"), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
