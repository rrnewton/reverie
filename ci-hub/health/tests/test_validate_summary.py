#!/usr/bin/env python3
"""A failure count is reported BESIDE its denominator, and absences are not folded in.

⚠️ THIS EXISTS BECAUSE THE COUNT ALONE MISLED THREE TIMES IN ONE NIGHT. Measured
on real receipts:

    752cbc3daef7   63 gates   9 failures
    4a4830af353f   67 gates   6 failures
    c0914b5d5e11   66 gates   9 failures

Read as 9 -> 6 -> 9 that sequence describes almost nothing. Coverage ROSE by four
gates across the first step and FELL by one across the second. A red set that
shrinks because a gate STOPPED RUNNING is the opposite of progress and reads
identically on the count -- which is exactly what happened: `scorecard.compatibility`
went from passing to not running at all, and the failure count went UP for
unrelated reasons, so the coverage loss was invisible in the number.

The second half of the rule is that the non-failures are not one bucket either.
Dependency-skipped, never-ran, no-outcome and intentional-skip need four different
actions, and one of them -- no-outcome -- means the INSTRUMENT failed to account
for a node rather than anything about the product.

Synthetic rows only: no ledger, no network, no validate.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib

HERE = pathlib.Path(__file__).resolve()
LEDGER = HERE.parent.parent.parent / "ledger" / "validate_rows.py"


def _load():
    # ⚠️ `ledger` IS BOTH A MODULE AND A DATA DIRECTORY IN THIS REPO. `ci-hub/ledger`
    # is the package validate_rows imports; `<root>/ledger` holds the tracked shards.
    # Whichever is found first on sys.path wins, and under pytest the repo root is
    # already there -- so the data directory resolves as an empty namespace package
    # and the import fails with "unknown location". Put ci-hub first and drop a
    # cached binding that is not ci-hub/ledger/ledger.py before loading. Keep one
    # that is: an earlier test module in the same session may have imported
    # validate_rows and publisher, which took their names from it, and a second
    # copy of ledger would give that module exception classes validate_rows
    # never raises.
    import sys
    sys.path.insert(0, str(LEDGER.parent))        # ci-hub/ledger -- holds ledger.py
    sys.path.insert(0, str(LEDGER.parent.parent))  # ci-hub
    cached = sys.modules.get("ledger")
    cached_file = getattr(cached, "__file__", None)
    real = (LEDGER.parent / "ledger.py").resolve()
    if cached is not None and (cached_file is None or pathlib.Path(cached_file).resolve() != real):
        del sys.modules["ledger"]
    loader = importlib.machinery.SourceFileLoader("validate_rows_under_test", str(LEDGER))
    spec = importlib.util.spec_from_loader("validate_rows_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


vr = _load()


def _row(**kw):
    base = {
        "commit": "c0914b5d5e11d68fe676c372613faf99c0825c3e",
        "finished_at": "2026-08-26T03:33:03Z",
        "profile": "full",
        "selection_mode": "full",
        "result": "fail",
        "gates_run": 66,
        "failed_substeps": [],
        "dependency_skipped_nodes": [],
        "unaccounted_nodes": [],
        "intentional_skipped_nodes": [],
        "timed_out_nodes": [],
        "coverage": {"absent_nodes": []},
    }
    cov = kw.pop("absent_nodes", None)
    base.update(kw)
    if cov is not None:
        base["coverage"] = {"absent_nodes": cov}
    return base


# --------------------------------------------------------------------------
# The denominator.
# --------------------------------------------------------------------------

def test_the_headline_cannot_state_failures_without_gates_run() -> None:
    """⚠️ THE WHOLE POINT. There is no rendering path that emits a bare count."""
    text = vr.render_summary(vr.summarise(_row(failed_substeps=["a", "b"], gates_run=66)))
    head = [l for l in text.splitlines() if "FAILING" in l][0]
    assert head == "2 FAILING of 66 gates run", head


def test_the_same_count_at_two_coverages_renders_differently() -> None:
    """9 of 63 and 9 of 66 are not the same fact, and must not print the same."""
    a = vr.render_summary(vr.summarise(_row(failed_substeps=list("abcdefghi"), gates_run=63)))
    b = vr.render_summary(vr.summarise(_row(failed_substeps=list("abcdefghi"), gates_run=66)))
    assert "9 FAILING of 63 gates run" in a
    assert "9 FAILING of 66 gates run" in b
    assert a != b


# --------------------------------------------------------------------------
# The four buckets stay four.
# --------------------------------------------------------------------------

def test_absences_are_not_counted_as_failures() -> None:
    s = vr.summarise(_row(
        failed_substeps=["test.cli"],
        dependency_skipped_nodes=["scorecard.compatibility"],
        absent_nodes=["test.cli_kvm"],
    ))
    assert s["failing"] == ["test.cli"]
    assert s["dependency_skipped"] == ["scorecard.compatibility"]
    assert s["never_ran"] == ["test.cli_kvm"]
    assert len(s["failing"]) == 1, "an absence must never inflate the failing count"


def test_the_buckets_are_never_summed() -> None:
    """No total, by design: the four want four different actions, so a caller must
    choose which it means rather than being handed a number that hides the choice."""
    s = vr.summarise(_row(
        failed_substeps=["a"], dependency_skipped_nodes=["b"],
        absent_nodes=["c"], unaccounted_nodes=["d"],
    ))
    assert not any(k in s for k in ("total", "not_green", "unhealthy", "problems"))
    text = vr.render_summary(s)
    assert "4" not in [l.split()[0] for l in text.splitlines() if l and l[0].isdigit()]


def test_no_outcome_is_reported_distinctly_and_labelled_the_third_state() -> None:
    """The only bucket that says the INSTRUMENT failed, not the product. It was
    silent before: on 752cbc3daef7 two planned nodes returned no outcome at all."""
    text = vr.render_summary(vr.summarise(_row(
        unaccounted_nodes=["scorecard.compatibility", "test.strict_compat"])))
    assert "2 NO OUTCOME" in text
    assert "THIRD STATE" in text


def test_the_three_absence_buckets_print_even_when_empty() -> None:
    """⚠️ ZERO MUST BE STATED, NOT OMITTED. A missing line reads as 'not checked';
    an explicit 0 reads as 'checked, none'. That difference is the whole reason
    the coverage loss went unnoticed in the first place."""
    text = vr.render_summary(vr.summarise(_row(failed_substeps=["x"])))
    assert "0 dependency-skipped" in text
    assert "0 never-ran" in text
    assert "0 NO OUTCOME" in text


def test_an_empty_optional_bucket_is_omitted() -> None:
    """intentional-skip and timed-out are omitted at zero: they are not the
    coverage-loss shapes this exists to surface, and six always-on lines would
    bury the three that matter."""
    text = vr.render_summary(vr.summarise(_row(failed_substeps=["x"])))
    assert "intentional-skip" not in text and "timed-out" not in text
    withthem = vr.render_summary(vr.summarise(_row(
        failed_substeps=["x"], intentional_skipped_nodes=["y"], timed_out_nodes=["z"])))
    assert "1 intentional-skip" in withthem and "1 timed-out" in withthem


# --------------------------------------------------------------------------
# The shapes that actually occurred.
# --------------------------------------------------------------------------

def test_the_real_coverage_loss_is_visible() -> None:
    """c0914b5d5e11 as measured: 9 failing of 66, and the gate that stopped running
    named on its own line rather than absorbed into the nine."""
    text = vr.render_summary(vr.summarise(_row(
        gates_run=66,
        failed_substeps=["e2e.manifest_backend_parity_c", "e2e.manifest_c_programs",
                         "e2e.manifest_language_runtimes", "privileged-test.cli_kvm",
                         "test.cli", "test.hermit_integration", "test.hermit_modes",
                         "test.liteinst_strict", "test.sabre_examples"],
        dependency_skipped_nodes=["scorecard.compatibility"],
        absent_nodes=["test.cli_kvm"])))
    assert "9 FAILING of 66 gates run" in text
    assert "1 dependency-skipped" in text and "scorecard.compatibility" in text
    assert "1 never-ran" in text and "test.cli_kvm" in text


def test_a_clean_run_with_a_no_outcome_node_does_not_read_as_clean() -> None:
    """⚠️ THE DANGEROUS CELL. Zero failures, but a planned node produced no outcome.
    Reporting '0 failing' alone would be true and completely misleading."""
    text = vr.render_summary(vr.summarise(_row(
        result="pass", failed_substeps=[], unaccounted_nodes=["scorecard.compatibility"])))
    assert "0 FAILING of 66 gates run" in text
    assert "1 NO OUTCOME" in text


def test_summarise_tolerates_missing_and_null_fields() -> None:
    """Older receipts predate some of these keys; a reader that throws on them
    would push callers back to reading raw rows, which is how the discipline gets
    bypassed."""
    s = vr.summarise({"commit": "abc", "gates_run": 5})
    assert s["failing"] == [] and s["never_ran"] == [] and s["no_outcome"] == []
    s2 = vr.summarise({"commit": "abc", "gates_run": 5, "coverage": None,
                       "dependency_skipped_nodes": None, "unaccounted_nodes": None})
    assert s2["dependency_skipped"] == [] and s2["never_ran"] == []
