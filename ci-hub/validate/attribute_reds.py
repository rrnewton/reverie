#!/usr/bin/env python3
"""Attribute FAILED validate-ledger rows to their per-substep cause.

WHY THIS EXISTS. In the recent window, ~87% of full-run failures were recorded
under the single aggregate gate name ``portable CI DAG manifest``. That name has
been proven to cover at least three UNRELATED faults — a DynamoRIO link failure
from a zero-byte object, a stale ``--locked`` Cargo.lock, and LiteInst clock
nondeterminism — so the recorded gate name alone cannot triage a red: every one
requires opening the log. This turns that manual investigation into a QUERY.

WHAT IT DOES. Current ledger rows already record ``failed_substep_classes``.
This tool reads that producer-owned value first. For historical rows where the
field is absent, it dereferences ``log_file`` through the same per-node
classifier (:func:`failure_evidence.classify_failed_substeps`). It surfaces,
for each failing row, the FACTS the aggregate name hid: which nested substep
failed, whether it is a build/prep step or a product test, whether the fault is
infrastructure or code, the matched infra signature, and — decisively — the
first substantive error line the failing node emitted, VERBATIM.

BINDING (Proxy Binding review axis). Recorded classes are bound to the row by
its producer. The historical fallback is bound to the failing run's own log
stream, node-scoped by the ``[<node.tag>] `` prefix, so one node's stdout can
never be read as another's evidence. A malformed recorded value refuses by the
field name rather than being treated as absent. It never decides a verdict
(that is ``validate_status``); it only renders the evidence a triager would
otherwise open the log to read.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
import sys
from typing import Mapping

sys.path.insert(0, str(Path(__file__).resolve().parent))
from failure_evidence import (  # noqa: E402
    DEFAULT_REGISTRY,
    classify_failed_substeps,
    failed_substep_classes_from_gates,
    failed_substep_classes_from_row,
    flaky_cells,
)
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402

# The historical durable capture. WHY IT STILL EXISTS: older run rows carry only
# a ``log_file`` under ephemeral /tmp and have no recorded
# ``failed_substep_classes``. Run before one of those logs is evicted, this
# sidecar lifts the node / fault class / VERBATIM first_error_line into an
# append-only record keyed to the red run. Current rows already carry that value
# in the ledger and do not depend on this sidecar. It never fabricates: an old row
# whose log is gone contributes nothing.
DEFAULT_ATTRIBUTION = (
    Path(__file__).resolve().parents[2] / "ignored" / "validate-red-attribution.jsonl"
)


# --------------------------------------------------------------------------
# REQUIRED COLUMNS PER SCHEMA VERSION.
#
# WHY THIS EXISTS. At v5 the run ledger silently stopped writing about a dozen
# columns that v4 had populated -- toolchain 203/206 -> 2/51, tree 201/206 ->
# 2/51, and with them `concurrent_validates`, `known_flaky_failure`, `dag_jobs`
# and `zero_byte_purged`. Nothing caught it, because NOTHING DECLARED WHAT v5
# OWED, and whole-ledger percentages actively hid the drop as ordinary
# sparseness. A declared required set would have failed every one of them AT
# WRITE TIME instead of surfacing months later as unattributable gaps.
#
# WHY IT IS DECLARED AND NOT INFERRED FROM COVERAGE. A percentage cannot tell a
# defect from correct behaviour: `coverage` at 7.0% is HEALTHY (conditional by
# design) while `toolchain` at 28.3% was a regression. Only a declared set
# distinguishes them.
#
# WHY IT IS PER-VERSION. A single global set is unsatisfiable -- older rows
# legitimately lack columns invented later. Older rows are UNQUALIFIED, never
# retro-failed. THERE IS NO BACKFILL: an absent value stays absent and honest.
ATTRIBUTION_SCHEMA_VERSION = 1

#: Columns whose absence is ALWAYS a defect, per schema version. This is the one
#: place that decides non-verdict; a new field becomes enforced by being added
#: here, never by growing a bespoke branch at its own call site.
REQUIRED_COLUMNS: dict[int, frozenset[str]] = {
    1: frozenset(
        {"commit", "finished_at", "log_file", "node", "sub_step_class", "fault_class"}
    ),
}

#: Columns that are CONDITIONAL BY DESIGN -- null is their legitimate value, so
#: requiring them would turn every healthy row into a non-verdict. Named
#: explicitly rather than merely omitted, so a future author sees the decision
#: instead of assuming an oversight. `first_error_line` is the load-bearing
#: example: a red whose log was evicted genuinely has none, and fabricating one
#: is the failure this whole sidecar exists to avoid.
CONDITIONAL_COLUMNS: frozenset[str] = frozenset(
    {"first_error_line", "infra_signature"}
)

#: The three states, kept distinct on purpose. Collapsing UNQUALIFIED into
#: NON_VERDICT would retro-condemn every older row; collapsing either into
#: FAILED would report a bookkeeping gap as a product failure.
VERDICT = "verdict"
NON_VERDICT = "non_verdict"
UNQUALIFIED = "unqualified"


def missing_required(
    row: Mapping[str, object], version: int = ATTRIBUTION_SCHEMA_VERSION
) -> list[str]:
    """Required columns absent or null in ``row``, sorted.

    Null counts as missing: a column present with a null value carries exactly
    as much diagnostic information as one that was never written, and treating
    them differently is how a regression hides behind key-presence statistics.
    An unknown version requires nothing -- see ``classify_row``.
    """
    required = REQUIRED_COLUMNS.get(version, frozenset())
    return sorted(name for name in required if row.get(name) in (None, ""))


def classify_row(row: Mapping[str, object]) -> str:
    """Which of the three states ``row`` is in.

    A row stamped with a version we do not know -- an OLDER writer, or the
    orphaned correction schema (`corrects` / `correction_*`) that no live
    producer emits -- is UNQUALIFIED. It is not failed and not refused: we
    simply cannot judge a row against a contract we never declared. That is the
    deliberate answer to "what does an orphaned schema mean", rather than
    crashing on it or silently passing it as complete.
    """
    version = row.get("schema_version")
    if not isinstance(version, int) or version not in REQUIRED_COLUMNS:
        return UNQUALIFIED
    return NON_VERDICT if missing_required(row, version) else VERDICT


def seal_row(row: dict[str, object]) -> dict[str, object]:
    """Stamp ``row`` with its schema version and its verdict state.

    A row missing a currently-required column is written AS A NON-VERDICT --
    not dropped, not accepted as a verdict. Dropping it would lose the evidence
    that a writer regressed; accepting it would let the gap masquerade as a
    real attribution. `missing_required` names WHICH columns, so the reason is
    machine-readable rather than folklore.
    """
    row["schema_version"] = ATTRIBUTION_SCHEMA_VERSION
    absent = missing_required(row)
    if absent:
        row["result"] = NON_VERDICT
        row["missing_required"] = absent
    else:
        row["result"] = VERDICT
    return row


def _is_red(row: Mapping[str, object]) -> bool:
    """A row worth attributing: it recorded a genuine failure.

    Keys on the row's own ``result`` being ``fail`` OR any recorded gate being
    ``fail``. This is a SELECTION filter for triage, not a verdict — the shared
    ``validate_status`` authority still decides FAILED/TRUNCATED/NEEDS-RERUN. A
    row selected here may still be a truncation; the attribution simply shows what
    the failing substep was, which is useful either way.
    """
    if str(row.get("result") or "") == "fail":
        return True
    gates = row.get("gates")
    if isinstance(gates, list):
        for gate in gates:
            if isinstance(gate, Mapping) and str(gate.get("result") or "") == "fail":
                return True
    return False


def _failed_gate_names(row: Mapping[str, object]) -> list[str]:
    gates = row.get("gates")
    names: list[str] = []
    if isinstance(gates, list):
        for gate in gates:
            if isinstance(gate, Mapping) and str(gate.get("result") or "") == "fail":
                name = gate.get("name")
                if name:
                    names.append(str(name))
    return names


#: A DAG node id is ``group.job`` (``test.cli``, ``build.liteinst_runtime_release``).
#: Outer gate names -- ``portable CI DAG manifest`` -- are prose and never match.
_NODE_ID_RE = re.compile(r"^[a-z0-9_-]+\.[a-z0-9_.-]+$")


def gate_causes(row: Mapping[str, object]) -> list[dict[str, object]]:
    """One PER-NODE record for each failed gate, derived from the ROW, not the log.

    ``classes`` is the good attribution and current rows carry it durably.
    Historical rows require the log fallback. Measured 2026-08-28 across the
    canonical ledger: 978 failed rows had only seven surviving logs, while 226
    log-missing rows already carried 681 durable classifications. Before this
    reader consumed the recorded field, it reported zero classifications and
    fell back to ``failed_gates`` -- a FLAT LIST OF NAMES.

    The gates array is still in the row and still per-node. This turns it back
    into per-node records carrying the exit code and duration, so a triager can
    tell two reds apart even with no log.

    ⚠️ IT DELIBERATELY CARRIES NO ``first_error_line`` AND NO ``fault_class``.
    There is no evidence for either, and ``CONDITIONAL_COLUMNS`` exists exactly
    so a log-evicted red can say so rather than fabricate one. ``evidence`` names
    the provenance so a consumer never mistakes a gate-derived record for a
    log-derived one -- weaker, and honestly labelled, beats an aggregate.
    """
    gates = row.get("gates")
    out: list[dict[str, object]] = []
    if not isinstance(gates, list):
        return out
    # A DAG node id is ``group.job``. An entry that is not shaped that way is an
    # OUTER gate name, and the outer name is the aggregate this tool exists to
    # get past -- ``portable CI DAG manifest`` alone covers ~87% of full-run
    # failures and is PROVEN to span at least three unrelated faults. Measured
    # on this ledger: of 722 log-less rows with derivable causes, 218 have NO
    # entry other than that name. Emitting those as ``gate-row`` would report
    # the aggregate under a per-node key -- reintroducing, inside the fix, the
    # exact defect the fix exists to remove. They are labelled distinctly so a
    # consumer can filter them out rather than be misled by them.
    for gate in gates:
        if not isinstance(gate, Mapping):
            continue
        if str(gate.get("result") or "") != "fail":
            continue
        name = gate.get("name")
        if not name:
            continue
        text = str(name)
        per_node = bool(_NODE_ID_RE.match(text))
        out.append(
            {
                "node": text,
                "evidence": "gate-row" if per_node else "aggregate-gate",
                "exit_code": gate.get("exit_code"),
                "real_seconds": gate.get("real_seconds"),
            }
        )
    return out


def attribute_row(
    row: Mapping[str, object], *, registry: set[str] | None = None
) -> dict[str, object]:
    """Attribute one ledger row from recorded classes or the historical log.

    Returns ``{commit, finished_at, result, exit_code, failed_gates, log_file,
    log_status, classes}`` where ``log_status`` is ``present``/``missing`` and
    ``classes`` is the producer-owned per-node value when present. Only a row
    where that field is absent may fall back to its log. A malformed recorded
    field raises ``ValueError`` by name instead of becoming empty evidence."""
    commit = str(row.get("commit") or "")
    log_file = row.get("log_file")
    recorded_classes = failed_substep_classes_from_row(row)
    gate_classes = (
        failed_substep_classes_from_gates(row, flaky_registry=registry or set())
        if recorded_classes is None
        else None
    )
    record: dict[str, object] = {
        "commit": commit,
        "finished_at": row.get("finished_at"),
        "result": row.get("result"),
        "exit_code": row.get("exit_code"),
        "failed_gates": _failed_gate_names(row),
        "log_file": log_file,
        "log_status": "missing",
        "classes": (
            recorded_classes if recorded_classes is not None else gate_classes or []
        ),
        # Per-node fallback that does NOT need the log. Always populated, so a
        # row whose log was reaped still says which node failed and how, rather
        # than degrading to the flat `failed_gates` name list.
        "gate_causes": gate_causes(row),
    }
    if isinstance(log_file, str) and log_file:
        path = Path(log_file)
        if path.is_file():
            try:
                text = path.read_text(errors="replace")
            except OSError:
                text = None
            if text is not None:
                record["log_status"] = "present"
                if recorded_classes is None and gate_classes is None:
                    record["classes"] = classify_failed_substeps(
                        text, flaky_registry=registry or set()
                    )
    return record


def _attribution_key(commit: str, finished_at: object, node: object) -> str:
    """Stable idempotency key for one persisted red-node attribution."""
    return f"{commit}\x1f{finished_at or ''}\x1f{node or ''}"


def _existing_attribution_keys(path: Path) -> set[str]:
    keys: set[str] = set()
    if not path.is_file():
        return keys
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            keys.add(
                _attribution_key(
                    str(rec.get("commit") or ""),
                    rec.get("finished_at"),
                    rec.get("node"),
                )
            )
    return keys


def persist_attributions(
    records: list[Mapping[str, object]], path: Path
) -> tuple[int, int]:
    """Append durable per-red-node attribution rows to ``path``, idempotently.

    For every attributed row whose log was PRESENT, emit one durable record per
    failing node carrying the verbatim ``first_error_line`` (and the node / fault
    class it was read from). A red run+node already present in ``path`` is skipped,
    so this is safe to run repeatedly and on every landing. Rows whose log was
    missing contribute nothing — the line is only ever captured from a real log,
    never fabricated. Returns ``(appended, skipped_existing)``.

    The write is a single ``O_APPEND`` open so it races no concurrent producer
    appending to a different ledger; each line is one self-contained JSON object.
    """
    seen = _existing_attribution_keys(path)
    pending: list[str] = []
    skipped = 0
    for record in records:
        if record.get("log_status") != "present":
            continue
        commit = str(record.get("commit") or "")
        finished_at = record.get("finished_at")
        for cls in record.get("classes") or []:
            node = cls.get("node")
            key = _attribution_key(commit, finished_at, node)
            if key in seen:
                skipped += 1
                continue
            seen.add(key)
            pending.append(
                json.dumps(
                    seal_row(
                        {
                            "commit": commit,
                            "finished_at": finished_at,
                            "log_file": record.get("log_file"),
                            "node": node,
                            "sub_step_class": cls.get("sub_step_class"),
                            "fault_class": cls.get("fault_class"),
                            "infra_signature": cls.get("infra_signature"),
                            "first_error_line": cls.get("first_error_line"),
                        }
                    ),
                    separators=(",", ":"),
                    sort_keys=True,
                )
            )
    if pending:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            for line in pending:
                handle.write(line + "\n")
    return len(pending), skipped


def refill_attributions(
    path: Path, *, registry: set[str] | None = None
) -> tuple[int, int, int]:
    """Backfill ``first_error_line`` for records an OLDER extractor persisted null.

    The idempotency key (``commit``, ``finished_at``, ``node``) deliberately
    EXCLUDES ``first_error_line``, so a later extractor improvement never re-fires
    through :func:`persist_attributions` — a red run+node already present is
    skipped, and its stale null line is stranded. This maintenance pass closes that
    gap: for every persisted record whose ``first_error_line`` is null/absent, if
    its log still survives it re-classifies that node with the CURRENT extractor and
    updates the record IN PLACE (all fields), leaving every other line
    byte-identical. A record whose log is gone is left untouched — never fabricated.
    Returns ``(refilled, still_null_log_present, log_evicted)``.

    Unlike :func:`persist_attributions` (append-only, race-free) this REWRITES the
    file via an atomic temp+rename, so it is a manual maintenance op and MUST NOT be
    run concurrently with a ``--persist`` appender.
    """
    if not path.is_file():
        return (0, 0, 0)

    cache: dict[str, dict[object, Mapping[str, object]]] = {}

    def classes_for(log_file: str) -> dict[object, Mapping[str, object]] | None:
        """Node->class map for a surviving log, or None if the log is gone."""
        if log_file in cache:
            return cache[log_file]
        p = Path(log_file)
        if not p.is_file():
            cache[log_file] = None  # type: ignore[assignment]
            return None
        try:
            text = p.read_text(errors="replace")
        except OSError:
            cache[log_file] = None  # type: ignore[assignment]
            return None
        by_node = {
            cls.get("node"): cls
            for cls in classify_failed_substeps(text, flaky_registry=registry or set())
        }
        cache[log_file] = by_node
        return by_node

    out: list[str] = []
    refilled = 0
    still_null = 0
    evicted = 0
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError:
            out.append(raw)
            continue
        if rec.get("first_error_line"):
            out.append(raw)  # already attributed — leave byte-identical
            continue
        log_file = rec.get("log_file")
        by_node = classes_for(log_file) if isinstance(log_file, str) and log_file else None
        if by_node is None:
            evicted += 1
            out.append(raw)
            continue
        cls = by_node.get(rec.get("node"))
        if cls is not None and cls.get("first_error_line"):
            out.append(
                json.dumps(
                    {
                        "commit": rec.get("commit"),
                        "finished_at": rec.get("finished_at"),
                        "log_file": log_file,
                        "node": rec.get("node"),
                        "sub_step_class": cls.get("sub_step_class"),
                        "fault_class": cls.get("fault_class"),
                        "infra_signature": cls.get("infra_signature"),
                        "first_error_line": cls.get("first_error_line"),
                    },
                    separators=(",", ":"),
                    sort_keys=True,
                )
            )
            refilled += 1
        else:
            still_null += 1
            out.append(raw)

    if refilled:
        tmp = path.with_name(path.name + ".refill.tmp")
        tmp.write_text("\n".join(out) + "\n", encoding="utf-8")
        tmp.replace(path)
    return (refilled, still_null, evicted)


def _render(record: Mapping[str, object]) -> str:
    commit = str(record.get("commit") or "")[:12]
    finished = record.get("finished_at") or "-"
    gates = ", ".join(record.get("failed_gates") or []) or "-"
    header = f"{commit:<12} {finished}  gate=[{gates}]"
    classes = record.get("classes") or []
    if not classes:
        status = record.get("log_status")
        reason = "no nested substep" if status == "present" else f"log {status}"
        return f"{header}\n    (no per-substep attribution — {reason})"
    lines = [header]
    for cls in classes:
        node = cls.get("node")
        fault = cls.get("fault_class")
        sub = cls.get("sub_step_class")
        sig = cls.get("infra_signature")
        first = cls.get("first_error_line")
        lines.append(f"    {node}  {fault}/{sub}" + (f"  sig={sig!r}" if sig else ""))
        if first:
            lines.append(f"        ↳ {first}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--registry", type=Path, default=DEFAULT_REGISTRY)
    parser.add_argument(
        "--commit", help="only rows whose commit starts with this prefix"
    )
    parser.add_argument(
        "--last",
        type=int,
        default=30,
        help="attribute the most recent N failed rows (0 = all)",
    )
    parser.add_argument("--json", action="store_true", help="emit JSON, not text")
    parser.add_argument(
        "--persist",
        nargs="?",
        type=Path,
        const=DEFAULT_ATTRIBUTION,
        default=None,
        metavar="PATH",
        help=(
            "append durable per-red-node attribution (verbatim first_error_line) "
            "captured from each surviving log into PATH (default "
            "ignored/validate-red-attribution.jsonl) — append-only and idempotent, "
            "so it can run on every landing to beat /tmp log eviction"
        ),
    )
    parser.add_argument(
        "--refill",
        nargs="?",
        type=Path,
        const=DEFAULT_ATTRIBUTION,
        default=None,
        metavar="PATH",
        help=(
            "maintenance: rewrite PATH in place, backfilling first_error_line for "
            "records an older extractor persisted null whose log still survives "
            "(the idempotency key excludes first_error_line, so --persist alone "
            "never re-fires). Atomic rewrite — do NOT run concurrently with --persist"
        ),
    )
    args = parser.parse_args(argv)

    try:
        registry = flaky_cells(args.registry)
    except (OSError, ValueError, json.JSONDecodeError):
        registry = set()

    if args.refill is not None:
        refilled, still_null, evicted = refill_attributions(
            args.refill, registry=registry
        )
        print(
            f"— refilled {refilled} record(s) in {args.refill} "
            f"({still_null} still null with log present, {evicted} log evicted); "
            f"maintenance rewrite — do not run concurrently with --persist"
        )
        return 0

    rows = canonical_ledger.rows()
    reds = [r for r in rows if _is_red(r)]
    if args.commit:
        reds = [r for r in reds if str(r.get("commit") or "").startswith(args.commit)]
    # Newest first by event-time (finished_at), not file position.
    reds.sort(key=lambda r: str(r.get("finished_at") or ""), reverse=True)
    if args.last and args.last > 0:
        reds = reds[: args.last]

    records = [attribute_row(r, registry=registry) for r in reds]

    if args.persist is not None:
        appended, skipped = persist_attributions(records, args.persist)
        print(
            f"— persisted {appended} durable attribution row(s) to {args.persist} "
            f"({skipped} already present); these survive /tmp log eviction"
        )

    if args.json:
        print(json.dumps(records, separators=(",", ":"), sort_keys=True))
        return 0

    attributable = sum(1 for rec in records if rec["classes"])
    log_present = sum(1 for rec in records if rec["log_status"] == "present")
    for rec in records:
        print(_render(rec))
        print()
    print(
        f"— {len(records)} failed rows; log present {log_present}; "
        f"per-substep attributable {attributable}; "
        f"log-missing {len(records) - log_present} (historical rows without "
        f"failed_substep_classes still require a surviving log)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
