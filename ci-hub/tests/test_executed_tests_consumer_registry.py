#!/usr/bin/env python3
"""A new `executed_tests` consumer must not appear unnoticed.

WHY THIS EXISTS. `executed_tests` is a REFUTED completeness signal: a count
cannot distinguish "ran fewer tests" from "covered fewer nodes". Commit
`ee303899` carries 8 PASS rows at `executed=427` that are legitimately NOT full
greens (`executed_test_nodes=4` of `planned=19`, 15 absent nodes). Completeness
is decided by `coverage.{planned,executed,absent}_nodes`; a red is decided by a
named `gates[]` entry plus `exit_code`. `executed_tests` survives ONLY as a
diagnostic and as the grandfather floor for pre-coverage rows.

The correction was applied to some call sites and not others, which is the worse
failure: the surviving sites look correct, disagree silently with the fixed ones,
and nobody can tell which authority a number came from.

THIS GUARD IS THE ANTIDOTE TO THE RECURRING SHAPE — a hardcoded list of a
growing set (the backend lint covering 3 of 6 backends, the `-j` default defined
twice, the ledger path under three names). It does not judge whether a given use
is correct; a static check cannot. It asserts that THE SET OF FILES ALLOWED TO
MENTION THE FIELD IS EXACTLY THE REVIEWED SET, so a new consumer cannot be added
silently and a removed one cannot leave a stale entry behind.

That churn is real, not hypothetical: consumers have repeatedly appeared and
dropped away, and a hand-maintained list was already out of date when this guard
was written.

WHEN THIS TEST FAILS, DO NOT JUST EDIT THE LIST. Classify the new site first:
  * producer / passthrough / diagnostic      -> add it, with its reason
  * grandfather floor for pre-coverage rows  -> add it, with its reason
  * a completeness or green/qualified key    -> THAT IS THE BUG. Rewrite it
    against `coverage.*` (greens) or named `gates[]`+`exit_code` (reds) instead.
The reference implementation is `ci-hub/lib/qualifying_receipt.rs`
`row_qualifies` / `coverage_satisfied`, mirrored in `ci-hub/qualifying_receipt.py`.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

# Reviewed as of hermit/dev-hermit main 8e8f8079efd50f057eb2e823c13f1efc7351c032.
# Each entry is a file that may mention `executed_tests`, with WHY.
ALLOWED: dict[str, str] = {
    # --- the shared predicate: the one place completeness logic belongs ---
    "ci-hub/lib/qualifying_receipt.rs": "reference impl; floor only, held to coverage",
    "ci-hub/qualifying_receipt.py": "python mirror of the reference impl",
    # --- schema / passthrough: carry the field, do not judge on it ---
    "ci-hub/lib/records.rs": "schema definition of the row",
    "ci-hub/lib/history_queries.rs": "passthrough/serialisation",
    "ci-hub/ci-hub.rs": "CLI surface / passthrough",
    # --- producers: write the field ---
    "ci-hub/validate/finalize_receipt.py": "producer: writes executed_tests + coverage",
    "ci-hub/validate/scan-finalize.sh": "producer glue",
    # --- consumers reviewed as diagnostic or grandfather-floor use ---
    "ci-hub/lib/validate_status.rs": "disposition; floor/grandfather use",
    "ci-hub/validate/flake_class.py": "flake classification input",
    "ci-hub/validate/aggregate.py": "reporting/derivation",
    "ci-hub/validate/anchor_select.py": "anchor selection",
    "ci-hub/validate/qualified_rows.py": "row filtering",
    "ci-hub/validate/scope_declaration.py": "scope declaration",
    "ci-hub/validate/preflight_anchor.py": "preflight anchor check",
    "ci-hub/validate/mutation_suite.py": "mutation harness over the predicate",
    "ci-hub/landing/preflight.py": "landing preflight",
    "ci-hub/landing/test-local-validation-eligibility.sh": "eligibility harness",
    "ci-hub/landing/exact-head-validation-authority.sh": (
        "landing authority; nonzero floor after complete coverage and all-pass gates"
    ),
    "ci-hub/landing/test-exact-head-validation-authority.sh": (
        "landing-authority fixture producer; carries the nonzero-floor field"
    ),
    "ci-hub/validation/verify_receipt.sh": "receipt verifier",
    "ci-hub/remediation/nonzero_result.py": "result classification",
    "ci-hub/pin/validation_gate.py": "pin validation gate",
    # Reads a SCORECARD CELL's own `executed_tests`, not a validate-ledger row,
    # and only as a REFUSAL FLOOR: a cell reporting <= 0 becomes NO_RESULT
    # ("scorecard cell N executed zero tests"). It never treats a positive count
    # as completeness -- the completeness verdict for a cell comes from the
    # comparison_tier / stdout_parity / stack+heap checks immediately below it.
    # Floor-only refusal is the same disposition already reviewed for
    # validate_status.rs. If the scorecard ever grows a coverage-shaped field,
    # this call site should move to it and leave this list.
    "scripts/release-0.3-acceptance.py": "acceptance gate; floor-only refusal on a scorecard cell",
    "scripts/owner-checkout-validate-watch.py": (
        "periodic reporter; delegates green/completeness to authoritative_row_qualification "
        "and exposes the count only as a diagnostic/null refusal"
    ),
    # A TEST FIXTURE in a production file, not a consumer. The file's only
    # mention is in its #[cfg(test)] module: the lost-run quarantine test
    # schema10_exact_quarantine_is_answered_without_reading_and_never_verifies
    # changes the recorded row's count, as it changes every other field, to
    # prove that the quarantine binds the whole row. No production code in the
    # file reads the field. Rust unit tests sit beside the code, so the
    # file-level derivation below cannot exclude them as it excludes tests/.
    "ci-hub/lib/validation_evidence.rs": (
        "unit-test fixture only; the quarantine test changes the count to prove "
        "the whole recorded row is bound"
    ),
}


def _referencing_files() -> set[str]:
    """Every non-test tracked file mentioning the field, derived from git."""
    out = subprocess.run(
        ["git", "grep", "-l", "executed_tests", "--", "*.py", "*.rs", "*.sh"],
        cwd=REPO_ROOT, capture_output=True, text=True,
    ).stdout
    found = set()
    for line in out.splitlines():
        p = line.strip()
        if not p:
            continue
        # Tests legitimately name the field to exercise it; they are not consumers.
        parts = p.split("/")
        if any(seg in ("test", "tests") for seg in parts):
            continue
        # Only a python test module is excluded by name. A shell harness under a
        # production directory (e.g. ci-hub/landing/test-local-validation-
        # eligibility.sh) IS a consumer and must stay in scope -- excluding it by
        # a loose `test-` prefix is exactly how a real call site goes unwatched.
        if parts[-1].startswith("test_"):
            continue
        found.add(p)
    return found


def test_no_unreviewed_executed_tests_consumer_appears() -> None:
    """A file not on the reviewed list must not mention the refuted field."""
    new = sorted(_referencing_files() - set(ALLOWED))
    assert not new, (
        "UNREVIEWED `executed_tests` consumer(s) appeared:\n  "
        + "\n  ".join(new)
        + "\n\n`executed_tests` is a REFUTED completeness signal. Classify each "
        "site before adding it to ALLOWED in this file:\n"
        "  greens -> coverage.{planned,executed,absent}_nodes\n"
        "  reds   -> named gates[] + exit_code\n"
        "See ci-hub/lib/qualifying_receipt.rs row_qualifies for the reference."
    )


def test_allowlist_has_no_stale_entries() -> None:
    """The other direction: a listed file that no longer mentions the field.

    Without this, the list only ever grows and silently stops describing the
    code -- the same drift that produced the original split-brain.
    """
    stale = sorted(set(ALLOWED) - _referencing_files())
    assert not stale, (
        "ALLOWED lists file(s) that no longer mention `executed_tests`; "
        "prune them so the list keeps describing reality:\n  " + "\n  ".join(stale)
    )


def test_the_detector_is_not_inert() -> None:
    """Positive control: the derivation actually finds the known consumers.

    An empty or broken `git grep` would make both tests above pass vacuously --
    a guard that can never fire is worse than none.
    """
    found = _referencing_files()
    assert len(found) >= 15, f"expected the known consumer set, derived only {len(found)}"
    for anchor in ("ci-hub/lib/qualifying_receipt.rs", "ci-hub/lib/records.rs"):
        assert anchor in found, f"derivation missed a known consumer: {anchor}"
