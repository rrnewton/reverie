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


def _summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    return summary_module.summarize_rows(
        rows, store_sha256="a" * 64, series_commit=LEDGER_COMMIT, tool_hermit_commit="c" * 40
    )


def _carriage(summary: dict[str, Any], store_files: int = 1, markers: int = 1) -> dict[str, Any]:
    data = summary_module.encode_summary(summary)
    blob = hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()
    return summary_module.present_carriage(LEDGER_COMMIT, blob, data, store_files, markers)


def _panel(carriage: dict[str, Any] | None) -> str:
    runs = {} if carriage is None else {"parity": carriage}
    return render_page._parity_panel(SimpleNamespace(runs=runs))


def _text(markup: str) -> str:
    markup = re.sub(r"</(p|tr|h2|h3|caption|li|summary)>", "\n", markup)
    markup = re.sub(r"</t[hd]>", " | ", markup)
    return re.sub(r"<[^>]+>", "", markup).replace("&#x27;", "'").replace("&quot;", '"').replace("&amp;", "&")


@pytest.fixture(scope="module")
def run_1953() -> dict[str, Any]:
    return _summary(_rows(_sources()))


def _cell(test_id: str, backend: str = "kvm", *, verdict: str = "diverged", credit: float | None = 0.25,
          basis: str | None = "clean") -> dict[str, Any]:
    """One parity-summary cell, the way `_cell_from_row` shapes it."""
    unavailable = verdict not in {"matched", "diverged", "record-missing", "refused"}
    return {
        "cell": f"{test_id}@{backend}", "emitted_cell": f"{test_id}@{backend}", "test_id": test_id,
        "backend": backend, "verdict": verdict,
        "unavailable_class": ledger.unavailable_class(verdict, "fixture") if unavailable else None,
        "reason": "fixture" if verdict not in {"matched", "diverged"} else None,
        "credit": credit, "credit_basis": basis if credit is not None else None,
        "matched_prefix": None, "left_len": None, "right_len": None,
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
    assert summary_module.validate_carriage(carriage) == run_1953
    assert summary_module.load_summary(carriage["summary_json"]) == run_1953


def test_run_1953_panel_quotes_the_line_and_the_data_as_of(run_1953: dict[str, Any]) -> None:
    text = _text(_panel(_carriage(run_1953)))
    assert "Parity (measured after determinism)" in text
    assert RUN_1953_LINE in text
    assert f"validate run {RUN_ID}" in text
    assert "Hermit commit d3a0a4ae3d16 measured" in text
    assert "ledger commit bbbbbbbbbbbb (scorecard/parity.json" in text
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
    leaderboard = render_page._leaderboard_parity_columns("kvm", document["producers"][0])
    assert "<strong>0 / 12</strong>" in leaderboard and "<strong>0.5000</strong>" in leaderboard


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


def test_duplicate_keys_and_non_finite_numbers_are_refused() -> None:
    with pytest.raises(summary_module.ParitySummaryError, match="parity summary repeats the key 'schema'"):
        summary_module.load_summary('{"schema": "parity-summary/v1", "schema": "x"}')
    with pytest.raises(summary_module.ParitySummaryError, match="parity summary contains the non-JSON constant NaN"):
        summary_module.load_summary('{"schema": NaN}')


def test_a_carriage_whose_bytes_disagree_is_refused(run_1953: dict[str, Any]) -> None:
    carriage = _carriage(run_1953)
    tampered = dict(carriage, sha256="0" * 64)
    with pytest.raises(summary_module.ParitySummaryError, match=r"\.sha256 disagrees with the bytes carried"):
        summary_module.validate_carriage(tampered)
    tampered = dict(carriage, bytes=carriage["bytes"] + 1)
    with pytest.raises(summary_module.ParitySummaryError,
                       match=rf"\.bytes {carriage['bytes'] + 1} is not the {carriage['bytes']} byte\(s\) carried"):
        summary_module.validate_carriage(tampered)
    tampered = dict(carriage, store_files=0)
    with pytest.raises(summary_module.ParitySummaryError,
                       match=f"reports a parity/ store but ledger commit {LEDGER_COMMIT} has no parity/ shard"):
        summary_module.validate_carriage(tampered)
    absent = summary_module.absent_carriage(LEDGER_COMMIT, 0, 0)
    with pytest.raises(summary_module.ParitySummaryError, match="an absent summary has no summary_json"):
        summary_module.validate_carriage(dict(absent, summary_json=carriage["summary_json"]))


def test_the_panel_refuses_a_tampered_carriage(run_1953: dict[str, Any]) -> None:
    carriage = _carriage(run_1953)
    carriage["sha256"] = "0" * 64
    with pytest.raises(render_page.RenderRefused, match="parity carriage is refused: .*sha256 disagrees"):
        _panel(carriage)


# --- absence is never zero ----------------------------------------------------------


def test_each_absent_state_says_what_is_missing() -> None:
    no_capture = _text(_panel(None))
    assert summary_module.ABSENT_LINE in no_capture and "carries no parity capture" in no_capture
    empty = _text(_panel(summary_module.absent_carriage(LEDGER_COMMIT, 0, 1)))
    assert summary_module.ABSENT_LINE in empty
    assert f"Ledger commit {LEDGER_COMMIT} holds no parity records (1 parity run marker file(s))" in empty
    unsummarized = _text(_panel(summary_module.absent_carriage(LEDGER_COMMIT, 3, 2)))
    assert ("holds 3 parity record shard file(s) and 2 run marker file(s), but no scorecard/parity.json: "
            "its parity records were published and never summarized.") in unsummarized
    for text in (no_capture, empty, unsummarized):
        assert "Nothing here is zero parity." in text and "0/" not in text
    board = render_page._leaderboard_parity_columns("kvm", None)
    assert board.count("no parity summary") == 2 and "0 / " not in board
    assert "ptrace is the parity reference" in render_page._leaderboard_parity_columns(
        "ptrace", {"per_backend": {}})


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
