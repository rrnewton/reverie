#!/usr/bin/env python3
"""Distinguish a DEFECT red from a FLAKE / CONTENTION red in the validate ledger.

The asymmetry this addresses: a GAP in the ledger invites re-measurement, but a
recorded ``result: fail`` does NOT — nobody re-runs a commit the ledger condemns.
So a spurious red is *more* damaging than a missing record and is invisible
because it looks exactly like a real one. This module makes a red carry its
CONDITIONS so it can be interpreted rather than merely believed — the same
principle as the executed-count check (a verdict without its conditions is
uninterpretable), applied to failures instead of passes.

Two MEASURED false-red sources, both real:
  1. CONTENTION on the shared cargo package cache / build dir: #1592 FAILED at
     ``-j16`` (concurrent with other validates) and PASSED at ``-j4`` solo — and
     ``-j4`` was faster. The red was contention, not a defect.
  2. A measured FLAKY CELL: ``command_strict_verify`` was 9 PASS / 1 FAIL of 10
     under identical conditions. One run of it is a 90/10 coin; one tail lands a
     permanent red. See ``flaky-cells.json``.

This is the READ-SIDE half (parent tooling): it reclassifies already-recorded
reds so the false ones surface for re-measurement, and it works retroactively on
the whole ledger history. The WRITE-SIDE half — hermit/scripts/validate.rs recording the
``-j`` width and concurrent-validate count at run time, splitting the DAG gate
into manifest-check vs lane-run, and enforcing "re-run solo at -j4 before writing
FAILURE" — is a separate hermit PR. Until then this module DERIVES concurrency
from overlapping ledger intervals and PARSES the failing cell names from the run
log, so it is useful before the producer changes land.

Every classification carries its OWN firing conditions in ``reasons`` — the
classifier refuses to launder a guess into a verdict, just like the records it
judges.
"""
from __future__ import annotations

import datetime as dt
import json
import os
import re
from dataclasses import dataclass, field

# A DAG node's own per-node verdict line, e.g.:
#   [test.command_strict_verify] ✗ FAIL   Portable command strict verification (113s, exit 101)
#   [test.command_strict_verify] test result: FAILED. 9 passed; 1 failed; ...
# Anchored on the `[test.<cell>]` prefix the safe-ci-dag-runner writes itself, and
# an UPPERCASE FAIL/FAILED token so a lowercase "0 failed" count never matches.
_NODE_PREFIX_RE = re.compile(r"^\[test\.(?P<cell>[^\]]+)\]\s+(?P<rest>.*)$")
_FAIL_TOKEN_RE = re.compile(r"(✗\s*FAIL\b|\bFAILED\b)")

FLAKY_REGISTRY_PATH = os.path.join(os.path.dirname(__file__), "flaky-cells.json")

# Contention rule threshold: the measured false red (#1592) was at -j16. -j4 solo
# passed, so widths at or below this are treated as low-contention even when other
# validates overlap. Kept as a named constant, not a magic literal.
CONTENTION_SAFE_JOBS = 4
FULL_GATES_EXPECTED = 5

# The shell's "command not found" exit code. A gate at this code never ran the
# tool it wraps, so it exercised nothing about the product.
EXIT_COMMAND_NOT_FOUND = 127


def gate_counts(record: dict) -> tuple[int | None, int | None]:
    """Return (ran, expected), including the legacy full-profile fallback."""
    ran = record.get("gates_run", record.get("checks"))
    expected = record.get("gates_expected")
    # Schema-3 introduced the current anchored producer shape, whose full
    # profile has five gates. Do not apply that count to reconstructed schema-1
    # history: older complete full runs legitimately had four gates.
    if (
        expected is None
        and record.get("profile") == "full"
        and isinstance(record.get("schema_version"), int)
        and record["schema_version"] >= 3
    ):
        expected = FULL_GATES_EXPECTED
    return (
        ran if isinstance(ran, int) else None,
        expected if isinstance(expected, int) else None,
    )


def is_full_coverage(record: dict) -> bool:
    """Whether a run exercised the FULL validation contract, as opposed to a
    narrowed ``*-only``/subset profile (``portable-strict-compat-only``,
    ``only-portable``, ``portable-only``, ``quick``, ...).

    Hermit's Rust driver and ``aggregate.py`` both name a partial run with a non-
    ``full`` profile, and ``aggregate.py`` additionally records an explicit
    ``full_coverage`` boolean. Prefer that explicit field when present; otherwise
    fall back to the profile taxonomy. This keys on the SAME ``profile == "full"``
    signal as the Rust ``validate_status::is_clean_full_coverage`` landing
    predicate.

    SCOPE OF THIS ANSWER, corrected 2026-08-07. This reports the run's DECLARED
    scope, not its achieved coverage, and the two are not the same. An earlier
    version of this docstring claimed "a partial pass can never be read as a full
    green here either"; that was false. The ``full_coverage`` field it prefers is
    emitted by NO current ledger writer (17 schema-3 rows and nothing since --
    schema 4 at ci-hub.rs:3849 and schema 5 at finalize_receipt.py:52 both omit
    it), so in practice this always falls through to ``profile == "full"``. On the
    live ledger there are 13 rows with ``profile == "full"`` whose own coverage
    block records fewer executed test nodes than planned -- e.g. ``aff18cf466e3``
    at 0/19 and ``ee3038998f`` at 4/19 -- and this function returns True for every
    one of them.

    That is acceptable HERE, because flake classification asks "was this run
    supposed to be full-scope?", which a declaration answers. It is not
    acceptable for a question about what actually ran. For that, use
    ``totality.classify``/``totality.is_total``, which decide from observed
    execution and return UNKNOWN rather than True when nothing records it. The
    two predicates disagree on 386 of 654 live rows -- by design, because they
    answer different questions. Do not swap one for the other without deciding
    which question you are asking."""
    fc = record.get("full_coverage")
    if isinstance(fc, bool):
        return fc
    return record.get("profile") == "full"


def has_real_failure(record: dict) -> bool:
    """True when a gate/test ACTUALLY failed — as opposed to an incomplete or
    interrupted run. A gate row marked fail/timeout, or a nonzero product
    ``failures`` count, is a real red signal. Used to keep ``ran < expected`` from
    laundering a fail-fast red into a truncation."""
    for g in record.get("gates", []):
        if isinstance(g, dict) and g.get("result") in ("fail", "failed", "timeout"):
            return True
    f = record.get("failures")
    return isinstance(f, int) and f >= 1


def is_truncated(record: dict) -> bool:
    """True when this row did not complete its declared validation gate set.

    Completeness is structural: when both counts exist, only ``ran < expected``
    is an incomplete result. An OVER-RUN (``ran > expected``) is NOT a truncation:
    the hardcoded ``FULL_GATES_EXPECTED`` can lag the live plan (a full run today
    executes six gates against a hardcoded expectation of five), so a 6/5 row is a
    complete PASS, not a short run. A fail-fast row may contain a real failing
    gate, but it still did not execute the full validation contract and cannot
    become a durable FAILED verdict. A genuine red control is a complete row whose
    gate counts are satisfied. For legacy rows without both counts, interruption
    signals are used only when no real failure was recorded.
    """
    if record.get("result") == "truncated":
        return True
    ran, expected = gate_counts(record)
    if expected is not None and expected > 0 and ran is not None:
        return ran < expected
    if has_real_failure(record):
        return False
    return record.get("exit_code") == 130


def _failing_gates(record: dict) -> list[dict]:
    return [
        g
        for g in record.get("gates", [])
        if isinstance(g, dict) and g.get("result") in ("fail", "failed", "timeout")
    ]


def is_env_fault(record: dict) -> bool:
    """True when a red's gates could not run at all — an ENVIRONMENT fault, not a
    product defect, so the red carries no information about the commit. Mirrors
    the Rust authority (``validate_status::is_env_fault_red``) so the two engines
    never disagree. Two each-sufficient tells, each bound to a value the row
    carries (Proxy Binding — classify on the observed fault, not on the absence
    of condition fields):

      (A) COMMAND-NOT-FOUND STORM: at least one failing gate at exit 127 AND no
          failing gate is a genuine product failure (a red gate that ran >0s at a
          non-127 exit). A real test/assertion red is exit 1/101 after real
          execution — never 127 — so a genuine defect is never laundered.
          LIVE: a1493427 recorded fail at 1s with five gates at exit 127; the
          SAME commit passed 6/6 at 58s.
      (B) SUB-SECOND COLLAPSE: whole-run wall <= 1s AND every gate that produced
          a result failed — the run died before doing any real work.
    """
    failed = _failing_gates(record)
    if not failed:
        return False

    def _exit(g: dict) -> int | None:
        c = g.get("exit_code")
        return c if isinstance(c, int) else None

    def _secs(g: dict) -> float:
        s = g.get("real_seconds")
        return float(s) if isinstance(s, (int, float)) else 0.0

    any_cmd_not_found = any(_exit(g) == EXIT_COMMAND_NOT_FOUND for g in failed)
    any_genuine_red = any(
        _exit(g) != EXIT_COMMAND_NOT_FOUND and _secs(g) > 0 for g in failed
    )
    storm = any_cmd_not_found and not any_genuine_red

    wall = record.get("real_seconds")
    gates = [g for g in record.get("gates", []) if isinstance(g, dict)]
    subsecond = (
        isinstance(wall, (int, float))
        and wall <= 1
        and bool(gates)
        and all(g.get("result") in ("fail", "failed", "timeout") for g in gates)
    )
    return storm or subsecond


def effective_result(record: dict) -> object:
    """Result for analytics: incomplete rows are never product failures, and an
    environment fault (gates could not run) is a no-result, not a failure.

    An environment fault is the MORE SPECIFIC reading and wins over the
    completeness (truncation) branch even when the collapse also left gates
    unrun — a sub-second collapse that died after 3 of 5 gates carries no
    information about the commit and is a no-result, not a truncation. This
    ordering (env-fault BEFORE completeness) mirrors the Rust authority
    ``validate_status::failure_disposition`` so the two engines never disagree.
    An explicit first-class ``truncated`` result is not fail/timeout, so it falls
    through to ``is_truncated`` and stays ``truncated``.

    A PASS over a NARROWED scope (a non-``full`` profile) is downgraded to
    ``pass-partial`` so a reader tells it from a full-coverage green WITHOUT
    knowing the profile taxonomy — a 2-check ``portable-strict-compat-only`` pass
    must not read identically to a full green. This mirrors the ``aggregate.py``
    producer verdict (which already types reconstructed schema-1 partial passes
    ``pass-partial``) and the Rust ``history_queries`` reader (which already
    matches both ``pass`` and ``pass-partial``); it closes the gap for the live
    schema-4 ``validate.sh`` rows, whose ``result`` is a bare ``pass``. The
    landing certifier already refuses a partial via its ``profile == full``
    predicate; this makes the analytics result self-describing too."""
    if record.get("result") in ("fail", "timeout") and is_env_fault(record):
        return "no-result"
    if is_truncated(record):
        return "truncated"
    # A red with no NAMED failing gate and no failure count carries no observable
    # defect — a no-result wearing a red badge, never a durable failure. Checked
    # AFTER env-fault and truncation (each a more specific reading) and mirroring
    # the Rust authority's ``failure_disposition`` gate. ``executed_tests`` was
    # refuted as a proxy in BOTH directions (a build/clippy red exercises zero
    # tests yet is a genuine defect; a high count can still be a no-result), so the
    # verdict binds to the named-gate authority, not a count.
    if record.get("result") in ("fail", "timeout") and not has_real_failure(record):
        return "no-result"
    if record.get("result") == "pass" and not is_full_coverage(record):
        return "pass-partial"
    return record.get("result")


def failure_tier(record: dict) -> str:
    """Tier a fail/timeout row by the STRENGTH of its failure evidence, keyed on
    the named-gate authority — the successor of the refuted executed-count gate and
    the ONE shared red-side predicate for pr-status. Returns:

      ``"ok"``          — a genuine named failing gate survives the env-fault and
                          truncation readings: a durable defect.
      ``"needs-rerun"`` — the run did not complete its gate contract (truncated),
                          or it recorded a failure count with no named gate to
                          attribute it to: re-run to obtain observable evidence.
      ``"no-result"``   — not a red, an environment fault, or a red with neither a
                          named failing gate nor a failure count: no observable
                          defect; re-dispatch.

    Ordering mirrors the Rust authority ``validate_status::failure_disposition``
    (env-fault, then truncation, then named-gate presence) so the two engines never
    disagree. ``executed_tests`` is diagnostic only and no longer keys the tier."""
    if record.get("result") not in ("fail", "timeout"):
        return "no-result"
    if is_env_fault(record):
        return "no-result"
    if is_truncated(record):
        return "needs-rerun"
    if not _failing_gates(record):
        return "needs-rerun" if has_real_failure(record) else "no-result"
    return "ok"


def load_registry(path: str = FLAKY_REGISTRY_PATH) -> dict[str, dict]:
    """Return {cell_name: entry}. Missing/invalid file -> empty registry."""
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (OSError, json.JSONDecodeError):
        return {}
    out: dict[str, dict] = {}
    for entry in data.get("cells", []):
        if isinstance(entry, dict) and entry.get("cell"):
            out[str(entry["cell"])] = entry
    return out


def parse_failing_cells(log_path: str | None) -> list[str]:
    """Failing DAG-node cell names parsed from a run log, deduped in first-seen
    order. Empty when the log is absent or the failure was not a per-cell DAG
    node (a direct gate) — in which case the caller cannot attribute it to a
    known-flaky cell and must not treat it as flake."""
    if not log_path or not os.path.exists(log_path):
        return []
    seen: dict[str, None] = {}
    try:
        with open(log_path, errors="replace") as fh:
            for ln in fh:
                m = _NODE_PREFIX_RE.match(ln.rstrip("\n"))
                if not m:
                    continue
                if _FAIL_TOKEN_RE.search(m.group("rest")):
                    seen.setdefault(m.group("cell").strip(), None)
    except OSError:
        return []
    return list(seen)


def _epoch(ts: object) -> float | None:
    if not isinstance(ts, str) or not ts:
        return None
    try:
        return dt.datetime.strptime(ts, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=dt.timezone.utc).timestamp()
    except ValueError:
        return None


def _interval(record: dict) -> tuple[float, float] | None:
    """[start, finish] epoch seconds for a record. Falls back to
    finish - real_seconds when started_at is absent (reconstructed rows)."""
    finish = _epoch(record.get("finished_at"))
    if finish is None:
        return None
    start = _epoch(record.get("started_at"))
    if start is None:
        secs = record.get("real_seconds")
        start = finish - secs if isinstance(secs, (int, float)) and secs >= 0 else finish
    return (min(start, finish), max(start, finish))


def derive_concurrent_validates(record: dict, all_records: list[dict]) -> int:
    """Count OTHER validate runs whose wall interval overlaps this run's.

    Contention is invisible in a single run's own output, and pre-schema-4 rows
    do not record it — so we recover it from the ledger's own timeline. Two runs
    overlap iff a.start < b.finish and b.start < a.finish. Identity is by
    log_file (unique per run) with an interval fallback so a record without a
    log_file is not treated as overlapping every anonymous sibling."""
    mine = _interval(record)
    if mine is None:
        return 0
    my_lf = record.get("log_file")
    count = 0
    for other in all_records:
        if other is record:
            continue
        if my_lf and other.get("log_file") == my_lf:
            continue
        oiv = _interval(other)
        if oiv is None:
            continue
        if mine[0] < oiv[1] and oiv[0] < mine[1]:
            count += 1
    return count


@dataclass
class FlakeAnalysis:
    verdict: str  # "defect" | "needs-rerun" | "truncated" | "no-result" | "n/a"
    reasons: list[str] = field(default_factory=list)
    failing_cells: list[str] = field(default_factory=list)
    flaky_failing_cells: list[str] = field(default_factory=list)
    concurrent_validates: int = 0
    jobs: object = None  # int width the run executed at, or None if unrecorded

    def to_dict(self) -> dict:
        return {
            "verdict": self.verdict,
            "reasons": self.reasons,
            "failing_cells": self.failing_cells,
            "flaky_failing_cells": self.flaky_failing_cells,
            "concurrent_validates": self.concurrent_validates,
            "jobs": self.jobs,
        }


def classify(
    record: dict,
    all_records: list[dict],
    registry: dict[str, dict],
) -> FlakeAnalysis:
    """Classify one record's red as defect vs needs-rerun.

    NEVER mutates ``record``. Only ``fail``/``timeout`` results are judged;
    anything else returns verdict ``n/a``. A red is ``needs-rerun`` iff:
      (a) it has attributed failing cells and EVERY one is in the flaky
          registry (a mixed defect+flake failure stays ``defect`` — a real cell
          also broke), OR
      (b) no width is recorded (or width > CONTENTION_SAFE_JOBS) AND at least one
          other validate overlapped it in time (contention-suspect).
    Otherwise ``defect``. Each firing signal is named in ``reasons``."""
    result = record.get("result")

    # An environment fault (command-not-found storm, sub-second collapse) is the
    # most specific reading: its gates could not run, so the red carries no
    # information about the commit. Checked FIRST — before the completeness
    # (truncation) branch — so a sub-second collapse that died after only 3 of 5
    # gates reads "no-result", not "truncated"; and before flake/contention so a
    # 127-storm never reads "defect". Re-runnable, never a failure. This ordering
    # mirrors the Rust authority ``validate_status::failure_disposition``.
    if result in ("fail", "timeout") and is_env_fault(record):
        return FlakeAnalysis(
            verdict="no-result",
            reasons=[
                "env-fault: gate commands could not run (command-not-found storm "
                "or sub-second collapse) — an environment fault, not a product "
                "defect; re-dispatch"
            ],
        )

    if is_truncated(record):
        ran, expected = gate_counts(record)
        return FlakeAnalysis(
            verdict="truncated",
            reasons=[f"truncated: completed {ran}/{expected} required gates"],
        )

    if result not in ("fail", "timeout"):
        return FlakeAnalysis(verdict="n/a")

    # No NAMED failing gate and no failure count means no observable defect to
    # judge (mirrors the Rust authority validate_status::failure_disposition, and
    # checked here AFTER env-fault and truncation, BEFORE flake/contention).
    # ``executed_tests`` was refuted as a proxy in BOTH directions, so the verdict
    # binds to the named-gate authority, not a count: a build/clippy red exercises
    # zero tests yet is a genuine defect. checks==6 is not evidence either.
    if not has_real_failure(record):
        return FlakeAnalysis(
            verdict="no-result",
            reasons=[
                "no named failing gate and no failure count — the red carries no "
                "observable defect; re-dispatch"
            ],
        )

    jobs = record.get("jobs")  # schema-4 producer field; None until it lands
    failing = parse_failing_cells(record.get("log_file"))
    flaky_hits = [c for c in failing if c in registry]
    concurrent = derive_concurrent_validates(record, all_records)

    reasons: list[str] = []
    verdict = "defect"

    if failing and flaky_hits and set(failing) == set(flaky_hits):
        verdict = "needs-rerun"
        for c in flaky_hits:
            e = registry.get(c, {})
            reasons.append(
                "known-flaky-cell: {} (measured {}/{}) — single FAILED is a "
                "weighted coin, re-run solo".format(
                    c, e.get("observed_pass", "?"), e.get("sample_size", "?")))
    else:
        width_unknown = not isinstance(jobs, int)
        contended = concurrent > 0 and (width_unknown or jobs > CONTENTION_SAFE_JOBS)
        if contended:
            verdict = "needs-rerun"
            reasons.append(
                "contended: {} concurrent validate(s) overlapped; width {} — "
                "cargo cache/build-dir contention produced a false red before "
                "(#1592 -j16 fail vs -j4 pass), re-run solo at -j{}".format(
                    concurrent,
                    "unrecorded" if width_unknown else "-j%d" % jobs,
                    CONTENTION_SAFE_JOBS))
        if flaky_hits and verdict == "defect":
            # Mixed: a flaky cell AND a non-flaky cell failed -> real defect, but
            # surface the flaky component so a re-run separates the two.
            reasons.append(
                "defect: non-flaky cell(s) also failed ({}); flaky component: "
                "{}".format(
                    ", ".join(c for c in failing if c not in registry),
                    ", ".join(flaky_hits)))
    if verdict == "defect" and not reasons:
        reasons.append(
            "defect: no known-flaky failing cell and no overlapping validate"
            + (" (width -j%d)" % jobs if isinstance(jobs, int) else ""))

    return FlakeAnalysis(
        verdict=verdict,
        reasons=reasons,
        failing_cells=failing,
        flaky_failing_cells=flaky_hits,
        concurrent_validates=concurrent,
        jobs=jobs,
    )


def annotate(all_records: list[dict], registry: dict[str, dict] | None = None) -> None:
    """Attach a ``flake_analysis`` dict to every red record in place (additive;
    ``result`` is never changed)."""
    reg = registry if registry is not None else load_registry()
    for r in all_records:
        fa = classify(r, all_records, reg)
        if fa.verdict != "n/a":
            r["flake_analysis"] = fa.to_dict()
