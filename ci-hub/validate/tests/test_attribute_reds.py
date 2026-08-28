#!/usr/bin/env python3
"""Bar for the durable red-attribution capture (attribute_reds --persist).

WHY: the run ledger's ``log_file`` is ephemeral /tmp state; a red row is
attributable only while its log survives. The capture must lift the VERBATIM
first_error_line out of a surviving log into a durable append-only record that
outlives the log — and must NEVER fabricate a line when the log is gone, and must
be idempotent so it can run on every landing without duplicating rows.
"""
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from attribute_reds import (  # noqa: E402
    attribute_row,
    persist_attributions,
    refill_attributions,
)


def _log(tmp_path: Path, name: str, node: str, *body: str) -> Path:
    lines = [f"[{node}] {b}" for b in body]
    lines.append(f"[{node}] ✗ FAIL   {node} (exit 101)")
    p = tmp_path / name
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return p


def _red_row(commit: str, finished_at: str, log_file: str | None) -> dict:
    return {
        "commit": commit,
        "finished_at": finished_at,
        "result": "fail",
        "exit_code": 1,
        "gates": [{"name": "portable CI DAG manifest", "result": "fail"}],
        "log_file": log_file,
    }


def _recorded_class(node: str = "build.runtime_release") -> dict:
    return {
        "node": node,
        "group": node.split(".", 1)[0],
        "sub_step_class": "dependency-build",
        "fault_class": "infrastructure",
        "infra_signature": "corrupt archive",
        "host_env_signature": None,
        "first_error_line": "archive member is not an object",
        "known_flaky": False,
    }


def _persist(rows, out: Path):
    records = [attribute_row(r) for r in rows]
    return records, persist_attributions(records, out)


def test_persist_captures_verbatim_line_and_survives_the_log(tmp_path):
    log = _log(
        tmp_path,
        "u7wd6E.log",
        "build.runtime_release",
        "scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::op_infile[abi:cxx11]'",
    )
    out = tmp_path / "attr.jsonl"
    _, (appended, skipped) = _persist(
        [_red_row("deadbeef", "2026-08-04T19:31:00Z", str(log))], out
    )
    assert (appended, skipped) == (1, 0)

    # Delete the log: the durable record must still carry the verbatim line.
    log.unlink()
    [rec] = [json.loads(l) for l in out.read_text().splitlines() if l.strip()]
    assert rec["node"] == "build.runtime_release"
    assert rec["fault_class"] == "infrastructure"
    assert rec["first_error_line"] == (
        "scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::op_infile[abi:cxx11]'"
    )
    assert rec["commit"] == "deadbeef"


def test_persist_separates_two_faults_under_one_gate_name(tmp_path):
    dynamorio = _log(
        tmp_path, "a.log", "build.runtime_release",
        "undefined reference to `dynamorio::drmemtrace::op_infile[abi:cxx11]'",
    )
    lockfile = _log(
        tmp_path, "b.log", "build.runtime_release",
        "error: cannot update the lock file "
        "liteinst-runtime-build/Cargo.lock because --locked was passed",
    )
    out = tmp_path / "attr.jsonl"
    _persist(
        [
            _red_row("aaaa1111", "2026-08-04T19:31:00Z", str(dynamorio)),
            _red_row("bbbb2222", "2026-08-04T19:32:00Z", str(lockfile)),
        ],
        out,
    )
    recs = {
        json.loads(l)["commit"]: json.loads(l)
        for l in out.read_text().splitlines()
        if l.strip()
    }
    # Same gate name, same failing node — separated by the verbatim line alone.
    assert "undefined reference to" in recs["aaaa1111"]["first_error_line"]
    assert "--locked was passed" in recs["bbbb2222"]["first_error_line"]
    assert recs["aaaa1111"]["fault_class"] == "infrastructure"
    assert recs["bbbb2222"]["fault_class"] == "code"


def test_persist_is_idempotent_across_reruns(tmp_path):
    log = _log(tmp_path, "c.log", "build.runtime_release", "error: boom")
    out = tmp_path / "attr.jsonl"
    rows = [_red_row("cafe", "2026-08-04T20:00:00Z", str(log))]

    _, first = _persist(rows, out)
    _, second = _persist(rows, out)
    assert first == (1, 0)
    assert second == (0, 1)  # already present -> nothing re-appended
    lines = [l for l in out.read_text().splitlines() if l.strip()]
    assert len(lines) == 1


def test_persist_never_fabricates_when_log_is_missing(tmp_path):
    out = tmp_path / "attr.jsonl"
    _, (appended, skipped) = _persist(
        [_red_row("dead", "2026-08-04T20:00:00Z", str(tmp_path / "gone.log"))], out
    )
    assert (appended, skipped) == (0, 0)
    assert not out.exists()


def test_recorded_classes_survive_a_reaped_log(tmp_path):
    row = _red_row("feedface", "2026-08-04T20:01:00Z", str(tmp_path / "gone.log"))
    row["failed_substep_classes"] = [_recorded_class()]
    record = attribute_row(row)
    assert record["log_status"] == "missing"
    assert record["classes"] == row["failed_substep_classes"]


def test_typed_gate_class_survives_a_reaped_log(tmp_path):
    row = _red_row("feedface", "2026-08-04T20:01:30Z", str(tmp_path / "gone.log"))
    row["gates"] = [
        {
            "name": "build.runtime_release",
            "result": "fail",
            "failure_class": "understood_infrastructure_failure",
            "failure_detail": "malformed archive",
        }
    ]
    record = attribute_row(row)
    assert record["log_status"] == "missing"
    [classification] = record["classes"]
    assert classification["node"] == "build.runtime_release"
    assert classification["failure_class"] == "understood_infrastructure_failure"
    assert classification["fault_class"] == "infrastructure"
    assert classification["infra_signature"] == "malformed archive"
    assert classification["first_error_line"] is None


def test_recorded_classes_win_over_a_surviving_log(tmp_path):
    log = _log(tmp_path, "other.log", "test.from-log", "error: different")
    row = _red_row("feedface", "2026-08-04T20:02:00Z", str(log))
    row["failed_substep_classes"] = [_recorded_class("build.from-row")]
    record = attribute_row(row)
    assert record["log_status"] == "present"
    assert [item["node"] for item in record["classes"]] == ["build.from-row"]


def test_recorded_empty_classes_do_not_reconstruct_from_a_log(tmp_path):
    log = _log(tmp_path, "present.log", "test.from-log", "error: different")
    row = _red_row("feedface", "2026-08-04T20:02:30Z", str(log))
    row["failed_substep_classes"] = []
    record = attribute_row(row)
    assert record["log_status"] == "present"
    assert record["classes"] == []


@pytest.mark.parametrize(
    ("value", "message"),
    [
        ({}, "failed_substep_classes must be a list"),
        ([{"node": "test.cli"}], r"failed_substep_classes\[0\] has inexact fields"),
        (
            [{**_recorded_class(), "fault_class": "future"}],
            r"failed_substep_classes\[0\]\.fault_class has unknown value",
        ),
        (
            [{**_recorded_class(), "timing": {}}],
            r"failed_substep_classes\[0\]\.timing has inexact fields",
        ),
    ],
)
def test_malformed_recorded_classes_refuse_by_name(tmp_path, value, message):
    row = _red_row("feedface", "2026-08-04T20:03:00Z", str(tmp_path / "gone.log"))
    row["failed_substep_classes"] = value
    with pytest.raises(ValueError, match=message):
        attribute_row(row)


def _null_record(commit: str, finished_at: str, node: str, log_file: str) -> str:
    """A record as an OLDER extractor would have persisted it: no first_error_line."""
    return json.dumps(
        {
            "commit": commit,
            "finished_at": finished_at,
            "log_file": log_file,
            "node": node,
            "sub_step_class": None,
            "fault_class": None,
            "infra_signature": None,
            "first_error_line": None,
        },
        separators=(",", ":"),
        sort_keys=True,
    )


def test_refill_backfills_null_line_when_the_log_survives(tmp_path):
    log = _log(tmp_path, "r.log", "e2e.metadata", "some noise")
    out = tmp_path / "attr.jsonl"
    out.write_text(
        _null_record("f00d", "2026-08-04T21:00:00Z", "e2e.metadata", str(log)) + "\n",
        encoding="utf-8",
    )
    refilled, still_null, evicted = refill_attributions(out)
    assert (refilled, still_null, evicted) == (1, 0, 0)
    [rec] = [json.loads(l) for l in out.read_text().splitlines() if l.strip()]
    # The harness terminal verdict the hardened extractor now surfaces.
    assert rec["first_error_line"] == "✗ FAIL   e2e.metadata (exit 101)"
    assert rec["node"] == "e2e.metadata"


def test_refill_leaves_evicted_log_records_untouched(tmp_path):
    out = tmp_path / "attr.jsonl"
    original = _null_record(
        "dead", "2026-08-04T21:00:00Z", "e2e.metadata", str(tmp_path / "gone.log")
    )
    out.write_text(original + "\n", encoding="utf-8")
    refilled, still_null, evicted = refill_attributions(out)
    assert (refilled, still_null, evicted) == (0, 0, 1)
    # Never fabricated; the record is byte-identical.
    assert out.read_text() == original + "\n"


def test_refill_does_not_touch_records_that_already_have_a_line(tmp_path):
    log = _log(tmp_path, "s.log", "build.runtime_release", "error: boom")
    out = tmp_path / "attr.jsonl"
    _persist([_red_row("cafe", "2026-08-04T20:00:00Z", str(log))], out)
    before = out.read_text()
    refilled, still_null, evicted = refill_attributions(out)
    assert (refilled, still_null, evicted) == (0, 0, 0)
    assert out.read_text() == before  # byte-identical, no rewrite churn


def test_refill_is_idempotent(tmp_path):
    log = _log(tmp_path, "t.log", "e2e.metadata", "noise")
    out = tmp_path / "attr.jsonl"
    out.write_text(
        _null_record("beef", "2026-08-04T21:00:00Z", "e2e.metadata", str(log)) + "\n",
        encoding="utf-8",
    )
    assert refill_attributions(out)[0] == 1
    assert refill_attributions(out) == (0, 0, 0)  # second pass finds nothing null


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))


def test_a_row_whose_log_was_reaped_still_yields_per_node_causes():
    """A red with no log must not degrade to a flat gate-name aggregate.

    Measured 2026-08-25: 854 of 856 non-pass rows name a /tmp log that no longer
    exists and 91% carry no per-node attribution at all, leaving only
    `failed_gates` -- which drops which node failed and how. The gates array is
    still in the row, so per-node records are recoverable without the log.
    """
    from attribute_reds import attribute_row
    row = {
        "commit": "deadbeef",
        "log_file": "/tmp/hermit-validate.GONE.log",
        "gates": [
            {"name": "portable CI DAG manifest", "result": "fail",
             "exit_code": 1, "real_seconds": 153},
            {"name": "Initialize repository submodules", "result": "pass"},
            {"name": "test.cli", "result": "fail", "exit_code": 101,
             "real_seconds": 12},
        ],
    }
    rec = attribute_row(row)
    assert rec["log_status"] == "missing"
    assert rec["classes"] == []
    causes = rec["gate_causes"]
    assert [c["node"] for c in causes] == ["portable CI DAG manifest", "test.cli"], (
        "one record per FAILED gate, passing gates excluded"
    )
    assert causes[1]["exit_code"] == 101 and causes[1]["real_seconds"] == 12
    assert [c["evidence"] for c in causes] == ["aggregate-gate", "gate-row"], (
        "an OUTER gate name is the aggregate this tool exists to get past; "
        "emitting it as a per-node cause would reintroduce the defect inside "
        "the fix. Only group.job ids are gate-row."
    )
    assert all("first_error_line" not in c for c in causes), (
        "no error line exists for a reaped log and inventing one is the "
        "failure this sidecar exists to avoid"
    )
