"""The website's parity summary checker and parity panel.

The real-shaped data is RUN 1953, the first validate whose
`manifest_backend_parity_c` post-pass wrote parity records on both lanes
(`ci-hub/tests/fixtures/parity-run-1953`, provenance beside it). Its 192
sources are wrapped exactly as `series.py append-parity` wraps them, then
summarized by the Python reference of hermit's `summarize_parity`; the
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
RUN_1953_LINE = (
    "parity: 0/192 matched; measured 175; unavailable 17; record-missing 0; "
    "mean credit (measured) 0.052; floor credit (selected) 0.047 [inputs not equalized]"
)


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


def _sources() -> list[dict[str, Any]]:
    text = (FIXTURE / "ledger-sources.jsonl").read_text(encoding="utf-8")
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


def _cell(test_id: str, backend: str = "kvm", *, verdict: str = "diverged", credit: float | None = 0.25,
          basis: str | None = "clean") -> dict[str, Any]:
    """One parity-summary cell, the way `_cell_from_row` shapes it."""
    unavailable = verdict not in {"matched", "diverged", "record-missing", "refused"}
    prefix, left, right = _lengths(verdict, credit)
    return {
        "cell": f"{test_id}@{backend}", "emitted_cell": f"{test_id}@{backend}", "test_id": test_id,
        "backend": backend, "verdict": verdict,
        "unavailable_class": ledger.unavailable_class(verdict, "fixture") if unavailable else None,
        "reason": "fixture" if verdict not in {"matched", "diverged"} else None,
        "credit": credit, "credit_basis": basis if credit is not None else None,
        "matched_prefix": prefix, "left_len": left, "right_len": right,
        "first_divergent_record": 4 if verdict == "diverged" else None,
        "first_difference_field": "token 1: `a` vs `b`" if verdict == "diverged" else None,
        "first_difference_syscall": 7 if verdict == "diverged" else None,
        "reference_message": "left" if verdict == "diverged" else None,
        "candidate_message": "right" if verdict == "diverged" else None,
        "lane": None if verdict == "refused" else "portable", "node": None if verdict == "refused" else "manifest_backend_parity_c",
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


# --- RUN 1953: real-shaped records, end to end -------------------------------------


def test_run_1953_total_line_is_the_committed_figure(run_1953: dict[str, Any]) -> None:
    summary_module.validate_summary(run_1953)
    [run] = run_1953["producers"]
    assert run["run_id"] == RUN_ID and run["hermit_sha"] == HERMIT_SHA
    assert run["line"] == RUN_1953_LINE
    total = run["total"]
    assert (total["selected"], total["measured"], total["matched"], total["diverged"]) == (192, 175, 0, 175)
    assert (total["unavailable"], total["record_missing"], total["refused"], total["credited"]) == (17, 0, 0, 175)
    assert {k: v for k, v in total["unavailable_by_class"].items() if v} == {
        "inputs-not-equalized": 14, "candidate-missing": 3,
    }
    # The mean divides by the 175 credited cells and the floor by all 192.
    assert total["mean_credit"] == total["credit_sum"] / 175
    assert total["floor_credit"] == total["credit_sum"] / 192


def test_run_1953_per_backend_and_first_divergences(run_1953: dict[str, Any]) -> None:
    [run] = run_1953["producers"]
    lines = {backend: summary_module.render_line(run["per_backend"][backend]) for backend in summary_module.BACKENDS}
    assert lines == {
        "dbt": "parity: 0/14 matched; measured 0; unavailable 14; record-missing 0; "
               "mean credit (measured) n/a; floor credit (selected) 0.000",
        "kvm": "parity: 0/77 matched; measured 76; unavailable 1; record-missing 0; "
               "mean credit (measured) 0.101; floor credit (selected) 0.100 [inputs not equalized]",
        "liteinst": "parity: 0/99 matched; measured 98; unavailable 1; record-missing 0; "
                    "mean credit (measured) 0.014; floor credit (selected) 0.014 [inputs not equalized]",
        "sabre": "parity: 0/2 matched; measured 1; unavailable 1; record-missing 0; "
                 "mean credit (measured) 0.020; floor credit (selected) 0.010 [inputs not equalized]",
    }
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
    assert ("Mean credit 0.0518 is the credit sum 9.0726 over the 175 credited (measured) cell(s); "
            "floor credit 0.0473 is the same sum over all 192 selected cell(s). The 17 selected cell(s) "
            "without a credit") in text
    assert "kvm | 77 | 76 | 0 | 76 | 1 | 0 | 0 | 0.1010 | 76 | 0.0997 |" in text
    assert "dbt | 14 | 0 | 0 | 0 | 14 | 0 | 0 | n/a | 0 | 0.0000 |" in text
    assert "TOTAL | 192 | 175 | 0 | 175 | 17 | 0 | 0 | 0.0518 | 175 | 0.0473 |" in text
    assert "inputs-not-equalized | 0 | 0 | 0 | 14 | 14 |" in text
    assert "candidate-missing | 1 | 1 | 1 | 0 | 3 |" in text
    assert ("kvm | 13 | 2 | token 12: `Ok(93824992251904)` vs `Ok(2117632)` | 74 of 76 | "
            "backend-parity-c/aio-refusal@kvm |") in text
    assert "First divergence of each of the 175 diverged cell(s), and every other unmatched cell (192 in all)" in text
    assert "legacy-rerun history: the retired ptrace rerun, never part of the parity above" in text


# --- honest accounting --------------------------------------------------------------


def test_zero_of_twelve_renders_as_zero_of_twelve_with_its_floor() -> None:
    cells = [_cell(f"t/{n:02d}", credit=0.5) for n in range(12)]
    document = _document(cells)
    line = document["producers"][0]["line"]
    assert line == ("parity: 0/12 matched; measured 12; unavailable 0; record-missing 0; "
                    "mean credit (measured) 0.500; floor credit (selected) 0.500")
    text = _text(_panel(_carriage(document)))
    assert line in text
    assert "kvm | 12 | 12 | 0 | 12 | 0 | 0 | 0 | 0.5000 | 12 | 0.5000 |" in text


def test_record_missing_keeps_the_denominator_and_lowers_the_floor() -> None:
    cells = [_cell(f"t/{n:02d}", credit=0.5) for n in range(9)]
    cells += [_cell(f"t/{n:02d}", verdict="record-missing", credit=None) for n in range(9, 12)]
    document = _document(cells)
    total = document["producers"][0]["total"]
    assert (total["selected"], total["measured"], total["record_missing"], total["credited"]) == (12, 9, 3, 9)
    assert document["producers"][0]["line"] == (
        "parity: 0/12 matched; measured 9; unavailable 0; record-missing 3; "
        "mean credit (measured) 0.500; floor credit (selected) 0.375")
    text = _text(_panel(_carriage(document)))
    assert "t/11@kvm | record-missing | n/a |" in text
    assert "over the 9 credited (measured) cell(s); floor credit 0.3750 is the same sum over all 12 selected" in text


def test_null_credits_are_excluded_from_the_mean_never_averaged_as_zero() -> None:
    cells = [
        _cell("t/a", credit=0.5),
        _cell("t/b", verdict="candidate-missing", credit=None),
        _cell("t/c", verdict="record-missing", credit=None),
        _cell("t/d", verdict="refused", credit=None),
    ]
    result = summary_module.tally(cells)
    assert result["credited"] == 1 and result["selected"] == 4
    assert result["mean_credit"] == 0.5  # not 0.125: null is not 0
    assert result["floor_credit"] == 0.125
    assert summary_module.render_line(result) == (
        "parity: 0/4 matched; measured 1; unavailable 1; record-missing 1; refused 1; "
        "mean credit (measured) 0.500; floor credit (selected) 0.125"
    )
    text = _text(_panel(_carriage(_document(cells))))
    assert ("Mean credit 0.5000 is the credit sum 0.5000 over the 1 credited (measured) cell(s); "
            "floor credit 0.1250 is the same sum over all 4 selected cell(s). The 3 selected cell(s) "
            "without a credit (unavailable, record-missing or refused) are left out of the mean") in text
    nothing = summary_module.tally([_cell("t/e", verdict="candidate-missing", credit=None)])
    assert nothing["mean_credit"] is None and nothing["floor_credit"] == 0.0
    assert "mean credit (measured) n/a" in summary_module.render_line(nothing)


def test_a_mean_that_averages_nulls_as_zero_is_refused() -> None:
    cells = [_cell("t/a", credit=0.5), _cell("t/b", verdict="candidate-missing", credit=None)]
    document = _document(cells)
    run = document["producers"][0]
    run["total"]["mean_credit"] = run["total"]["credit_sum"] / run["total"]["selected"]
    with pytest.raises(summary_module.ParitySummaryError, match=r"mean_credit .* is not credit_sum / credited"):
        summary_module.validate_summary(document)


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
         r"\.total: mean_credit .* is not credit_sum / credited = "),
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

RUN_AT = f"parity summary.producers[0] (validate run {RUN_ID})"


def _refused(document: dict[str, Any], message: str) -> None:
    with pytest.raises(summary_module.ParitySummaryError, match=re.escape(message)):
        summary_module.validate_summary(document)


def _named(document: dict[str, Any], index: int) -> tuple[dict[str, Any], str]:
    cell = document["producers"][0]["cells"][index]
    return cell, f"{RUN_AT}.cells[{index}] ({cell['cell']})"


# RUN 1953's cells[0] is backend-parity-c/aio-refusal@kvm, diverged at 12 of
# 106 | 106; cells[14] and [160] are dbt inputs-not-equalized; cells[59] is
# the liteinst candidate-missing cell.


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
    cell.update(credit=0.5, credit_basis="unequalized")
    _refused(document, f"{at}: a candidate-missing cell carries credit 0.5; only a measured cell is credited")


def test_an_unavailable_class_is_the_one_its_verdict_and_reason_imply(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    cell, at = _named(document, 59)
    cell["unavailable_class"] = "other"
    _refused(document, f"{at}: unavailable class other is not the candidate-missing its verdict and reason imply")


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


def test_a_declared_backend_has_no_measured_cell_in_its_run(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    measured, _ = _named(document, 0)
    cell, _ = _named(document, 14)
    declaration = cell["reason"]
    for field in ("verdict", "reason", "unavailable_class", "credit", "credit_basis", "matched_prefix",
                  "left_len", "right_len", "first_divergent_record", "first_difference_field",
                  "first_difference_syscall", "reference_message", "candidate_message"):
        cell[field] = measured[field]
    _refused(document, f"{RUN_AT}: cell {cell['cell']}: dbt inputs cannot be equalized, so it reports no "
                       f"measured comparison and never equal inputs: {declaration}")


def test_a_run_declares_one_reason_per_backend(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    first, _ = _named(document, 14)
    cell, _ = _named(document, 160)
    cell["reason"] = "some other declaration"
    _refused(document, f"{RUN_AT}: cell {cell['cell']} declares dbt inputs not equalizable for "
                       f"'some other declaration', but another cell of the run declares {first['reason']!r}; "
                       "a run's hermit makes one declaration per backend")


def test_the_floor_is_the_credit_sum_over_every_selected_cell(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    kvm = document["producers"][0]["per_backend"]["kvm"]
    floor = kvm["floor_credit"]
    kvm["floor_credit"] = kvm["mean_credit"]
    _refused(document, f"{RUN_AT}.per_backend.kvm: floor_credit {kvm['mean_credit']!r} is not "
                       f"credit_sum / selected = {floor!r}")


def test_every_tally_field_is_recomputed_from_the_cells(run_1953: dict[str, Any]) -> None:
    document = copy.deepcopy(run_1953)
    total = document["producers"][0]["total"]
    stated = dict(total["unavailable_by_class"])
    total["unavailable_by_class"].update({"candidate-missing": 2, "other": 1})  # same sum, 17
    _refused(document, f"{RUN_AT}.total: unavailable_by_class {total['unavailable_by_class']!r} disagrees "
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
    kvm["mean_credit"] = kvm["credit_sum"] / kvm["credited"]
    kvm["floor_credit"] = kvm["credit_sum"] / kvm["selected"]
    _refused(document, f"{RUN_AT}.per_backend.kvm: credit_sum {kvm['credit_sum']!r} disagrees with its cells, "
                       f"which give {true_sum!r}")


def _with_one_refused_cell() -> dict[str, Any]:
    document = _document([_cell("t/a", credit=0.5), _cell("t/b", verdict="refused", credit=None)])
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
    document = _document([*(_cell(f"t/{n}", verdict="matched", credit=1.0) for n in range(3)),
                          _cell("t/d", credit=0.999)])
    run = document["producers"][0]
    assert f"{run['total']['mean_credit']:.3f}" == "1.000" and run["total"]["mean_credit"] < 1.0
    assert run["line"] == ("parity: 3/4 matched; measured 4; unavailable 0; record-missing 0; "
                           "mean credit (measured) 0.999; floor credit (selected) 0.999")


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
        "verdict": "record-missing",
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
    assert (total["selected"], total["record_missing"], total["credited"]) == (4, 1, 3)
    assert "; record-missing 1; " in run["line"]
    [cell] = [cell for cell in run["cells"] if cell["verdict"] == "record-missing"]
    assert cell["credit"] is None and cell["lane"] == "privileged"
    text = _text(_panel(_carriage(document)))
    assert "backend-parity-c/never-ran@kvm | record-missing | n/a |" in text
    # Selected 3 on kvm keeps the missing cell: mean over the 2 credited, floor over all 3.
    assert "kvm | 3 | 2 | 0 | 2 | 0 | 1 | 0 | 0.0963 | 2 | 0.0642 |" in text
    assert "TOTAL | 4 | 3 | 0 | 3 | 0 | 1 | 0 | 0.0691 | 3 | 0.0518 |" in text


# --- hermit's own scorecard writer (S12) ---------------------------------------------
#
# The Python reference above is written in the same change as the checker, so it
# cannot catch a format the two share by mistake. These tests read a summary
# hermit's Rust scorecard actually wrote, with the rows it summarized, copied byte
# for byte from hermit (provenance.json names the commit and every file's digest).

S12 = REPOSITORY / "ci-hub/tests/fixtures/parity-s12-69feeee188e"
S12_COMMIT = "69feeee188e8480c34645716b57bc87f3f857cc5"


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


def test_hermit_s12_summary_is_admitted_byte_for_byte() -> None:
    data, rows, provenance = _s12()
    assert len(data) == 217963
    document = json.loads(data)
    assert summary_module.validate_summary(document) == document
    summary = summary_module.load_summary(data.decode())
    # The reader's own encoding reproduces hermit's bytes, so field order, float
    # text and layout all agree with the Rust writer.
    assert summary_module.encode_summary(summary) == data
    # Every row is a parity-ledger/v1 record the dev-hermit writer would publish.
    events = ledger.parse_lines(rows.decode(), "S12 rows.jsonl")
    assert len(events) == summary["rows_read"] == 192
    assert all(ledger.validate_event(event)["event_type"] == "parity.record" for event in events)
    # The summary's store digest is the one this reader recomputes from that store.
    digest = summary_module.store_digest({provenance["store_shard"]: rows})
    assert digest == summary["generated_from"]["store_sha256"] == provenance["store_sha256"]
    assert digest == "d2750e52eb70b0bf984f3b4798f05aa32e413261bf3a127f90f8f4de46cbc9b3"
    carriage = _s12_capture(data, rows, provenance)
    assert carriage["state"] == "current", carriage["refusal"]
    assert (carriage["store_records"], carriage["unsummarized_records"]) == (192, 0)
    assert summary_module.admit_carriage(carriage, LEDGER_COMMIT, "S12 carriage") == (carriage, summary)


def _parity_table(markup: str) -> dict[str, list[str]]:
    table = re.search(r'<table class="parity-summary">.*?<tbody>(.*?)</tbody>', markup, re.S)
    assert table is not None
    rows = {}
    for row in re.findall(r"<tr>(.*?)</tr>", table.group(1), re.S):
        label = re.sub(r"<[^>]+>", "", re.search(r"<th[^>]*>(.*?)</th>", row, re.S).group(1))
        rows[label] = [re.sub(r"<[^>]+>", "", cell) for cell in re.findall(r"<td>(.*?)</td>", row, re.S)]
    return rows


def _credit_text(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.4f}"


def test_hermit_s12_panel_shows_the_tallies_hermit_wrote() -> None:
    data, rows, provenance = _s12()
    producer = json.loads(data)["producers"][0]
    markup = _panel(_s12_capture(data, rows, provenance))
    table = _parity_table(markup)
    expected = {backend: producer["per_backend"][backend] for backend in producer["per_backend"]}
    expected["TOTAL"] = producer["total"]
    assert sorted(table) == sorted(expected) == ["TOTAL", "dbt", "kvm", "liteinst", "sabre"]
    for label, tally in expected.items():
        assert table[label] == [
            str(tally["selected"]), str(tally["measured"]), str(tally["matched"]), str(tally["diverged"]),
            str(tally["unavailable"]), str(tally["record_missing"]), str(tally["refused"]),
            _credit_text(tally["mean_credit"]), str(tally["credited"]), _credit_text(tally["floor_credit"]),
        ], label
    text = _text(markup)
    assert producer["line"] in text and producer["line"] == _s12_headline(data)
    assert ("parity store at ledger commit bbbbbbbbbbbb (scorecard/parity.json, sha256 d139a53f1a86); "
            "summary written by Hermit tool commit not recorded.") in text
    assert "inputs-not-equalized | 0 | 0 | 0 | 14 | 14 |" in text and "candidate-missing | 1 | 1 | 1 | 0 | 3 |" in text
    assert "74 of 76 | c-programs/aio-refusal@kvm |" in text


def _s12_headline(data: bytes) -> str:
    hermit_provenance = json.loads((S12 / "hermit-provenance.json").read_text(encoding="utf-8"))
    [run] = json.loads(data)["runs"]
    assert run["line"] == hermit_provenance["expected_summary"]
    return run["line"]


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


def test_the_python_reference_matches_hermit_s12_apart_from_the_catalogue_renames() -> None:
    data, rows, provenance = _s12()
    theirs = json.loads(data)
    events = ledger.parse_lines(rows.decode(), "S12 rows.jsonl")
    mine = summary_module.summarize_rows(events, store_sha256=summary_module.store_digest(
        {provenance["store_shard"]: rows}))
    assert mine != theirs
    renamed, count = _catalogue_renamed(mine)
    assert count == 192
    assert renamed == theirs
