"""The website's parity summary checker and parity panel.

The real-shaped data is RUN 1953, the first validate whose
`manifest_backend_parity_c` post-pass wrote parity records on both lanes
(`ci-hub/tests/fixtures/parity-run-1953`, provenance beside it). Its 192
sources, with the typed `unavailable_class` and `operand` their untyped
verdicts imply (`ledger-sources-typed.jsonl`; provenance.json records the
derivation), are wrapped exactly as `series.py append-parity` wraps them,
then summarized by the Python reference of hermit's `summarize_parity`; the
figures asserted below are that run's, not invented ones.

See https://github.com/rrnewton/hermit/issues/3301 for the parity design.
"""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import re
import sys
from fractions import Fraction
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
FIXTURE = REPOSITORY / "ci-hub/tests/fixtures/parity-run-1953"
RUN_ID = "validate-coord-s17-d3a0a4ae3d16-1790649970094317174-3328074-da07a1da"
HERMIT_SHA = "d3a0a4ae3d168565595ae4157b25ab8efbb1861a"
EMITTED_AT = "2026-09-28T21:40:00Z"
LEDGER_COMMIT = "b" * 40
#: RUN 1953: 175 measured cells, all diverged; dbt's 14 cells not compared;
#: 3 candidate cells with no result row, unmeasured and 0 in the floor. The
#: floor is the credit sum over 192 - 14 = 178 cells, the mean over the 175.
RUN_1953_LINE = (
    "parity: 0/192 matched; mean 0.052 over 175 measured; floor 0.051 over 175 measured of 192 selected "
    "(0 no golden; 14 not compared; 3 unmeasured: no-result-row 3) [inputs not equalized]"
)
#: The typed classes, written out here so a class moved between groups in the
#: code fails a test instead of moving silently with it.
NO_GOLDEN = (
    "determinism-mismatch", "timeout", "crash", "oom", "infrastructure-error", "sandbox-denied",
    "failed-untyped", "ended", "host-inapplicable",
)
NOT_COMPARED = ("inputs-not-equalized",)
UNMEASURED = (
    "log-not-retained", "log-unreadable", "no-result-row", "log-diff-failed", "invalid-test-id",
    "golden-not-written", "epoch-not-shared",
)
RECORD_MISSING = "record-missing"
GROUP_OF = {
    **{name: "no-golden" for name in NO_GOLDEN},
    **{name: "not-compared" for name in NOT_COMPARED},
    **{name: "unmeasured" for name in UNMEASURED},
    RECORD_MISSING: "record-missing",
}


def _load(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


render_page = _load(
    "parity_summary_test_render_page", REPOSITORY / "ci-hub/compatibility-website/render_page.py"
)
summary_module = render_page._parity_reader()
ledger = summary_module.parity_ledger_module()
WHY_DBT = ledger.INPUTS_NOT_EQUALIZABLE["dbt"]


def _sources() -> list[dict[str, Any]]:
    text = (FIXTURE / "ledger-sources-typed.jsonl").read_text(encoding="utf-8")
    return [ledger.loads(line, f"fixture line {n}") for n, line in enumerate(text.splitlines(), 1)]


def _rows(sources: list[dict[str, Any]], **overrides: Any) -> list[dict[str, Any]]:
    options = dict(
        producer="validate", run_id=RUN_ID, hermit_sha=HERMIT_SHA, emitted_at=EMITTED_AT,
        source_tree_dirty=False, host="fixturehost",
    )
    options.update(overrides)
    batch = ledger.build_batch(sources, **options)
    assert batch[0]["event_type"] == "parity.run"
    return [row for row in batch if row["event_type"] == "parity.record"]


#: The `parity/` store the carriage tests put at LEDGER_COMMIT; summaries
#: record its digest, as hermit's scorecard does.
SHARD = "parity/hermit/fixturehost/2026-09.jsonl"
STORE = {SHARD: b'{"line": 1}\n{"line": 2}\n'}
EARLIER_COMMIT = "e" * 40


def _summary(rows: list[dict[str, Any]], store: dict[str, bytes] = STORE) -> dict[str, Any]:
    return summary_module.summarize_rows(
        rows, store_sha256=summary_module.store_digest(store), series_commit=LEDGER_COMMIT,
        tool_hermit_commit="c" * 40,
    )


def _no_earlier_store(commit: str) -> Any:
    raise summary_module.ParitySummaryError(f"no store fixture at {commit}")


def _capture(summary: dict[str, Any] | None, *, store: dict[str, bytes] = STORE, store_files: int | None = None,
             markers: int = 1, earlier: dict[str, dict[str, bytes]] | None = None,
             ancestors: tuple[str, ...] = (), append: list[dict[str, Any]] | None = None,
             data: bytes | None = None) -> dict[str, Any]:
    """`capture_carriage` at LEDGER_COMMIT over `store`, as the exporter calls it."""
    if data is None and summary is not None:
        data = summary_module.encode_summary(summary)
    blob = None if data is None else hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()
    listed = append or []
    return summary_module.capture_carriage(
        LEDGER_COMMIT, summary_object=blob, summary_data=data, store=store,
        store_files=len(store) if store_files is None else store_files, run_marker_files=markers,
        read_store_at=(lambda commit: (earlier[commit], len(earlier[commit]))
                       if earlier and commit in earlier else _no_earlier_store(commit)),
        is_ancestor=lambda commit: commit in ancestors,
        append_refusals=listed[:summary_module.APPEND_REFUSAL_LIMIT], append_refusals_total=len(listed),
    )


def _carriage(summary: dict[str, Any], markers: int = 1) -> dict[str, Any]:
    carriage = _capture(summary, markers=markers)
    assert carriage["state"] == "current", carriage["refusal"]
    return carriage


def _model(carriage: dict[str, Any] | None, series_commit: str | None = LEDGER_COMMIT) -> Any:
    runs = {} if carriage is None else {"parity": carriage}
    return SimpleNamespace(runs=runs, data={"provenance": {"canonical_input_provenance": {"series_commit": series_commit}}})


def _panel(carriage: dict[str, Any] | None, series_commit: str | None = LEDGER_COMMIT) -> str:
    return render_page._parity_panel(_model(carriage, series_commit))


def _text(markup: str) -> str:
    markup = re.sub(r"</(p|tr|h2|h3|caption|li|summary)>", "\n", markup)
    markup = re.sub(r"</t[hd]>", " | ", markup)
    return re.sub(r"<[^>]+>", "", markup).replace("&#x27;", "'").replace("&quot;", '"').replace("&amp;", "&")


def _parity_table(markup: str) -> dict[str, list[str]]:
    """The panel's summary table: each row's label and its cells' text."""
    table = re.search(r'<table class="parity-summary">.*?<tbody>(.*?)</tbody>', markup, re.S)
    assert table is not None
    rows = {}
    for row in re.findall(r"<tr>(.*?)</tr>", table.group(1), re.S):
        label = re.sub(r"<[^>]+>", "", re.search(r"<th[^>]*>(.*?)</th>", row, re.S).group(1))
        rows[label] = [re.sub(r"<[^>]+>", "", cell) for cell in re.findall(r"<td>(.*?)</td>", row, re.S)]
    return rows


@pytest.fixture(scope="module")
def run_1953() -> dict[str, Any]:
    return _summary(_rows(_sources()))


def _lengths(verdict: str, credit: float | None) -> tuple[int | None, int | None, int | None]:
    """The (matched_prefix, left_len, right_len) a record with this credit has.

    A measured cell's credit is `credit(prefix, left, right)`, and a measured
    record without a prefix has null credit; an unmeasured one has neither.
    """
    if verdict not in {"matched", "diverged"}:
        return None, None, None
    if credit is None:
        return None, 8, 8
    ratio = Fraction(credit).limit_denominator(1000)
    lengths = (ratio.numerator, ratio.denominator, ratio.denominator)
    assert ledger.credit(*lengths) == credit, f"no small lengths give credit {credit!r}"
    return lengths


_IMPLIED: Any = object()
#: The class and operand these verdicts imply; a cell with any other verdict
#: names both.
_IMPLIED_CLASS = {
    "matched": (None, None),
    "diverged": (None, None),
    "refused": (None, None),
    "record-missing": ("record-missing", None),
    "inputs-not-equalized": ("inputs-not-equalized", "candidate"),
}


def _cell(test_id: str, backend: str = "kvm", *, verdict: str = "diverged", credit: Any = _IMPLIED,
          basis: str = "clean", unavailable_class: Any = _IMPLIED, operand: Any = _IMPLIED,
          reason: Any = _IMPLIED) -> dict[str, Any]:
    """One parity-summary cell, the way `_cell_from_row` shapes it.

    A matched cell has credit 1 and a diverged one 0.25 unless `credit` says
    otherwise; no other cell has a credit. An inputs-not-equalized cell's
    reason is hermit's declaration for its backend.
    """
    if credit is _IMPLIED:
        credit = {"matched": 1.0, "diverged": 0.25}.get(verdict)
    if unavailable_class is _IMPLIED or operand is _IMPLIED:
        implied_class, implied_operand = _IMPLIED_CLASS[verdict]
        unavailable_class = implied_class if unavailable_class is _IMPLIED else unavailable_class
        operand = implied_operand if operand is _IMPLIED else operand
    if reason is _IMPLIED:
        reason = {"matched": None, "diverged": None}.get(verdict, "fixture")
        if verdict == "inputs-not-equalized":
            reason = ledger.INPUTS_NOT_EQUALIZABLE[backend]
    prefix, left, right = _lengths(verdict, credit)
    diverged = verdict == "diverged"
    refused = verdict == "refused"
    return {
        "cell": f"{test_id}@{backend}", "emitted_cell": f"{test_id}@{backend}", "test_id": test_id,
        "backend": backend, "verdict": verdict, "unavailable_class": unavailable_class, "operand": operand,
        "reason": reason, "credit": credit, "credit_basis": basis if credit is not None else None,
        "matched_prefix": prefix, "left_len": left, "right_len": right,
        "first_divergent_record": 4 if diverged else None,
        "first_difference_field": "token 1: `a` vs `b`" if diverged else None,
        "first_difference_syscall": 7 if diverged else None,
        "reference_message": "left" if diverged else None,
        "candidate_message": "right" if diverged else None,
        "lane": None if refused else "portable", "node": None if refused else "manifest_backend_parity_c",
    }


def _document(cells: list[dict[str, Any]]) -> dict[str, Any]:
    """A one-run parity-summary/v1 document whose tallies come from `tally`."""
    per_backend = {
        backend: summary_module.tally([cell for cell in cells if cell["backend"] == backend])
        for backend in summary_module.BACKENDS
    }
    total = summary_module.tally(cells)
    line = summary_module.render_line(total)
    run = {
        "producer": "validate", "run_id": RUN_ID, "hermit_sha": HERMIT_SHA, "depth": None,
        "emitted_at": EMITTED_AT, "line": line, "per_backend": per_backend, "total": total, "cells": cells,
    }
    baseline = _summary(_rows(_sources()[:1]))
    document = copy.deepcopy(baseline)
    refused = [cell for cell in cells if cell["verdict"] == "refused"]
    document.update(
        rows_read=len(cells), producers=[run], duplicate_rows=0, refused_rows=len(refused),
        refusals=[{"path": "parity/rrnewton/fixturehost/2026-09.jsonl", "line": index + 1,
                   "message": f"{cell['cell']}: {cell['reason']}"} for index, cell in enumerate(refused)],
        runs=[{"producer": "validate", "run_id": RUN_ID, "hermit_sha": HERMIT_SHA, "depth": None,
               "emitted_at": EMITTED_AT, "headline": True, "line": line}],
    )
    summary_module.validate_summary(document)
    return document


RUN_AT = f"parity summary.producers[0] (validate run {RUN_ID})"


def _refused(document: dict[str, Any], message: str) -> None:
    with pytest.raises(summary_module.ParitySummaryError, match=re.escape(message)):
        summary_module.validate_summary(document)


def _named(document: dict[str, Any], index: int) -> tuple[dict[str, Any], str]:
    cell = document["producers"][0]["cells"][index]
    return cell, f"{RUN_AT}.cells[{index}] ({cell['cell']})"


# --- RUN 1953: real-shaped records, end to end -------------------------------------


def test_run_1953_total_line_is_the_committed_figure(run_1953: dict[str, Any]) -> None:
    summary_module.validate_summary(run_1953)
    [run] = run_1953["producers"]
    assert run["run_id"] == RUN_ID and run["hermit_sha"] == HERMIT_SHA
    assert run["line"] == RUN_1953_LINE
    total = run["total"]
    assert (total["selected"], total["measured"], total["matched"], total["diverged"]) == (192, 175, 0, 175)
    assert (total["no_golden"], total["not_compared"], total["unmeasured"], total["record_missing"],
            total["refused"]) == (0, 14, 3, 0, 0)
    assert (total["credited"], total["clean_credited"], total["floor_cells"]) == (175, 0, 178)
    assert not any(total["no_golden_by_class"].values())
    assert {k: v for k, v in total["unmeasured_by_class"].items() if v} == {"no-result-row": 3}
    # The mean divides by the 175 measured cells. The floor divides by the 178
    # left when dbt's 14 not-compared cells leave the 192 selected; the 3
    # unmeasured cells stay in it as 0.
    assert total["credit_sum"] == 9.072559709158929
    assert total["mean_credit"] == total["credit_sum"] / 175
    assert total["floor_credit"] == total["credit_sum"] / 178


def test_run_1953_per_backend_and_first_divergences(run_1953: dict[str, Any]) -> None:
    [run] = run_1953["producers"]
    lines = {backend: summary_module.render_line(run["per_backend"][backend]) for backend in summary_module.BACKENDS}
    assert lines == {
        "dbt": "parity: not compared: 0 measured of 14 selected (inputs cannot be equalized)",
        "kvm": "parity: 0/77 matched; mean 0.101 over 76 measured; floor 0.100 over 76 measured of 77 selected "
               "(0 no golden; 0 not compared; 1 unmeasured: no-result-row 1) [inputs not equalized]",
        "liteinst": "parity: 0/99 matched; mean 0.014 over 98 measured; floor 0.014 over 98 measured of 99 "
                    "selected (0 no golden; 0 not compared; 1 unmeasured: no-result-row 1) [inputs not equalized]",
        "sabre": "parity: 0/2 matched; mean 0.020 over 1 measured; floor 0.010 over 1 measured of 2 selected "
                 "(0 no golden; 0 not compared; 1 unmeasured: no-result-row 1) [inputs not equalized]",
    }
    assert {backend: run["per_backend"][backend]["line"] for backend in summary_module.BACKENDS} == lines
    assert {backend: run["per_backend"][backend]["floor_cells"] for backend in summary_module.BACKENDS} == {
        "dbt": 0, "kvm": 77, "liteinst": 99, "sabre": 2}
    dbt = run["per_backend"]["dbt"]
    assert (dbt["mean_credit"], dbt["floor_credit"], dbt["not_compared"]) == (None, None, 14)
    groups = {backend: run["per_backend"][backend]["first_divergence"] for backend in summary_module.BACKENDS}
    assert groups["dbt"] is None
    assert (groups["kvm"]["first_divergent_record"], groups["kvm"]["syscall"], groups["kvm"]["cells"],
            groups["kvm"]["of_diverged"]) == (13, 2, 74, 76)
    assert groups["kvm"]["field"] == "token 12: `Ok(93824992251904)` vs `Ok(2117632)`"
    assert (groups["liteinst"]["first_divergent_record"], groups["liteinst"]["syscall"],
            groups["liteinst"]["cells"], groups["liteinst"]["of_diverged"]) == (16, 3, 98, 98)
    assert groups["sabre"]["syscall"] is None and groups["sabre"]["example_cell"] == "backend-parity-c/pid-probe@sabre"


def test_run_1953_document_round_trips_through_the_carriage(run_1953: dict[str, Any]) -> None:
    carriage = _carriage(run_1953)
    assert carriage["present"] is True and carriage["bytes"] == len(carriage["summary_json"].encode())
    assert carriage["store_sha256"] == run_1953["generated_from"]["store_sha256"]
    assert (carriage["store_records"], carriage["unsummarized_records"], carriage["summarized_commit"]) == (
        2, 0, LEDGER_COMMIT)
    assert summary_module.validate_carriage(carriage) == run_1953
    assert summary_module.load_summary(carriage["summary_json"]) == run_1953


def test_run_1953_panel_quotes_the_line_and_the_data_as_of(run_1953: dict[str, Any]) -> None:
    text = _text(_panel(_carriage(run_1953)))
    assert "Parity (measured after determinism)" in text
    assert RUN_1953_LINE in text
    assert f"validate run {RUN_ID}" in text
    assert "Hermit commit d3a0a4ae3d16 measured" in text
    assert "parity store at ledger commit bbbbbbbbbbbb (scorecard/parity.json" in text
    assert "Stale:" not in text and "Parity summary refused" not in text
    assert ("Mean credit 0.0518 is the credit sum 9.0726 over the 175 measured cell(s); floor credit 0.0510 is "
            "the same sum over the 178 floor cell(s): the 192 selected cell(s) less 0 with no parity (no "
            "deterministic golden) and 14 not compared. Unmeasured, record-missing and refused cells count as 0 "
            "in the floor.") in text
    assert "kvm | 77 | 76 | 0 | 76 | 0 | 0 | 1 | 0 | 0 | 0.1010 | 0.0997 | 77 |" in text
    assert "dbt | 14 | 0 | 0 | 0 | 0 | 14 | 0 | 0 | 0 | n/a | n/a | 0 |" in text
    assert "TOTAL | 192 | 175 | 0 | 175 | 0 | 14 | 3 | 0 | 0 | 0.0518 | 0.0510 | 178 |" in text
    assert "No parity (no deterministic golden): 0 cells." in text
    assert "Not compared (inputs cannot be equalized), outside the mean and the floor: dbt 14; TOTAL 14." in text
    assert "no-result-row | 1 | 1 | 1 | 0 | 3 |" in text
    assert ("kvm | 13 | 2 | token 12: `Ok(93824992251904)` vs `Ok(2117632)` | 74 of 76 | "
            "backend-parity-c/aio-refusal@kvm |") in text
    assert ("Every cell that is not a full match (192 in all): 175 diverged, 0 no parity (no deterministic "
            "golden), 14 not compared, 3 unmeasured, 0 record-missing, 0 refused") in text
    assert "legacy-rerun history: the retired ptrace rerun, never part of the parity above" in text


# --- the typed classes: each in its group, read from the typed field alone ---------

#: For each class, a verdict and an operand it fits (`parity_ledger.check_class`).
FITTING = {
    "determinism-mismatch": ("nondeterministic", "reference"),
    "timeout": ("unavailable", "candidate"),
    "crash": ("reference-missing", "reference"),
    "oom": ("candidate-missing", "candidate"),
    "infrastructure-error": ("unavailable", "reference"),
    "sandbox-denied": ("unavailable", "candidate"),
    "failed-untyped": ("unavailable", "reference"),
    "ended": ("unavailable", "candidate"),
    "host-inapplicable": ("unavailable", "reference"),
    "inputs-not-equalized": ("inputs-not-equalized", "candidate"),
    "log-not-retained": ("unavailable", "reference"),
    "log-unreadable": ("candidate-missing", "candidate"),
    "no-result-row": ("reference-missing", "reference"),
    "log-diff-failed": ("unavailable", None),
    "invalid-test-id": ("unavailable", None),
    "golden-not-written": ("unavailable", "reference"),
    "epoch-not-shared": ("unavailable", None),
    "record-missing": ("record-missing", None),
}


def _classed(test_id: str, name: str, backend: str | None = None) -> dict[str, Any]:
    """A cell of class `name`: on dbt for inputs-not-equalized, else on kvm."""
    verdict, operand = FITTING[name]
    backend = backend or ("dbt" if name == "inputs-not-equalized" else "kvm")
    return _cell(test_id, backend, verdict=verdict, unavailable_class=name, operand=operand)


def test_the_class_sets_and_the_dedup_order_are_the_contract() -> None:
    for module in (summary_module, ledger):
        assert module.NO_GOLDEN_CLASSES == NO_GOLDEN
        assert module.NOT_COMPARED_CLASSES == NOT_COMPARED
        assert module.UNMEASURED_CLASSES == UNMEASURED
        assert module.RECORD_MISSING_CLASS == RECORD_MISSING
        assert module.UNAVAILABLE_CLASSES == (*NO_GOLDEN, *NOT_COMPARED, *UNMEASURED, RECORD_MISSING)
        assert module.OPERANDS == ("reference", "candidate")
    assert tuple(FITTING) == tuple(GROUP_OF) == ledger.UNAVAILABLE_CLASSES
    assert summary_module.INPUTS_NOT_EQUALIZABLE_BACKENDS == tuple(ledger.INPUTS_NOT_EQUALIZABLE) == ("dbt",)
    # A cell reported twice in one run keeps the first of these.
    assert summary_module.CELL_VERDICTS == (
        "refused", "record-missing", "nondeterministic", "unavailable", "reference-missing",
        "candidate-missing", "inputs-not-equalized", "diverged", "matched")
    assert summary_module.CELL_GROUPS == (
        "measured", "no-golden", "not-compared", "unmeasured", "record-missing", "refused")


@pytest.mark.parametrize("name", list(FITTING))
def test_each_class_is_counted_in_its_group(name: str) -> None:
    group = GROUP_OF[name]
    cell = _classed("t/x", name)
    document = _document([_cell("t/a", credit=0.5), cell])
    total = document["producers"][0]["total"]
    assert summary_module.cell_group(cell) == group
    counts = {field: total[field] for field in ("no_golden", "not_compared", "unmeasured", "record_missing")}
    field = group.replace("-", "_")
    assert counts == {key: int(key == field) for key in counts}
    for by_class in ("no_golden_by_class", "unmeasured_by_class"):
        assert total[by_class] == {key: int(key == name) for key in total[by_class]}
    # The mean is over the one measured cell whatever the class; the floor
    # leaves out a no-golden or not-compared cell and counts any other as 0.
    assert total["mean_credit"] == 0.5
    assert total["floor_credit"] == (0.5 if group in ("no-golden", "not-compared") else 0.25)
    part = {
        "no-golden": f"1 no golden: {name} 1; 0 not compared",
        "not-compared": "0 no golden; 1 not compared",
        "unmeasured": f"0 no golden; 0 not compared; 1 unmeasured: {name} 1",
        "record-missing": "0 no golden; 0 not compared; 1 record-missing",
    }[group]
    floor = "0.500" if group in ("no-golden", "not-compared") else "0.250"
    assert total["line"] == (f"parity: 0/2 matched; mean 0.500 over 1 measured; floor {floor} over 1 measured "
                             f"of 2 selected ({part})")


def test_an_unknown_class_is_refused_and_is_in_no_group() -> None:
    document = _document([_cell("t/a", credit=0.5), _classed("t/x", "timeout")])
    cell, at = _named(document, 1)
    cell["unavailable_class"] = "other"
    _refused(document, f"{at}: unavailable_class must be one of {', '.join(ledger.UNAVAILABLE_CLASSES)}, "
                       "got 'other'")
    assert ", ".join(ledger.UNAVAILABLE_CLASSES) == ", ".join((*NO_GOLDEN, *NOT_COMPARED, *UNMEASURED, RECORD_MISSING))
    with pytest.raises(summary_module.ParitySummaryError) as error:
        summary_module.cell_group(cell)
    assert str(error.value) == "cell t/x@kvm: unavailable_class 'other' is in no group"


def _unmeasured_like(source: dict[str, Any], verdict: str, unavailable_class: str, operand: str | None,
                     reason: str = "fixture: why this cell was not compared") -> dict[str, Any]:
    """A ledger source turned into an unmeasured one of the given verdict and class."""
    source.update(verdict=verdict, unavailable_class=unavailable_class, operand=operand, reason=reason)
    source["record"].update(
        verdict=verdict, unavailable_class=unavailable_class, operand=operand, reason=reason,
        inputs_equalized=False, credit=None, unequalized_credit=None, first_divergent_record=None,
        left_len=None, right_len=None, matched_prefix=None, first_difference=None,
        reference_log=None, candidate_log=None)
    return source


def test_a_cell_reported_twice_keeps_its_most_adverse_report() -> None:
    measured = _sources()[0]
    assert measured["verdict"] == "diverged"
    nondeterministic = _unmeasured_like(copy.deepcopy(measured), "nondeterministic", "determinism-mismatch",
                                        "reference")
    nondeterministic["source"]["lane"] = "privileged"
    missing = {
        "cell": measured["cell"], "test_id": measured["test_id"], "backend": measured["backend"],
        "verdict": "record-missing", "unavailable_class": "record-missing", "operand": None,
        "reason": "the parity post-pass of privileged/manifest_backend_parity_c left no parity.status.json",
        "run_id": None, "hermit_sha": None,
        "source": {"lane": "privileged", "node": "manifest_backend_parity_c", "post_pass_state": "absent",
                   "status_sha256": None, "records_sha256": None, "hermit_bin_sha256": None,
                   "scope_source": "expected-scope"},
        "record": None,
    }
    portable = _unmeasured_like(copy.deepcopy(measured), "nondeterministic", "determinism-mismatch", "reference")
    for pair, verdict, lane, line in (
        ((measured, nondeterministic), "nondeterministic", "privileged",
         "parity: 0/1 matched; mean n/a over 0 measured; floor n/a over 0 measured of 1 selected "
         "(1 no golden: determinism-mismatch 1; 0 not compared)"),
        ((missing, portable), "record-missing", "privileged",
         "parity: 0/1 matched; mean n/a over 0 measured; floor 0.000 over 0 measured of 1 selected "
         "(0 no golden; 0 not compared; 1 record-missing)"),
    ):
        for order in (pair, pair[::-1]):
            document = _summary(_rows(list(order)))
            summary_module.validate_summary(document)
            [run] = document["producers"]
            [cell] = run["cells"]
            assert (document["duplicate_rows"], cell["verdict"], cell["lane"], run["line"]) == (
                1, verdict, lane, line)


# --- honest accounting --------------------------------------------------------------


def test_zero_of_twelve_renders_as_zero_of_twelve_with_its_floor() -> None:
    cells = [_cell(f"t/{n:02d}", credit=0.5) for n in range(12)]
    document = _document(cells)
    line = document["producers"][0]["line"]
    assert line == ("parity: 0/12 matched; mean 0.500 over 12 measured; floor 0.500 over 12 measured of "
                    "12 selected (0 no golden; 0 not compared)")
    text = _text(_panel(_carriage(document)))
    assert line in text
    assert "kvm | 12 | 12 | 0 | 12 | 0 | 0 | 0 | 0 | 0 | 0.5000 | 0.5000 | 12 |" in text


def test_record_missing_keeps_the_denominator_and_lowers_the_floor() -> None:
    cells = [_cell(f"t/{n:02d}", credit=0.5) for n in range(9)]
    cells += [_cell(f"t/{n:02d}", verdict="record-missing") for n in range(9, 12)]
    document = _document(cells)
    total = document["producers"][0]["total"]
    assert (total["selected"], total["measured"], total["record_missing"], total["credited"],
            total["floor_cells"]) == (12, 9, 3, 9, 12)
    assert document["producers"][0]["line"] == (
        "parity: 0/12 matched; mean 0.500 over 9 measured; floor 0.375 over 9 measured of 12 selected "
        "(0 no golden; 0 not compared; 3 record-missing)")
    text = _text(_panel(_carriage(document)))
    assert "t/11@kvm | record-missing | record-missing | record-missing | none | none: 0 in the floor |" in text
    assert ("Mean credit 0.5000 is the credit sum 4.5000 over the 9 measured cell(s); floor credit 0.3750 is the "
            "same sum over the 12 floor cell(s): the 12 selected cell(s) less 0 with no parity (no deterministic "
            "golden) and 0 not compared") in text


def test_the_mean_is_over_the_measured_cells_and_the_floor_leaves_out_no_golden_ones() -> None:
    cells = [
        _cell("t/a", credit=0.5),
        _cell("t/b", credit=None),
        _classed("t/c", "no-result-row"),
        _cell("t/d", verdict="record-missing"),
        _cell("t/e", verdict="refused"),
        _classed("t/f", "determinism-mismatch"),
    ]
    result = summary_module.tally(cells)
    assert (result["selected"], result["measured"], result["no_golden"], result["unmeasured"],
            result["record_missing"], result["refused"], result["credited"], result["floor_cells"]) == (
        6, 2, 1, 1, 1, 1, 1, 5)
    # The mean is the credit sum over the 2 measured cells, the uncredited one
    # as 0. The floor is that sum over the 5 cells left when the no-golden one
    # leaves the 6 selected: the unmeasured, record-missing and refused cells
    # count as 0 in it, the no-golden cell as nothing at all.
    assert result["mean_credit"] == 0.25
    assert result["floor_credit"] == 0.1
    assert summary_module.render_line(result) == (
        "parity: 0/6 matched; mean 0.250 over 2 measured (1 credited); floor 0.100 over 2 measured of 6 selected "
        "(1 no golden: determinism-mismatch 1; 0 not compared; 1 unmeasured: no-result-row 1; 1 record-missing; "
        "1 refused)")
    text = _text(_panel(_carriage(_document(cells))))
    assert ("Mean credit 0.2500 is the credit sum 0.5000 over the 2 measured cell(s), 1 of them without a credit "
            "and counted as 0; floor credit 0.1000 is the same sum over the 5 floor cell(s): the 6 selected "
            "cell(s) less 1 with no parity (no deterministic golden) and 0 not compared") in text
    unmeasured = summary_module.tally([_classed("t/g", "no-result-row")])
    assert (unmeasured["mean_credit"], unmeasured["floor_credit"]) == (None, 0.0)
    assert unmeasured["line"] == ("parity: 0/1 matched; mean n/a over 0 measured; floor 0.000 over 0 measured of "
                                  "1 selected (0 no golden; 0 not compared; 1 unmeasured: no-result-row 1)")
    no_golden = summary_module.tally([_classed("t/h", "determinism-mismatch")])
    assert (no_golden["mean_credit"], no_golden["floor_credit"]) == (None, None)
    assert no_golden["line"] == ("parity: 0/1 matched; mean n/a over 0 measured; floor n/a over 0 measured of "
                                 "1 selected (1 no golden: determinism-mismatch 1; 0 not compared)")


def test_a_mean_that_averages_an_unmeasured_cell_as_zero_is_refused() -> None:
    document = _document([_cell("t/a", credit=0.5), _classed("t/b", "no-result-row")])
    total = document["producers"][0]["total"]
    total["mean_credit"] = total["credit_sum"] / total["selected"]
    _refused(document, f"{RUN_AT}.total: mean_credit 0.25 is not credit_sum / measured = 0.5")


def test_a_floor_that_counts_a_no_golden_cell_as_zero_is_refused() -> None:
    document = _document([_cell("t/a", credit=0.5), _classed("t/b", "determinism-mismatch")])
    total = document["producers"][0]["total"]
    total["floor_credit"] = total["credit_sum"] / total["selected"]
    _refused(document, f"{RUN_AT}.total: floor_credit 0.25 is not credit_sum / floor_cells = 0.5")


def test_every_credit_is_printed_with_the_cells_it_is_over() -> None:
    cells = [_cell("t/matched", verdict="matched"),
             *(_classed(f"t/{name}", name) for name in NO_GOLDEN)]
    document = _document(cells)
    line = ("parity: 1/10 matched; mean 1.000 over 1 measured; floor 1.000 over 1 measured of 10 selected "
            "(9 no golden: determinism-mismatch 1, timeout 1, crash 1, oom 1, infrastructure-error 1, "
            "sandbox-denied 1, failed-untyped 1, ended 1, host-inapplicable 1; 0 not compared)")
    run = document["producers"][0]
    assert run["line"] == run["total"]["line"] == run["per_backend"]["kvm"]["line"] == line
    coverage = "over 1 measured of 10 selected (9 no golden: "
    # The summary: the run's line, its total's and kvm's, and the run's brief.
    assert summary_module.encode_summary(document).decode().count(coverage) == 4
    # The panel: the headline and the kvm and TOTAL rows' coverage.
    markup = _panel(_carriage(document))
    assert _text(markup).count(coverage) == 3
    table = _parity_table(markup)
    assert table["kvm"] == table["TOTAL"] == ["10", "1", "1", "0", "9", "0", "0", "0", "0", "1.0000", "1.0000",
                                              "1", line]


def _dbt_run(cells: list[dict[str, Any]]) -> tuple[dict[str, Any], str]:
    document = _document(cells)
    return document, _text(_panel(_carriage(document)))


def test_a_backend_whose_inputs_cannot_be_equalized_enters_no_mean_or_floor() -> None:
    not_compared = [_cell(f"d/ine-{n:02d}", "dbt", verdict="inputs-not-equalized") for n in range(14)]
    # Every selected dbt cell not compared.
    document, text = _dbt_run(not_compared)
    run = document["producers"][0]
    line = "parity: not compared: 0 measured of 14 selected (inputs cannot be equalized)"
    assert run["line"] == run["total"]["line"] == run["per_backend"]["dbt"]["line"] == line
    assert f"dbt | 14 | 0 | 0 | 0 | 0 | 14 | 0 | 0 | 0 | n/a | n/a | 0 | {line} |" in text
    assert ("Mean credit is n/a: no selected cell was measured; floor credit is n/a: no cell is in the floor, "
            "which is the 14 selected cell(s) less 0 with no parity (no deterministic golden) and 14 not "
            "compared.") in text
    assert "0.0000" not in text and "0.000 " not in text
    # Two with no golden: the ordinary coverage line, each credit n/a.
    document, text = _dbt_run([_classed("d/nondet-0", "determinism-mismatch", "dbt"),
                               _classed("d/nondet-1", "determinism-mismatch", "dbt"), *not_compared[2:]])
    dbt = document["producers"][0]["per_backend"]["dbt"]
    assert (dbt["selected"], dbt["no_golden"], dbt["not_compared"], dbt["floor_cells"]) == (14, 2, 12, 0)
    assert dbt["line"] == ("parity: 0/14 matched; mean n/a over 0 measured; floor n/a over 0 measured of "
                           "14 selected (2 no golden: determinism-mismatch 2; 12 not compared)")
    assert "dbt | 14 | 0 | 0 | 0 | 2 | 12 | 0 | 0 | 0 | n/a | n/a | 0 |" in text
    # Its record-missing and refused cells stay counted, and stay out of every
    # floor: dbt's own, and the total's beside a measured kvm cell.
    document, text = _dbt_run([_cell("k/matched", verdict="matched"), *not_compared[2:],
                               _cell("d/missing", "dbt", verdict="record-missing"),
                               _cell("d/refused", "dbt", verdict="refused")])
    run = document["producers"][0]
    dbt, total = run["per_backend"]["dbt"], run["total"]
    assert (dbt["selected"], dbt["not_compared"], dbt["record_missing"], dbt["refused"], dbt["floor_cells"]) == (
        14, 12, 1, 1, 0)
    assert (dbt["mean_credit"], dbt["floor_credit"]) == (None, None)
    assert dbt["line"] == ("parity: 0/14 matched; mean n/a over 0 measured; floor n/a over 0 measured of "
                           "14 selected (0 no golden; 12 not compared; 1 record-missing; 1 refused; floor excludes "
                           "2 record-missing or refused cell(s) whose inputs cannot be equalized)")
    assert (total["selected"], total["floor_cells"], total["mean_credit"], total["floor_credit"]) == (
        15, 1, 1.0, 1.0)
    assert run["line"] == ("parity: 1/15 matched; mean 1.000 over 1 measured; floor 1.000 over 1 measured of "
                           "15 selected (0 no golden; 12 not compared; 1 record-missing; 1 refused; floor excludes "
                           "2 record-missing or refused cell(s) whose inputs cannot be equalized)")
    assert ("floor credit 1.0000 is the same sum over the 1 floor cell(s): the 15 selected cell(s) less 0 with "
            "no parity (no deterministic golden) and 12 not compared, and less 2 record-missing or refused "
            "cell(s) whose inputs cannot be equalized.") in text
    assert ("d/missing@dbt | record-missing | record-missing | record-missing | none | "
            "none: outside every floor (inputs cannot be equalized) |") in text
    assert "d/refused@dbt | refused | refused | none | none | none: outside every floor (inputs cannot be equalized) |" in text


def test_a_dbt_cell_with_no_golden_is_accepted_and_in_no_floor() -> None:
    cell = _classed("d/nondet", "determinism-mismatch", "dbt")
    document = _document([_cell("t/a", credit=0.5), cell])
    run = document["producers"][0]
    assert summary_module.cell_group(cell) == "no-golden" and not summary_module.in_a_floor(cell)
    assert (run["per_backend"]["dbt"]["no_golden"], run["per_backend"]["dbt"]["floor_cells"]) == (1, 0)
    assert (run["total"]["floor_cells"], run["total"]["floor_credit"]) == (1, 0.5)


#: A corpus with every group, on every backend: kvm, liteinst and sabre
#: measured, and dbt with two no-golden cells beside twelve not compared.
MIXED = [
    ("m/matched", "kvm", dict(verdict="matched")),
    ("m/diverged", "kvm", dict(credit=0.3)),
    ("m/nondet", "kvm", dict(verdict="nondeterministic", unavailable_class="determinism-mismatch", operand="reference")),
    ("m/no-row", "kvm", dict(verdict="candidate-missing", unavailable_class="no-result-row", operand="candidate")),
    ("m/missing", "kvm", dict(verdict="record-missing")),
    ("m/diverged", "liteinst", dict(credit=0.5, basis="unequalized")),
    ("m/timeout", "liteinst", dict(verdict="unavailable", unavailable_class="timeout", operand="candidate")),
    ("m/not-retained", "liteinst", dict(verdict="unavailable", unavailable_class="log-not-retained",
                                        operand="reference")),
    ("m/refused", "sabre", dict(verdict="refused")),
    ("m/diverged", "sabre", dict(credit=0.4)),
    ("m/nondet-0", "dbt", dict(verdict="nondeterministic", unavailable_class="determinism-mismatch", operand="reference")),
    ("m/nondet-1", "dbt", dict(verdict="nondeterministic", unavailable_class="determinism-mismatch", operand="candidate")),
    *((f"m/ine-{n:02d}", "dbt", dict(verdict="inputs-not-equalized")) for n in range(12)),
]
MIXED_LINES = {
    "kvm": "parity: 1/5 matched; mean 0.650 over 2 measured; floor 0.325 over 2 measured of 5 selected "
           "(1 no golden: determinism-mismatch 1; 0 not compared; 1 unmeasured: no-result-row 1; 1 record-missing)",
    "liteinst": "parity: 0/3 matched; mean 0.500 over 1 measured; floor 0.250 over 1 measured of 3 selected "
                "(1 no golden: timeout 1; 0 not compared; 1 unmeasured: log-not-retained 1) [inputs not equalized]",
    "sabre": "parity: 0/2 matched; mean 0.400 over 1 measured; floor 0.200 over 1 measured of 2 selected "
             "(0 no golden; 0 not compared; 1 refused)",
    "dbt": "parity: 0/14 matched; mean n/a over 0 measured; floor n/a over 0 measured of 14 selected "
           "(2 no golden: determinism-mismatch 2; 12 not compared)",
    "TOTAL": "parity: 1/24 matched; mean 0.550 over 4 measured; floor 0.275 over 4 measured of 24 selected "
             "(4 no golden: determinism-mismatch 3, timeout 1; 12 not compared; 2 unmeasured: log-not-retained 1, "
             "no-result-row 1; 1 record-missing; 1 refused) [inputs equalized for 3 of 4 credited]",
}


def test_a_mixed_corpus_renders_every_group_with_its_breakdown() -> None:
    document = _document([_cell(test_id, backend, **options) for test_id, backend, options in MIXED])
    run = document["producers"][0]
    assert {backend: run["per_backend"][backend]["line"] for backend in summary_module.BACKENDS} == {
        backend: MIXED_LINES[backend] for backend in summary_module.BACKENDS}
    assert run["line"] == run["total"]["line"] == MIXED_LINES["TOTAL"]
    markup = _panel(_carriage(document))
    assert _parity_table(markup) == {
        "kvm": ["5", "2", "1", "1", "1", "0", "1", "1", "0", "0.6500", "0.3250", "4", MIXED_LINES["kvm"]],
        "liteinst": ["3", "1", "0", "1", "1", "0", "1", "0", "0", "0.5000", "0.2500", "2", MIXED_LINES["liteinst"]],
        "sabre": ["2", "1", "0", "1", "0", "0", "0", "0", "1", "0.4000", "0.2000", "2", MIXED_LINES["sabre"]],
        "dbt": ["14", "0", "0", "0", "2", "12", "0", "0", "0", "n/a", "n/a", "0", MIXED_LINES["dbt"]],
        "TOTAL": ["24", "4", "1", "3", "4", "12", "2", "1", "1", "0.5500", "0.2750", "8", MIXED_LINES["TOTAL"]],
    }
    text = _text(markup)
    assert ("Mean credit 0.5500 is the credit sum 2.2000 over the 4 measured cell(s); floor credit 0.2750 is the "
            "same sum over the 8 floor cell(s): the 24 selected cell(s) less 4 with no parity (no deterministic "
            "golden) and 12 not compared. Unmeasured, record-missing and refused cells count as 0 in the floor.") in text
    assert ("No parity (no deterministic golden): 4 cell(s) by class, outside the mean and the floor\n"
            "Class | kvm | liteinst | sabre | dbt | TOTAL | \n"
            "determinism-mismatch | 1 | 0 | 0 | 2 | 3 | \n"
            "timeout | 0 | 1 | 0 | 0 | 1 | \n") in text
    assert "Not compared (inputs cannot be equalized), outside the mean and the floor: dbt 12; TOTAL 12." in text
    assert ("Unmeasured: 2 cell(s) by class, each 0 in the floor\n"
            "Class | kvm | liteinst | sabre | dbt | TOTAL | \n"
            "log-not-retained | 0 | 1 | 0 | 0 | 1 | \n"
            "no-result-row | 1 | 0 | 0 | 0 | 1 | \n") in text
    assert ("Every cell that is not a full match (23 in all): 3 diverged, 4 no parity (no deterministic golden), "
            "12 not compared, 2 unmeasured, 1 record-missing, 1 refused") in text
    for row in (
        "m/nondet-0@dbt | no parity (no deterministic golden) | nondeterministic | determinism-mismatch | "
        "reference | none: no parity, outside the mean and the floor |",
        "m/ine-00@dbt | not compared | inputs-not-equalized | inputs-not-equalized | candidate | "
        "none: not compared, outside the mean and the floor |",
        "m/no-row@kvm | unmeasured | candidate-missing | no-result-row | candidate | none: 0 in the floor |",
        "m/not-retained@liteinst | unmeasured | unavailable | log-not-retained | reference | none: 0 in the floor |",
        "m/missing@kvm | record-missing | record-missing | record-missing | none | none: 0 in the floor |",
        "m/refused@sabre | refused | refused | none | none | none: 0 in the floor |",
        "m/diverged@liteinst | diverged | diverged | none | none | 0.5000 (inputs not equalized) |",
        "m/diverged@kvm | diverged | diverged | none | none | 0.3000 |",
    ):
        assert row in text
    # No cell without a golden is shown as 0 credit or as a failure.
    no_golden = [row.split(" | ") for row in text.splitlines() if " | no parity (no deterministic golden) | " in row]
    assert sorted(columns[0] for columns in no_golden) == [
        "m/nondet-0@dbt", "m/nondet-1@dbt", "m/nondet@kvm", "m/timeout@liteinst"]
    assert {columns[5] for columns in no_golden} == {"none: no parity, outside the mean and the floor"}


def _class_meanings(markup: str) -> list[list[tuple[str, str]]]:
    """Each class legend of the panel, in order: its (class, meaning) entries."""
    legends = re.findall(r'<dl class="facts compact-facts parity-class-meanings">(.*?)</dl>', markup, re.S)
    return [
        [(name, meaning.replace("&#x27;", "'").replace("&quot;", '"').replace("&amp;", "&"))
         for name, meaning in re.findall(r"<dt><code>(.*?)</code></dt><dd>(.*?)</dd>", legend, re.S)]
        for legend in legends
    ]


def test_every_class_the_tables_can_count_has_a_meaning() -> None:
    assert tuple(render_page.PARITY_CLASS_MEANINGS) == (*NO_GOLDEN, *UNMEASURED)
    assert tuple(render_page.PARITY_CLASS_MEANINGS) == (*summary_module.NO_GOLDEN_CLASSES,
                                                        *summary_module.UNMEASURED_CLASSES)
    assert all(isinstance(text, str) and text.strip() for text in render_page.PARITY_CLASS_MEANINGS.values())


def test_the_panel_explains_each_class_it_counts_and_no_other() -> None:
    document = _document([_cell(test_id, backend, **options) for test_id, backend, options in MIXED])
    meaning = render_page.PARITY_CLASS_MEANINGS
    assert _class_meanings(_panel(_carriage(document))) == [
        [(name, meaning[name]) for name in ("determinism-mismatch", "timeout")],
        [(name, meaning[name]) for name in ("log-not-retained", "no-result-row")],
    ]
    # Run 1953 has no cell without a golden and three with no result row.
    assert _class_meanings(_panel(_carriage(_summary(_rows(_sources()))))) == [
        [("no-result-row", meaning["no-result-row"])]]


#: The definition of log-diff-failed that hermit's `UnavailableClass`
#: documents, as S12 widened it (https://github.com/rrnewton/hermit/issues/3301).
LOG_DIFF_FAILED_MEANING = (
    "The parity tool could not produce a usable comparison of two deterministic, retained logs. There are three "
    "cases. (1) log-diff returned a report that carries no verdict: NoResult, Refused, NoComparableMessages or "
    "IdenticalSoFar. IdenticalSoFar should be impossible in one-shot mode, so seeing it means a tool defect. "
    "(2) A matched or diverged report failed require_cross_backend_evidence. (3) from_comparison could not record "
    "the report. Every other exit from compare is also this class: deadline, output directory, paths, spawn, time "
    "bound, unreadable or contradictory report, and wrong inputs."
)


def test_a_log_diff_failed_cell_is_explained_with_the_widened_definition() -> None:
    document = _document([
        _cell("t/a", credit=0.5),
        _cell("t/x", verdict="unavailable", unavailable_class="log-diff-failed", operand=None),
    ])
    assert _class_meanings(_panel(_carriage(document))) == [[("log-diff-failed", LOG_DIFF_FAILED_MEANING)]]


def test_a_below_one_credit_never_prints_as_one() -> None:
    assert render_page._parity_credit(0.99996) == "0.9999"
    assert render_page._parity_credit(1.0) == "1.0000"
    assert render_page._parity_credit(None) == "n/a"


# --- malformed summaries and carriages are refused loudly ---------------------------


@pytest.mark.parametrize(
    ("tamper", "message"),
    [
        (lambda d: d["producers"][0]["total"].__setitem__("matched", 1),
         r"\.total: measured 175 is not matched \+ diverged"),
        (lambda d: d["producers"][0].__setitem__("line", RUN_1953_LINE.replace("0/192", "192/192")),
         r"line 'parity: 192/192 matched; .*' is not its total's 'parity: 0/192 matched; "),
        (lambda d: d["producers"][0]["total"].__setitem__("credit_sum", 99.0),
         r"\.total: mean_credit .* is not credit_sum / measured = "),
        (lambda d: d["producers"][0]["cells"][0].__setitem__("credit", 1.0),
         r"backend-parity-c/aio-refusal@kvm\): a diverged cell's credit 1\.0 must be below 1"),
        (lambda d: d.__setitem__("schema", "parity-summary/v0"),
         r"schema must be parity-summary/v1, got 'parity-summary/v0'"),
    ],
)
def test_a_tampered_summary_is_refused_with_its_reason(run_1953: dict[str, Any], tamper: Any, message: str) -> None:
    document = copy.deepcopy(run_1953)
    tamper(document)
    with pytest.raises(summary_module.ParitySummaryError, match=message):
        summary_module.validate_summary(document)


# --- one tamper per accounting guard, each refused with its own message -------------
#
# Every case starts from a summary that validates, changes the one field its
# guard covers, and asserts that guard's message. A guard deleted or inverted
# lets the tampered summary through or refuses it for another reason, and the
# case fails.

# RUN 1953's cells[0] is backend-parity-c/aio-refusal@kvm, diverged at 12 of
# 106 | 106; cells[14] and [160] are dbt inputs-not-equalized; cells[59] is
# a liteinst candidate-missing cell of class no-result-row.


def test_a_matched_cell_must_carry_full_credit(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 0)
    cell.update(verdict="matched", first_divergent_record=None, first_difference_field=None,
                first_difference_syscall=None, reference_message=None, candidate_message=None)
    _refused(document, f"{at}: a matched cell's credit must be 1, got 0.11320754716981132")
    cell.update(credit=None, credit_basis=None, matched_prefix=None)
    _refused(document, f"{at}: a matched cell's credit must be 1, got None")


def test_only_a_measured_cell_is_credited(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 59)
    assert (cell["verdict"], cell["unavailable_class"], cell["operand"]) == (
        "candidate-missing", "no-result-row", "candidate")
    cell.update(credit=0.5, credit_basis="unequalized")
    _refused(document, f"{at}: a candidate-missing cell carries credit 0.5; only a measured cell is credited")


def test_a_cell_credit_is_recomputed_from_its_matched_prefix(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 0)
    cell["credit"] = 0.5
    _refused(document, f"{at}: credit 0.5 is not the 0.11320754716981132 its matched prefix 12 of 106 | 106 gives")
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 0)
    cell["matched_prefix"] = 13
    _refused(document, f"{at}: credit 0.11320754716981132 is not the {13 / 106!r} its matched prefix 13 of 106 | 106 gives")
    cell["matched_prefix"] = None
    _refused(document, f"{at}: credit 0.11320754716981132 is not the None its matched prefix None of 106 | 106 gives")


def test_a_measured_cell_needs_both_lengths(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 0)
    cell["right_len"] = None
    _refused(document, f"{at}: a measured cell needs both compared lengths")


def test_a_matched_cell_carries_no_divergence_position(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 0)
    cell.update(verdict="matched", matched_prefix=106, credit=1.0)
    _refused(document, f"{at}: a matched cell carries no divergence position")


def test_an_unmeasured_cell_carries_no_lengths_or_divergence(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 59)
    cell["left_len"] = 5
    _refused(document, f"{at}: a candidate-missing cell is unmeasured and carries no compared lengths or divergence")


_TIMEOUT = dict(verdict="unavailable", unavailable_class="timeout", operand="candidate")
_NO_ROW = dict(verdict="candidate-missing", unavailable_class="no-result-row", operand="candidate")
_NONDETERMINISTIC = dict(verdict="nondeterministic", unavailable_class="determinism-mismatch", operand="reference")
_NOT_COMPARED = dict(verdict="inputs-not-equalized")
_MEASURED_DBT = f"dbt inputs cannot be equalized, so it reports no measured comparison and never equal inputs: {WHY_DBT}"


@pytest.mark.parametrize(
    ("backend", "target", "change", "message"),
    [
        # The operand is one of the closed set, or null.
        ("kvm", _NO_ROW, {"operand": "both"}, "operand must be one of reference, candidate, got 'both'"),
        # Only a backend hermit declares inputs-not-equalizable (dbt) is not compared...
        ("kvm", _TIMEOUT, {"verdict": "inputs-not-equalized", "unavailable_class": "inputs-not-equalized"},
         "kvm inputs can be equalized; a comparison with unequal inputs is measured and reports unequalized_credit"),
        # ...and such a backend is never measured.
        ("dbt", _NOT_COMPARED, {"verdict": "diverged", "unavailable_class": None, "operand": None}, _MEASURED_DBT),
        ("dbt", _NOT_COMPARED, {"verdict": "matched", "unavailable_class": None, "operand": None}, _MEASURED_DBT),
        # A measured verdict has no class and no operand.
        ("kvm", dict(verdict="matched"), {"unavailable_class": "timeout"},
         "a matched verdict is measured and carries no unavailable_class, got timeout"),
        ("kvm", {}, {"operand": "candidate"}, "a diverged verdict is measured and names no operand, got candidate"),
        # Any other verdict has a class that fits it.
        ("kvm", _TIMEOUT, {"unavailable_class": None}, "verdict unavailable needs an unavailable_class"),
        ("kvm", _TIMEOUT, {"unavailable_class": "determinism-mismatch"},
         "unavailable_class determinism-mismatch cannot go with verdict unavailable; it needs nondeterministic"),
        ("kvm", _NONDETERMINISTIC, {"unavailable_class": "timeout"},
         "unavailable_class timeout cannot go with verdict nondeterministic; it needs unavailable or "
         "reference-missing or candidate-missing"),
        ("dbt", _NOT_COMPARED, {"verdict": "unavailable"},
         "unavailable_class inputs-not-equalized cannot go with verdict unavailable; it needs inputs-not-equalized"),
        ("dbt", _NOT_COMPARED, {"unavailable_class": "timeout"},
         "unavailable_class timeout cannot go with verdict inputs-not-equalized; it needs unavailable or "
         "reference-missing or candidate-missing"),
        ("kvm", dict(verdict="record-missing"), {"unavailable_class": "timeout"},
         "unavailable_class timeout cannot go with verdict record-missing; it needs unavailable or "
         "reference-missing or candidate-missing"),
        ("kvm", dict(verdict="record-missing"), {"verdict": "unavailable"},
         "unavailable_class record-missing cannot go with verdict unavailable; it needs record-missing"),
        ("kvm", _NO_ROW, {"verdict": "nondeterministic"},
         "unavailable_class no-result-row cannot go with verdict nondeterministic; it needs unavailable or "
         "reference-missing or candidate-missing"),
        ("kvm", dict(verdict="candidate-missing", unavailable_class="log-unreadable", operand="candidate"),
         {"verdict": "unavailable"},
         "unavailable_class log-unreadable cannot go with verdict unavailable; it needs reference-missing or "
         "candidate-missing"),
        ("kvm", dict(verdict="unavailable", unavailable_class="no-result-row", operand="reference"), {"operand": None},
         "unavailable_class no-result-row needs operand reference or candidate, got null"),
        # dbt may have no golden, but no comparison is attempted, so nothing is unmeasured.
        ("dbt", _NONDETERMINISTIC, {"verdict": "unavailable", "unavailable_class": "log-not-retained"},
         "dbt inputs cannot be equalized, so no comparison is attempted and it carries no unmeasured class, "
         "got log-not-retained"),
        # The operand fits the class, and a side the verdict names.
        ("dbt", _NOT_COMPARED, {"operand": "reference"},
         "unavailable_class inputs-not-equalized needs operand candidate, got reference"),
        ("kvm", dict(verdict="unavailable", unavailable_class="log-diff-failed", operand=None), {"operand": "candidate"},
         "unavailable_class log-diff-failed needs operand null, got candidate"),
        ("kvm", dict(verdict="unavailable", unavailable_class="golden-not-written", operand="reference"),
         {"operand": "candidate"}, "unavailable_class golden-not-written needs operand reference, got candidate"),
        ("kvm", _TIMEOUT, {"operand": None}, "unavailable_class timeout needs operand reference or candidate, got null"),
        ("kvm", dict(verdict="record-missing"), {"operand": "reference"},
         "unavailable_class record-missing needs operand null, got reference"),
        ("kvm", _NO_ROW, {"operand": "reference"}, "verdict candidate-missing names the candidate operand, got reference"),
        # A refused row was never admitted, so it has neither.
        ("kvm", dict(verdict="refused"), {"unavailable_class": "timeout"},
         "a refused cell has no unavailable class or operand"),
        ("kvm", dict(verdict="refused"), {"operand": "candidate"}, "a refused cell has no unavailable class or operand"),
    ],
)
def test_each_class_guard_refuses_its_own_tamper(backend: str, target: dict[str, Any], change: dict[str, Any],
                                                 message: str) -> None:
    document = _document([_cell("t/a", credit=0.5), _cell("t/x", backend, **target)])
    cell, at = _named(document, 1)
    cell.update(change)
    _refused(document, f"{at}: {message}")


@pytest.mark.parametrize("operand", ["reference", "candidate"])
def test_an_unavailable_no_result_row_cell_is_unmeasured_on_either_side(operand: str) -> None:
    # hermit's `retained_operand` makes a rejected row, or a retry history it
    # refuses, `unavailable` with class no-result-row; its side is the operand.
    cell = _cell("t/x", verdict="unavailable", unavailable_class="no-result-row", operand=operand)
    document = _document([_cell("t/a", credit=0.5), cell])
    total = document["producers"][0]["total"]
    assert summary_module.cell_group(cell) == "unmeasured"
    assert (total["unmeasured"], total["unmeasured_by_class"]["no-result-row"], total["floor_cells"]) == (1, 1, 2)
    assert total["line"] == ("parity: 0/2 matched; mean 0.500 over 1 measured; floor 0.250 over 1 measured of 2 "
                             "selected (0 no golden; 0 not compared; 1 unmeasured: no-result-row 1)")
    assert (f"t/x@kvm | unmeasured | unavailable | no-result-row | {operand} | none: 0 in the floor |"
            in _text(_panel(_carriage(document))))


def test_a_declared_backend_has_no_measured_cell_in_its_run(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    measured, _ = _named(document, 0)
    cell, at = _named(document, 14)
    assert (cell["verdict"], cell["reason"]) == ("inputs-not-equalized", WHY_DBT)
    for field in ("verdict", "reason", "unavailable_class", "operand", "credit", "credit_basis", "matched_prefix",
                  "left_len", "right_len", "first_divergent_record", "first_difference_field",
                  "first_difference_syscall", "reference_message", "candidate_message"):
        cell[field] = measured[field]
    _refused(document, f"{at}: {_MEASURED_DBT}")


def test_a_run_declares_one_reason_per_backend(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    first, _ = _named(document, 14)
    cell, _ = _named(document, 160)
    cell["reason"] = "some other declaration"
    _refused(document, f"{RUN_AT}: cell {cell['cell']} declares dbt inputs not equalizable for "
                       f"'some other declaration', but another cell of the run declares {first['reason']!r}; "
                       "a run's hermit makes one declaration per backend")


def test_the_floor_is_the_credit_sum_over_the_floor_cells(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    kvm = document["producers"][0]["per_backend"]["kvm"]
    floor = kvm["floor_credit"]
    kvm["floor_credit"] = kvm["mean_credit"]
    _refused(document, f"{RUN_AT}.per_backend.kvm: floor_credit {kvm['mean_credit']!r} is not "
                       f"credit_sum / floor_cells = {floor!r}")


def test_the_floor_cells_are_the_selected_less_the_no_golden_and_not_compared(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    kvm = document["producers"][0]["per_backend"]["kvm"]
    kvm.update(floor_cells=76, floor_credit=kvm["credit_sum"] / 76)
    kvm["line"] = summary_module.render_line(kvm)
    _refused(document, f"{RUN_AT}.per_backend.kvm: floor_cells 76 is not selected - no golden - not compared = 77")
    document = copy.deepcopy(run_1953)
    dbt = document["producers"][0]["per_backend"]["dbt"]
    dbt.update(floor_cells=14, floor_credit=0.0)
    _refused(document, f"{RUN_AT}.per_backend.dbt: floor_cells 14 must be 0: dbt inputs cannot be equalized, so "
                       "none of its cells is in a floor")
    document = copy.deepcopy(run_1953)
    total = document["producers"][0]["total"]
    total.update(floor_cells=179, floor_credit=total["credit_sum"] / 179)
    _refused(document, f"{RUN_AT}.total: floor_cells 179 exceeds selected - no golden - not compared = 178")


def test_every_tally_field_is_recomputed_from_the_cells(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    total = document["producers"][0]["total"]
    stated = dict(total["unmeasured_by_class"])
    total["unmeasured_by_class"].update({"no-result-row": 2, "log-not-retained": 1})  # same sum, 3
    total["line"] = summary_module.render_line(total)
    _refused(document, f"{RUN_AT}.total: unmeasured_by_class {total['unmeasured_by_class']!r} disagrees "
                       f"with its cells, which give {stated!r}")
    document = copy.deepcopy(run_1953)
    kvm = document["producers"][0]["per_backend"]["kvm"]
    group = dict(kvm["first_divergence"])
    kvm["first_divergence"]["cells"] = 73
    _refused(document, f"{RUN_AT}.per_backend.kvm: first_divergence {kvm['first_divergence']!r} disagrees "
                       f"with its cells, which give {group!r}")


def test_the_credit_sum_is_the_sum_of_the_cells_credits(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    kvm = document["producers"][0]["per_backend"]["kvm"]
    true_sum = kvm["credit_sum"]
    kvm["credit_sum"] = true_sum + 0.5
    kvm["mean_credit"] = kvm["credit_sum"] / kvm["measured"]
    kvm["floor_credit"] = kvm["credit_sum"] / kvm["floor_cells"]
    kvm["line"] = summary_module.render_line(kvm)
    _refused(document, f"{RUN_AT}.per_backend.kvm: credit_sum {kvm['credit_sum']!r} disagrees with its cells, "
                       f"which give {true_sum!r}")


def _with_one_refused_cell() -> dict[str, Any]:
    document = _document([_cell("t/a", credit=0.5), _cell("t/b", verdict="refused")])
    assert (document["refused_rows"], len(document["refusals"])) == (1, 1)
    return document


def test_every_refused_row_is_listed() -> None:
    document = _with_one_refused_cell()
    document["refusals"] = []
    _refused(document, "parity summary: 1 refused row(s) but only 0 refusal(s) listed")


def test_a_run_counts_no_more_refused_cells_than_the_store_refused() -> None:
    document = _with_one_refused_cell()
    document.update(refused_rows=0, refusals=[])
    _refused(document, f"parity summary: validate run {RUN_ID} counts more refused cells than the store refused")


def test_the_legacy_line_is_rendered_from_its_entries(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    document["legacy_rerun"]["line"] = "legacy-rerun: 3 matched"
    _refused(document, "parity summary.legacy_rerun.line 'legacy-rerun: 3 matched' is not "
                       "'legacy-rerun: no retired ptrace-rerun comparison is retained'")


def test_the_panel_withholds_the_retired_placeholder_word_in_the_scorecards_texts() -> None:
    """render_site refuses every page when one page holds the retired
    placeholder word. The scorecard's texts reach the panel as written: a
    refused row's message and reason, and each reason it dropped a legacy
    observation for. The panel shows each with the word withheld."""
    word = "Unavail" + "able"
    reason = f"verdict {word} is not a ledger verdict; {word} is the Debug form of unavailable"
    document = _document([_cell("t/a", credit=0.5), _cell("t/b", verdict="refused", reason=reason)])
    document["legacy_dropped"] = {f"legacy verdict {word}": 2}
    summary_module.validate_summary(document)
    panel = _panel(_carriage(document))
    assert word not in panel
    shown = ("verdict [retired word withheld] is not a ledger verdict; [retired word withheld] is the Debug form "
             "of unavailable")
    assert f"<li><code>parity/rrnewton/fixturehost/2026-09.jsonl:1</code>: t/b@kvm: {shown}</li>" in panel
    assert f'<td><code class="parity-message">{shown}</code></td></tr>' in panel
    assert ("<p>Legacy observations dropped rather than shown as verdicts: "
            "legacy verdict [retired word withheld] 2.</p>") in panel


def test_a_headline_brief_agrees_with_its_run(run_1953: dict[str, Any]) -> None:
    for field, value in (("emitted_at", "2026-09-28T22:00:00Z"), ("hermit_sha", "e" * 40),
                         ("line", "parity: something else")):
        document = copy.deepcopy(run_1953)
        stated = document["producers"][0][field]
        document["runs"][0][field] = value
        _refused(document, f"parity summary: validate run {RUN_ID} has {field} {stated!r} in producers "
                           f"but {value!r} in runs")


def test_a_summary_without_a_store_reports_nothing(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    document["store_present"] = False
    _refused(document, "parity summary: a summary without a parity store reports no rows or runs")


def test_a_headline_credit_below_one_never_prints_as_one() -> None:
    assert summary_module.credit_text(0.9995) == "0.999"
    assert summary_module.credit_text(0.99999) == "0.999"
    assert summary_module.credit_text(0.9994) == "0.999"
    assert summary_module.credit_text(1.0) == "1.000"
    assert summary_module.credit_text(0.0) == "0.000"
    assert summary_module.credit_text(None) == "n/a"
    # Three matches and one divergence at 999 of 1000: the mean is 0.99975,
    # which three decimals alone would print as full credit.
    document = _document([*(_cell(f"t/{n}", verdict="matched") for n in range(3)), _cell("t/d", credit=0.999)])
    run = document["producers"][0]
    assert f"{run['total']['mean_credit']:.3f}" == "1.000" and run["total"]["mean_credit"] < 1.0
    assert run["line"] == ("parity: 3/4 matched; mean 0.999 over 4 measured; floor 0.999 over 4 measured of "
                           "4 selected (0 no golden; 0 not compared)")


def test_duplicate_keys_and_non_finite_numbers_are_refused() -> None:
    with pytest.raises(summary_module.ParitySummaryError, match="parity summary repeats the key 'schema'"):
        summary_module.load_summary('{"schema": "parity-summary/v1", "schema": "x"}')
    with pytest.raises(summary_module.ParitySummaryError, match="parity summary contains the non-JSON constant NaN"):
        summary_module.load_summary('{"schema": NaN}')


# --- the carriage: bound to the store it summarizes -------------------------------


def _digest(store: dict[str, bytes]) -> str:
    return summary_module.store_digest(store)


def _refusal(run_id: str, emitted_at: str, error: str = "the source rows are malformed") -> dict[str, Any]:
    return ledger.build_refusal(
        producer="validate", run_id=run_id, hermit_sha=HERMIT_SHA, emitted_at=emitted_at,
        source_tree_dirty=False, host="fixturehost", input_bytes=run_id.encode(), input_lines=1, error=error,
    )


def _listed(event: dict[str, Any]) -> dict[str, Any]:
    return {field: event[field] for field in summary_module.APPEND_REFUSAL_FIELDS}


GROWN = {SHARD: STORE[SHARD] + b'{"line": 3}\n', "parity/hermit/otherhost/2026-09.jsonl": b'{"line": 4}\n{"line": 5}\n'}


def _earlier(run_1953: dict[str, Any], series_commit: str | None = EARLIER_COMMIT,
             store: dict[str, bytes] = STORE) -> dict[str, Any]:
    summary = copy.deepcopy(run_1953)
    summary["generated_from"].update(series_commit=series_commit, store_sha256=_digest(store))
    return summary


def _refused_by(carriage: dict[str, Any]) -> str:
    assert carriage["state"] == "refused" and carriage["summary_json"] is None, carriage
    assert summary_module.validate_carriage(carriage) is None
    prefix = f"ledger commit {LEDGER_COMMIT} scorecard/parity.json is refused: "
    assert carriage["refusal"].startswith(prefix), carriage["refusal"]
    return carriage["refusal"][len(prefix):]


def test_the_store_digest_is_hermits() -> None:
    # hermit 69feeee188e `read_parity_store`: per shard in byte order of its
    # path, the path, a NUL byte, the length as a little-endian u64, the bytes.
    shards = {"parity/b/h/2026-09.jsonl": b"x\n", "parity/a/h/2026-10.jsonl": b"yy\n"}
    expected = hashlib.sha256()
    for path in ("parity/a/h/2026-10.jsonl", "parity/b/h/2026-09.jsonl"):
        expected.update(path.encode() + b"\0" + len(shards[path]).to_bytes(8, "little") + shards[path])
    assert _digest(shards) == expected.hexdigest()
    assert summary_module.is_store_shard("parity/hermit/host/2026-09.jsonl")
    for path in ("parity/hermit/2026-09.jsonl", "parity/hermit/host/x/2026-09.jsonl",
                 "parity/hermit/host/2026-09.json", "parity-runs/hermit/host/2026-09.jsonl"):
        assert not summary_module.is_store_shard(path)
    assert [summary_module.record_lines(data) for data in (b"", b"a\n", b"a\nb", b"a\n\n")] == [0, 1, 2, 2]


def test_a_summary_of_the_store_at_the_ledger_commit_is_current(run_1953: dict[str, Any]) -> None:
    carriage = _capture(run_1953)
    assert (carriage["state"], carriage["refusal"], carriage["store_sha256"]) == ("current", None, _digest(STORE))
    assert summary_module.validate_carriage(carriage) == run_1953
    # The mirror commits the summary one commit after the series commit it
    # names; the store is unchanged there, so the summary is current.
    parent = _earlier(run_1953, series_commit="d" * 40)
    carriage = _capture(parent, ancestors=("d" * 40,))
    assert (carriage["state"], carriage["summarized_commit"]) == ("current", "d" * 40)


def test_a_summary_of_an_earlier_store_the_ledger_extends_is_stale(run_1953: dict[str, Any]) -> None:
    summary = _earlier(run_1953)
    carriage = _capture(summary, store=GROWN, earlier={EARLIER_COMMIT: STORE}, ancestors=(EARLIER_COMMIT,))
    assert (carriage["state"], carriage["summarized_commit"], carriage["store_records"],
            carriage["unsummarized_records"], carriage["store_sha256"]) == (
        "stale", EARLIER_COMMIT, 5, 3, _digest(GROWN))
    assert summary_module.validate_carriage(carriage) == summary
    text = _text(_panel(carriage))
    assert (f"Stale: this summary covers the parity store at ledger commit {EARLIER_COMMIT}; 3 newer parity "
            f"record line(s) at ledger commit {LEDGER_COMMIT} are not summarized. The figures below are as of "
            f"{EARLIER_COMMIT}.") in text
    assert ("parity store at ledger commit eeeeeeeeeeee (stale: 3 newer record line(s) at ledger commit "
            "bbbbbbbbbbbb not summarized) (scorecard/parity.json") in text
    # The current commit is never printed as the as-of point.
    assert "parity store at ledger commit bbbbbbbbbbbb" not in text


@pytest.mark.parametrize(
    ("name", "arguments", "reason"),
    [
        ("current, but its series commit is foreign", lambda r: (_earlier(r, "d" * 40), {}),
         f"its store_sha256 matches the parity/ store at ledger commit {LEDGER_COMMIT}, but its series_commit "
         f"{'d' * 40} is not that commit or an ancestor of it"),
        ("store moved, no series commit", lambda r: (_earlier(r, None), {"store": GROWN}),
         "{mismatch}, and it names no series_commit to locate what it summarized"),
        ("store moved, series commit is the ledger commit", lambda r: (r, {"store": GROWN}),
         f"{{mismatch}}, and its series_commit {LEDGER_COMMIT} is not an ancestor of it"),
        ("store moved, series commit not an ancestor", lambda r: (_earlier(r), {"store": GROWN}),
         f"{{mismatch}}, and its series_commit {EARLIER_COMMIT} is not an ancestor of it"),
        ("store moved, earlier store unreadable", lambda r: (_earlier(r), {"store": GROWN, "ancestors": (EARLIER_COMMIT,)}),
         f"no store fixture at {EARLIER_COMMIT}"),
        ("store moved, digest is not the earlier store's",
         lambda r: (_earlier(r), {"store": GROWN, "ancestors": (EARLIER_COMMIT,),
                                  "earlier": {EARLIER_COMMIT: {SHARD: b"other\n"}}}),
         f"{{mismatch}}, nor the digest of the store at its series_commit {EARLIER_COMMIT}"),
        ("store moved, an earlier shard is gone",
         lambda r: (_earlier(r, store={**STORE, "parity/hermit/gone/2026-08.jsonl": b"x\n"}),
                    {"store": GROWN, "ancestors": (EARLIER_COMMIT,),
                     "earlier": {EARLIER_COMMIT: {**STORE, "parity/hermit/gone/2026-08.jsonl": b"x\n"}}}),
         f"{{mismatch}}; shard parity/hermit/gone/2026-08.jsonl at its series_commit {EARLIER_COMMIT} is gone "
         f"at {LEDGER_COMMIT}"),
        ("store moved, a shard was rewritten",
         lambda r: (_earlier(r, store={SHARD: b'{"line": 9}\n'}),
                    {"store": GROWN, "ancestors": (EARLIER_COMMIT,), "earlier": {EARLIER_COMMIT: {SHARD: b'{"line": 9}\n'}}}),
         f"{{mismatch}}; shard {SHARD} at {LEDGER_COMMIT} does not extend its text at the series_commit "
         f"{EARLIER_COMMIT} (the ledger is append-only)"),
        ("no store at the ledger commit", lambda r: (r, {"store": {}, "store_files": 0}),
         f"the summary reports a parity/ store but ledger commit {LEDGER_COMMIT} has none"),
    ],
)
def test_each_way_a_summary_can_miss_its_store_is_refused(
    run_1953: dict[str, Any], name: str, arguments: Any, reason: str
) -> None:
    summary, options = arguments(run_1953)
    carriage = _capture(summary, **options)
    claimed = summary["generated_from"]["store_sha256"]
    actual = _digest(options.get("store", STORE))
    mismatch = (f"its store_sha256 {claimed} is not {actual}, the digest of the parity/ store at ledger "
                f"commit {LEDGER_COMMIT}")
    assert _refused_by(carriage) == reason.replace("{mismatch}", mismatch), name
    # A refused carriage still describes the bytes it refused.
    assert carriage["present"] is True and carriage["sha256"] == hashlib.sha256(
        summary_module.encode_summary(summary)).hexdigest()


def test_a_summary_that_is_not_a_summary_of_a_store_is_refused(run_1953: dict[str, Any]) -> None:
    assert _refused_by(_capture(None, data=b"\xff")) == (
        "scorecard/parity.json is not UTF-8: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte")
    with pytest.raises(summary_module.ParitySummaryError) as error:
        summary_module.load_summary("{}")
    assert _refused_by(_capture(None, data=b"{}")) == str(error.value)
    no_digest = copy.deepcopy(run_1953)
    no_digest["generated_from"]["store_sha256"] = None
    assert _refused_by(_capture(no_digest)) == "the summary reports a parity/ store but no store_sha256"
    storeless = summary_module.summarize_rows([], series_commit=LEDGER_COMMIT)
    storeless["store_present"] = False
    summary_module.validate_summary(storeless)
    assert _refused_by(_capture(storeless)) == (
        "the scorecard writes scorecard/parity.json only for a ledger with a parity/ store")


def _stale(run_1953: dict[str, Any]) -> dict[str, Any]:
    return _capture(_earlier(run_1953), store=GROWN, earlier={EARLIER_COMMIT: STORE}, ancestors=(EARLIER_COMMIT,))


def _absent() -> dict[str, Any]:
    return _capture(None, store={}, store_files=0, markers=1)


def _refused_carriage() -> dict[str, Any]:
    return summary_module.refused_carriage(LEDGER_COMMIT, "refused for the test")


def _with_summary(carriage: dict[str, Any], summary: dict[str, Any]) -> dict[str, Any]:
    data = summary_module.encode_summary(summary)
    return dict(carriage, summary_json=data.decode(), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())


_ITEM = {"producer": "validate", "run_id": "r", "hermit_sha": HERMIT_SHA, "emitted_at": EMITTED_AT, "error": "e"}
_AT = "parity carriage"


@pytest.mark.parametrize(
    ("base", "change", "message"),
    [
        ("current", lambda c, r: dict(c, path="x"), f"{_AT}.path must be scorecard/parity.json, got 'x'"),
        ("current", lambda c, r: dict(c, state="fresh"),
         f"{_AT}.state must be one of absent, current, stale, refused; got 'fresh'"),
        ("current", lambda c, r: dict(c, append_refusals={}), f"{_AT}.append_refusals must be a list of at most 20"),
        ("current", lambda c, r: dict(c, append_refusals=[_ITEM] * 21, append_refusals_total=21),
         f"{_AT}.append_refusals must be a list of at most 20"),
        ("current", lambda c, r: dict(c, append_refusals=[{"producer": "validate"}], append_refusals_total=1),
         f"{_AT}.append_refusals[0] is missing field(s): run_id, hermit_sha, emitted_at, error"),
        ("current", lambda c, r: dict(c, append_refusals=[dict(_ITEM, producer="x")], append_refusals_total=1),
         f"{_AT}.append_refusals[0].producer must be one of validate, pressure-test; got 'x'"),
        ("current", lambda c, r: dict(c, append_refusals=[dict(_ITEM, run_id="")], append_refusals_total=1),
         f"{_AT}.append_refusals[0].run_id must be a non-empty string, got ''"),
        ("current", lambda c, r: dict(c, append_refusals=[dict(_ITEM, hermit_sha=None)], append_refusals_total=1),
         f"{_AT}.append_refusals[0].hermit_sha must be set"),
        ("refused", lambda c, r: dict(c, append_refusals=[_ITEM], append_refusals_total=None),
         f"{_AT}: append refusals are listed but not counted"),
        ("current", lambda c, r: dict(c, append_refusals_total=None),
         f"{_AT}.append_refusals_total must be a non-negative integer, got None"),
        ("current", lambda c, r: dict(c, append_refusals=[_ITEM], append_refusals_total=0),
         f"{_AT}.append_refusals_total 0 is fewer than the 1 listed"),
        ("current", lambda c, r: dict(c, append_refusals=[_ITEM], append_refusals_total=2),
         f"{_AT}: 1 append refusal(s) listed of 2; up to 20 are listed"),
        ("current", lambda c, r: dict(c, store_sha256="A" * 64), f"{_AT}.store_sha256 must be 64 lowercase hex digits"),
        ("refused", lambda c, r: dict(c, refusal=""), f"{_AT}.refusal must be a non-empty string, got ''"),
        ("refused", lambda c, r: dict(c, ledger_commit="x"), f"{_AT}.ledger_commit must be 40 lowercase hex digits, got 'x'"),
        ("refused", lambda c, r: dict(c, summary_json="{}"), f"{_AT}: a refused carriage carries no summary to render"),
        ("current", lambda c, r: dict(c, refusal="no"), f"{_AT}: only a refused carriage carries a refusal"),
        ("current", lambda c, r: dict(c, ledger_commit=None), f"{_AT}.ledger_commit must be set"),
        ("current", lambda c, r: dict(c, store_sha256=None),
         f"{_AT}.store_sha256 must be set exactly when a parity/ store exists"),
        ("absent", lambda c, r: dict(c, store_records=3), f"{_AT}: 3 record(s) counted without a parity/ store"),
        ("absent", lambda c, r: dict(c, present=True), f"{_AT}: an absent summary is not present"),
        ("absent", lambda c, r: dict(c, summary_json="{}"), f"{_AT}: an absent summary has no summary_json"),
        ("current", lambda c, r: dict(c, present=False), f"{_AT}: a current summary must be present"),
        ("current", lambda c, r: dict(c, git_object="x"), f"{_AT}.git_object must be a lowercase 40- or 64-hex object id"),
        ("current", lambda c, r: dict(c, summary_json=None), f"{_AT}.summary_json must be a string"),
        ("current", lambda c, r: dict(c, bytes=c["bytes"] + 1),
         "{at}.bytes {plus} is not the {bytes} byte(s) carried"),
        ("current", lambda c, r: dict(c, sha256="0" * 64), f"{_AT}.sha256 disagrees with the bytes carried"),
        ("current", lambda c, r: _with_summary(c, dict(summary_module.summarize_rows([]), store_present=False)),
         f"{_AT}: the scorecard writes scorecard/parity.json only for a ledger with a parity/ store"),
        ("current", lambda c, r: dict(c, store_files=0, store_sha256=None, store_records=0),
         f"{_AT}: the summary reports a parity/ store but ledger commit {LEDGER_COMMIT} has no parity/ shard"),
        ("current", lambda c, r: dict(c, summarized_commit="d" * 40),
         f"{_AT}.summarized_commit '{'d' * 40}' is not the summary's series_commit '{LEDGER_COMMIT}'"),
        ("current", lambda c, r: dict(c, store_sha256=_digest(GROWN)),
         f"{_AT}: a current summary's store_sha256 '{_digest(STORE)}' must be the store's {_digest(GROWN)}"),
        ("current", lambda c, r: dict(c, unsummarized_records=1), f"{_AT}: a current summary leaves no record unsummarized"),
        ("current", lambda c, r: dict(c, unsummarized_records=None),
         f"{_AT}.unsummarized_records must be a non-negative integer, got None"),
        ("stale", lambda c, r: dict(c, store_sha256=_digest(STORE)),
         f"{_AT}: a stale summary's store_sha256 must differ from the store's"),
        ("current", lambda c, r: dict(c, state="stale", store_sha256=_digest(GROWN)),
         f"{_AT}: a stale summary names the earlier ledger commit it summarized"),
        ("stale", lambda c, r: dict(c, unsummarized_records=6),
         f"{_AT}: 6 unsummarized record(s) exceed the 5 in the store"),
    ],
)
def test_each_carriage_guard_refuses_its_own_tamper(
    run_1953: dict[str, Any], base: str, change: Any, message: str
) -> None:
    carriage = {"current": lambda: _carriage(run_1953), "stale": lambda: _stale(run_1953),
                "absent": _absent, "refused": _refused_carriage}[base]()
    assert summary_module.validate_carriage(copy.deepcopy(carriage)) is not None or base in ("absent", "refused")
    message = message.format(at=_AT, plus=(carriage["bytes"] or 0) + 1, bytes=carriage["bytes"])
    with pytest.raises(summary_module.ParitySummaryError) as error:
        summary_module.validate_carriage(change(copy.deepcopy(carriage), run_1953))
    assert str(error.value) == message


def test_admission_confines_a_refusal_to_a_refused_carriage(run_1953: dict[str, Any]) -> None:
    carriage = _carriage(run_1953)
    admitted, summary = summary_module.admit_carriage(carriage, LEDGER_COMMIT, "runs.parity")
    assert admitted == carriage and admitted is not carriage and summary == run_1953
    for value, series_commit, reason in (
        (dict(carriage, sha256="0" * 64), LEDGER_COMMIT, f"{_AT}.sha256 disagrees with the bytes carried"),
        (carriage, None, "there is no series ledger commit to read it at"),
        (carriage, "f" * 40, f"it was read at ledger commit {LEDGER_COMMIT}, not the series commit {'f' * 40}"),
    ):
        refused, summary = summary_module.admit_carriage(value, series_commit, "runs.parity")
        assert summary is None and summary_module.validate_carriage(refused) is None
        assert (refused["state"], refused["ledger_commit"], refused["refusal"]) == (
            "refused", series_commit, f"runs.parity is refused: {reason}")


def test_a_tampered_carriage_is_refused_in_the_panel_alone(run_1953: dict[str, Any]) -> None:
    carriage = _carriage(run_1953)
    carriage["sha256"] = "0" * 64
    text = _text(_panel(carriage))
    assert "Parity summary refused; no parity figure is shown for this build. The determinism figures are unaffected." in text
    assert f"parity carriage is refused: {_AT}.sha256 disagrees with the bytes carried" in text
    assert RUN_1953_LINE not in text and "0/" not in text


def test_a_refused_capture_says_what_it_read(run_1953: dict[str, Any]) -> None:
    carriage = _capture(run_1953, store=GROWN)
    text = _text(_panel(carriage))
    assert f"its store_sha256 {_digest(STORE)} is not {_digest(GROWN)}" in text
    assert (f"Read at: ledger commit {LEDGER_COMMIT}; 2 parity record shard file(s); 5 record line(s); "
            "scorecard/parity.json sha256 ") in text


def test_append_refusals_without_a_later_marker_are_listed_newest_first() -> None:
    superseded = _refusal("run-a", "2026-09-28T01:00:00Z")
    older = _refusal("run-b", "2026-09-28T02:00:00Z")
    newer = _refusal("run-c", "2026-09-28T03:00:00Z")
    marker = {"event_type": "parity.run", "producer": "validate", "run_id": "run-a"}
    unsigned = dict(_refusal("run-d", "2026-09-28T04:00:00Z"), event_id="0" * 64)
    shards = {
        "parity-runs/hermit/fixturehost/2026-09.jsonl": b"".join(
            json.dumps(event).encode() + b"\n" for event in (superseded, marker, older, newer, unsigned)
        ) + b"not json\n",
        "parity-runs/hermit/otherhost/2026-09.jsonl": json.dumps(newer).encode() + b"\n",
    }
    assert summary_module.append_refusals_from(shards) == ([_listed(newer), _listed(older)], 2)
    many = {"parity-runs/hermit/h/2026-09.jsonl": b"".join(
        json.dumps(_refusal(f"run-{index:02}", f"2026-09-28T00:00:{index:02}Z")).encode() + b"\n" for index in range(25))}
    listed, total = summary_module.append_refusals_from(many)
    assert total == 25 and [item["run_id"] for item in listed] == [f"run-{index:02}" for index in range(24, 4, -1)]


def test_append_refusals_are_shown_in_every_state(run_1953: dict[str, Any]) -> None:
    events = [_refusal("run-c", "2026-09-28T03:00:00Z", "no parity node reported a status")]
    for carriage in (_capture(run_1953, append=[_listed(e) for e in events]),
                     _capture(None, store={}, store_files=0, append=[_listed(e) for e in events])):
        text = _text(_panel(carriage))
        assert ("1 parity run(s) recorded no parity rows: append-parity refused their records and they were not "
                "later recorded (1 newest listed). These runs are not in any figure below.") in text
        assert (f"validate run run-c at Hermit {HERMIT_SHA[:12]}, 2026-09-28T03:00:00Z: "
                "no parity node reported a status") in text


# --- absence is never zero ----------------------------------------------------------


def test_each_absent_state_says_what_is_missing() -> None:
    no_capture = _text(_panel(None))
    assert summary_module.ABSENT_LINE in no_capture and "carries no parity capture" in no_capture
    empty = _text(_panel(_capture(None, store={}, store_files=0, markers=1)))
    assert summary_module.ABSENT_LINE in empty
    assert f"Ledger commit {LEDGER_COMMIT} holds no parity records (1 parity run marker file(s))" in empty
    unsummarized = _text(_panel(_capture(None, store=GROWN, store_files=3, markers=2)))
    assert ("holds 3 parity record shard file(s) and 2 run marker file(s), but no scorecard/parity.json: "
            "its parity records were published and never summarized.") in unsummarized
    for text in (no_capture, empty, unsummarized):
        assert "Nothing here is zero parity." in text and "0/" not in text


def test_record_missing_source_survives_into_the_panel() -> None:
    # The shape `parity::ledger_sources` emits for a node that left no status
    # (the same source test_parity_ledger.py transports).
    missing = {
        "cell": "backend-parity-c/never-ran@kvm", "test_id": "backend-parity-c/never-ran", "backend": "kvm",
        "verdict": "record-missing", "unavailable_class": "record-missing", "operand": None,
        "reason": "the parity post-pass of privileged/manifest_backend_parity_c left no parity.status.json",
        "run_id": None, "hermit_sha": None,
        "source": {"lane": "privileged", "node": "manifest_backend_parity_c", "post_pass_state": "absent",
                   "status_sha256": None, "records_sha256": None, "hermit_bin_sha256": None,
                   "scope_source": "expected-scope"},
        "record": None,
    }
    document = _summary(_rows([*_sources()[:3], missing]))
    run = document["producers"][0]
    total = run["total"]
    assert (total["selected"], total["record_missing"], total["credited"], total["floor_cells"]) == (4, 1, 3, 4)
    assert run["line"] == ("parity: 0/4 matched; mean 0.069 over 3 measured; floor 0.052 over 3 measured of "
                           "4 selected (0 no golden; 0 not compared; 1 record-missing) [inputs not equalized]")
    [cell] = [cell for cell in run["cells"] if cell["verdict"] == "record-missing"]
    assert cell["credit"] is None and cell["lane"] == "privileged"
    assert (cell["unavailable_class"], cell["operand"]) == ("record-missing", None)
    text = _text(_panel(_carriage(document)))
    assert ("backend-parity-c/never-ran@kvm | record-missing | record-missing | record-missing | none | "
            "none: 0 in the floor |") in text
    # kvm keeps the missing cell among its 3 selected: the mean is over its 2
    # measured cells, the floor over all 3.
    assert "kvm | 3 | 2 | 0 | 2 | 0 | 0 | 0 | 1 | 0 | 0.0963 | 0.0642 | 3 |" in text
    assert "TOTAL | 4 | 3 | 0 | 3 | 0 | 0 | 0 | 1 | 0 | 0.0691 | 0.0518 | 4 |" in text


# --- hermit's own scorecard writer (S12) ---------------------------------------------
#
# The Python reference above is written in the same change as the checker, so it
# cannot catch a format the two share by mistake. The fixture is a summary hermit's
# Rust scorecard wrote at S12 commit 69feeee188e, with the rows it summarized,
# copied byte for byte from hermit (provenance.json names the commit and every
# file's digest).
#
# That commit predates the typed `unavailable_class` and `operand` fields, so this
# reader refuses its summary and its rows. The byte-for-byte comparison with the
# Rust writer needs the fixture re-vendored from the typed S12 commit; until then
# the tests below prove the refusal, and that the reference over the same rows,
# typed, agrees with S12 on everything the typed accounting did not change.

S12 = REPOSITORY / "ci-hub/tests/fixtures/parity-s12-69feeee188e"
S12_COMMIT = "69feeee188e8480c34645716b57bc87f3f857cc5"
S12_REFUSAL = f"parity summary.producers[0] (validate run {RUN_ID}).cells[0] is missing field(s): operand"


def _s12() -> tuple[bytes, bytes, dict[str, Any]]:
    provenance = json.loads((S12 / "provenance.json").read_text(encoding="utf-8"))
    assert provenance["hermit_commit"] == S12_COMMIT
    assert sorted(provenance["files"]) == [
        "expected-parity.json", "hermit-provenance.json", "retired-ids.json", "rows.jsonl"]
    for name, entry in provenance["files"].items():
        blob = (S12 / name).read_bytes()
        assert (len(blob), hashlib.sha256(blob).hexdigest(),
                hashlib.sha1(b"blob %d\0" % len(blob) + blob).hexdigest()) == (
            entry["bytes"], entry["sha256"], entry["git_blob"]), name
    hermit_provenance = json.loads((S12 / "hermit-provenance.json").read_text(encoding="utf-8"))
    rows = (S12 / "rows.jsonl").read_bytes()
    assert hermit_provenance["rows_jsonl_sha256"] == hashlib.sha256(rows).hexdigest()
    return (S12 / "expected-parity.json").read_bytes(), rows, provenance


def _s12_capture(data: bytes, rows: bytes, provenance: dict[str, Any]) -> dict[str, Any]:
    store = {provenance["store_shard"]: rows}
    blob = hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()
    return summary_module.capture_carriage(
        LEDGER_COMMIT, summary_object=blob, summary_data=data, store=store, store_files=1,
        run_marker_files=1, read_store_at=_no_earlier_store, is_ancestor=lambda commit: False,
    )


def _s12_headline(data: bytes) -> str:
    hermit_provenance = json.loads((S12 / "hermit-provenance.json").read_text(encoding="utf-8"))
    [run] = json.loads(data)["runs"]
    assert run["line"] == hermit_provenance["expected_summary"]
    return run["line"]


def test_hermit_s12_predates_the_typed_classes_and_is_refused() -> None:
    data, rows, provenance = _s12()
    assert len(data) == 217963
    with pytest.raises(summary_module.ParitySummaryError) as error:
        summary_module.validate_summary(json.loads(data))
    assert str(error.value) == S12_REFUSAL
    # Every row lacks the typed fields, so none is a row this ledger publishes.
    events = ledger.parse_lines(rows.decode(), "S12 rows.jsonl")
    assert len(events) == 192
    for event in events:
        with pytest.raises(ledger.ParityLedgerError) as error:
            ledger.validate_event(event)
        assert str(error.value) == (
            "parity ledger row fields are not exactly its schema's: missing unavailable_class, operand")
    # The store digest still agrees: the refusal is the schema, not the binding.
    digest = summary_module.store_digest({provenance["store_shard"]: rows})
    assert digest == json.loads(data)["generated_from"]["store_sha256"] == provenance["store_sha256"]
    assert digest == "d2750e52eb70b0bf984f3b4798f05aa32e413261bf3a127f90f8f4de46cbc9b3"


def test_hermit_s12_panel_shows_the_refusal_and_no_figure() -> None:
    data, rows, provenance = _s12()
    carriage = _s12_capture(data, rows, provenance)
    assert _refused_by(carriage) == S12_REFUSAL
    text = _text(_panel(carriage))
    assert ("Parity summary refused; no parity figure is shown for this build. The determinism figures are "
            "unaffected.") in text
    assert S12_REFUSAL in text
    assert _s12_headline(data) not in text and "0/" not in text


def _catalogue_renamed(summary: dict[str, Any]) -> tuple[dict[str, Any], int]:
    """`summary` with the catalogue renames hermit's scorecard applies.

    The scorecard maps every emitted test id through hermit's retired-ids.json
    (vendored from the same commit), reports the live id and keeps the emitted
    cell as `emitted_cell`; cells stay ordered by their reported id.
    """
    [retirement] = json.loads((S12 / "retired-ids.json").read_text(encoding="utf-8"))["retirements"]
    ids = retirement["ids"]
    renamed = copy.deepcopy(summary)
    count = 0

    def current(cell: str) -> str:
        test_id, backend = cell.rsplit("@", 1)
        return f"{ids[test_id]}@{backend}"

    for producer in renamed["producers"]:
        for cell in producer["cells"]:
            assert cell["emitted_cell"] is None
            cell["emitted_cell"] = cell["cell"]
            cell["cell"], cell["test_id"] = current(cell["cell"]), ids[cell["test_id"]]
            count += 1
        producer["cells"].sort(key=lambda cell: (cell["test_id"].encode(), cell["backend"]))
        for tally in [producer["total"], *producer["per_backend"].values()]:
            if tally["first_divergence"] is not None:
                tally["first_divergence"]["example_cell"] = current(tally["first_divergence"]["example_cell"])
    return renamed, count


def _typed(value: dict[str, Any], unavailable_class: str | None, operand: str | None) -> dict[str, Any]:
    typed = {}
    for key, item in value.items():
        typed[key] = item
        if key == "verdict":
            typed.update(unavailable_class=unavailable_class, operand=operand)
    return typed


def _typed_s12_rows(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """S12's rows with the typed class and operand their untyped verdicts imply.

    Besides measured cells RUN 1953 has two shapes: dbt's declared inputs not
    equalized, and a candidate cell with no result row. Any other shape fails.
    """
    rows = []
    for row in events:
        verdict, backend = row["verdict"], row["backend"]
        if verdict in ("matched", "diverged"):
            implied = (None, None)
        elif verdict == "inputs-not-equalized" and row["reason"] == ledger.INPUTS_NOT_EQUALIZABLE.get(backend):
            implied = ("inputs-not-equalized", "candidate")
        elif verdict == "candidate-missing" and row["reason"] == (
                f"the {backend} candidate verify cell of {row['test_id']} has no result row in this run"):
            implied = ("no-result-row", "candidate")
        else:
            raise AssertionError(f"no typed class for {row['cell']}: {verdict} {row['reason']!r}")
        typed = _typed(row, *implied)
        if typed["record"] is not None:
            typed["record"] = _typed(typed["record"], *implied)
        rows.append(dict(ledger.validate_row(typed)))
    return rows


def test_the_reference_over_typed_s12_rows_agrees_with_s12_but_for_the_typed_accounting() -> None:
    data, rows, provenance = _s12()
    theirs = json.loads(data)
    digest = summary_module.store_digest({provenance["store_shard"]: rows})
    typed = _typed_s12_rows(ledger.parse_lines(rows.decode(), "S12 rows.jsonl"))
    mine, count = _catalogue_renamed(summary_module.summarize_rows(typed, store_sha256=digest))
    assert count == 192
    summary_module.validate_summary(mine)
    [run], [s12] = mine["producers"], theirs["producers"]
    assert run["line"] == RUN_1953_LINE
    # Every cell agrees but for its class, which is now the typed one; S12 had
    # no operand.
    assert len(run["cells"]) == len(s12["cells"]) == 192
    classes = set()
    for ours, hermit in zip(run["cells"], s12["cells"]):
        assert "operand" not in hermit
        assert {key: value for key, value in ours.items() if key not in ("unavailable_class", "operand")} == {
            key: value for key, value in hermit.items() if key != "unavailable_class"}
        classes.add((hermit["unavailable_class"], ours["unavailable_class"], ours["operand"]))
    assert classes == {(None, None, None), ("inputs-not-equalized", "inputs-not-equalized", "candidate"),
                       ("candidate-missing", "no-result-row", "candidate")}
    # Every tally agrees on what was measured and credited. S12's one
    # "unavailable" count is split three ways, and its floor divided by every
    # selected cell, dbt's 14 not-compared ones too.
    for label in ("total", *summary_module.BACKENDS):
        ours = run["total"] if label == "total" else run["per_backend"][label]
        hermit = s12["total"] if label == "total" else s12["per_backend"][label]
        for field in ("selected", "measured", "matched", "diverged", "record_missing", "refused", "credited",
                      "clean_credited", "unequalized_credited", "credit_sum", "mean_credit", "first_divergence"):
            assert ours[field] == hermit[field], (label, field)
        assert hermit["unavailable"] == ours["no_golden"] + ours["not_compared"] + ours["unmeasured"], label
        if label in ("kvm", "liteinst", "sabre"):
            assert ours["floor_credit"] == hermit["floor_credit"], label
    assert s12["total"]["floor_credit"] == s12["total"]["credit_sum"] / 192
    assert run["total"]["floor_credit"] == run["total"]["credit_sum"] / 178
    assert (s12["per_backend"]["dbt"]["floor_credit"], run["per_backend"]["dbt"]["floor_credit"]) == (0.0, None)
    # Everything else is the same: the document around the producers, each
    # run's identity, and each brief but for its line.
    assert {key: value for key, value in mine.items() if key not in ("producers", "runs")} == {
        key: value for key, value in theirs.items() if key not in ("producers", "runs")}
    identity = ("line", "per_backend", "total", "cells")
    assert {key: value for key, value in run.items() if key not in identity} == {
        key: value for key, value in s12.items() if key not in identity}
    assert [dict(brief, line=None) for brief in mine["runs"]] == [dict(brief, line=None) for brief in theirs["runs"]]
