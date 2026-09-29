#!/usr/bin/env python3
"""Tests for the REAL green gate and the bounded scheduled job.

The atomicity engine is covered by test_auto_bump.py. These cover the wiring:
that the gate binds to the ledger at an exact SHA rather than to a launcher's
exit code, and that the scheduled job always leaves exactly one auditable
outcome no matter how it ends.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import auto_bump  # noqa: E402
import bump_job  # noqa: E402
import validation_gate as gate  # noqa: E402

OLD = "d" * 40
NEW = "0" * 39 + "a"
OTHER = "b" * 40
CAND = "c" * 40


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    r = tmp_path / "hermit"
    (r / "scripts").mkdir(parents=True)
    (r / "scripts" / "check-reverie-pin.rs").write_text("// canonical\n")
    (r / "Cargo.toml").write_text(
        f'[dependencies]\nreverie = {{ git = "{auto_bump.REVERIE_REMOTE}", rev = "{OLD}" }}\n'
        f'liteinst2 = {{ git = "https://github.com/rrnewton/liteinst2", rev = "{OTHER}" }}\n')
    (r / "Cargo.lock").write_text(
        f'[[package]]\nsource = "git+{auto_bump.REVERIE_REMOTE}?rev={OLD}#dddd"\n')
    _git(r, "init", "-q")
    _git(r, "add", "-A")
    _git(r, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init")
    return r


def snapshot(repo: Path) -> dict[str, bytes]:
    return {str(p.relative_to(repo)): p.read_bytes()
            for p in sorted(repo.rglob("*")) if p.is_file() and ".git" not in p.parts}


def receipt(commit: str, *, executed: int = 857, failures: int = 0,
            verdict: str = "VALIDATED", **fields) -> dict:
    """A `validate-status --json` payload shaped like the real one."""
    payload = {"verdict": verdict,
               "newest_qualifying": {"commit": commit, "executed_tests": executed,
                                     "failures": failures, "exit_code": 0},
               "qualifying_count": 1, "exit_code": 0 if verdict == "VALIDATED" else 4,
               "unresolved_failure_obligations": [], "unverifiable_evidence": []}
    payload.update(fields)
    return payload


# ---- the gate binds to the LEDGER, at an EXACT SHA -------------------------

def test_a_qualifying_receipt_at_the_exact_sha_is_green():
    v = gate.assess_ledger_verdict(receipt(CAND), CAND)
    assert v.green and v.executed_tests == 857


def test_a_receipt_for_a_DIFFERENT_commit_is_not_green():
    """The load-bearing check.

    `newest_qualifying` is the newest qualifying record, not necessarily one for
    the SHA asked about. Accepting it would green-light a bump on the strength
    of some other tree's validation.
    """
    v = gate.assess_ledger_verdict(receipt("f" * 40), CAND)
    assert not v.green
    assert "DIFFERENT commit" in v.reason


def test_no_receipt_is_not_green():
    v = gate.assess_ledger_verdict({"newest_qualifying": None}, CAND)
    assert not v.green and "absence of evidence" in v.reason


def test_a_receipt_with_zero_executed_tests_is_not_green():
    """`test result: ok` with nothing executed is a no-result."""
    v = gate.assess_ledger_verdict(receipt(CAND, executed=0), CAND)
    assert not v.green and "ZERO tests" in v.reason


def test_a_receipt_with_failures_is_not_green():
    v = gate.assess_ledger_verdict(receipt(CAND, failures=3), CAND)
    assert not v.green and "3 failure" in v.reason


FOREIGN_ENTRY = {"state": "foreign-host-artifact-absent", "run_id": "devbig030-1790000000-1",
                 "commit": "4c91b952" + "0" * 32, "producing_host": "devbig030"}
OBLIGATION = {"run_id": "devbig014-1790000000-2", "commit": OTHER, "node": "tests"}


def test_a_qualifying_receipt_without_the_validated_verdict_is_not_green():
    """https://github.com/rrnewton/dev-hermit/issues/395: the receipt is
    necessary, not sufficient. NEEDS-RERUN and FAILED-ON-RECORD can coexist
    with a perfect exact-head receipt."""
    for verdict, fields in (
        ("NEEDS-RERUN", {"unverifiable_evidence": [FOREIGN_ENTRY]}),
        ("NEEDS-RERUN", {"unresolved_failure_obligations": [OBLIGATION]}),
        ("FAILED-ON-RECORD", {}),
        ("LEDGER-UNREADABLE", {}),
    ):
        v = gate.assess_ledger_verdict(receipt(CAND, verdict=verdict, **fields), CAND)
        assert not v.green, verdict
        assert f"canonical verdict is '{verdict}'" in v.reason, v.reason


def test_a_payload_without_a_verdict_is_not_green():
    payload = receipt(CAND)
    del payload["verdict"]
    v = gate.assess_ledger_verdict(payload, CAND)
    assert not v.green and "canonical verdict is None" in v.reason


def test_a_validated_verdict_with_blocking_entries_is_refused_as_contradictory():
    for field, entry in (("unresolved_failure_obligations", OBLIGATION),
                         ("unverifiable_evidence", FOREIGN_ENTRY)):
        v = gate.assess_ledger_verdict(receipt(CAND, **{field: [entry]}), CAND)
        assert not v.green
        assert f"{field} lists 1 entry" in v.reason, v.reason


def test_a_missing_or_malformed_blocking_list_is_not_green():
    for field in ("unresolved_failure_obligations", "unverifiable_evidence"):
        payload = receipt(CAND)
        del payload[field]
        v = gate.assess_ledger_verdict(payload, CAND)
        assert not v.green and f"no list in '{field}'" in v.reason, v.reason
        v = gate.assess_ledger_verdict(receipt(CAND, **{field: {"n": 1}}), CAND)
        assert not v.green and f"no list in '{field}'" in v.reason, v.reason


def test_the_real_needs_rerun_payload_shape_reaches_the_gate_through_the_runner():
    """The subprocess path parses the JSON and applies the same verdict rule."""
    payload = receipt(CAND, verdict="NEEDS-RERUN", unverifiable_evidence=[FOREIGN_ENTRY])
    v = gate.ledger_verdict(CAND, ci_hub=Path("ci-hub"),
                            runner=lambda cmd: (4, json.dumps(payload)))
    assert not v.green and "NEEDS-RERUN" in v.reason


def test_a_non_json_answer_fails_closed():
    """An unparseable verdict is not a green one."""
    v = gate.ledger_verdict(CAND, ci_hub=Path("/nonexistent"),
                            runner=lambda cmd: (2, "boom: not json"))
    assert not v.green and "non-JSON" in v.reason


def test_a_short_sha_is_refused_without_even_querying():
    called: list[list[str]] = []
    v = gate.ledger_verdict("abc123", ci_hub=Path("/x"),
                            runner=lambda cmd: (called.append(cmd), (0, "{}"))[1])
    assert not v.green and called == [], "a floating ref must not reach the ledger query"


def test_the_gate_ignores_validate_run_exit_status(repo: Path):
    """validate-run reports FINISHED/rc=0 even when it ran NOTHING.

    So a green must survive the launcher claiming success while the ledger says
    there is no receipt.
    """
    calls: list[list[str]] = []

    def runner(cmd: list[str]) -> tuple[int, str]:
        calls.append(cmd)
        if "validate-run" in cmd:
            return 0, "FINISHED state=completed"        # the lie
        if "validate-status" in cmd:
            return 4, json.dumps({"newest_qualifying": None})
        if cmd[:2] == ["git", "-C"] and "rev-parse" in cmd:
            return 0, CAND + "\n"
        return 0, ""

    v = gate.real_validator(repo, ci_hub=Path("ci-hub"), runner=runner)
    assert v() is False, "a FINISHED/rc=0 launcher must not manufacture a green"
    assert any("validate-run" in c for c in calls)


# ---- the scheduled job -----------------------------------------------------

def test_a_validated_bump_records_source_target_and_cost(repo: Path, tmp_path: Path):
    log = tmp_path / "outcomes.jsonl"
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=log,
                            target=NEW, validate=lambda: True)
    assert out.outcome == "bumped-and-validated"
    assert out.source_sha == OLD and out.target_sha == NEW
    assert out.entries_before == out.entries_after == 2
    assert out.wall_seconds is not None and out.budget_seconds is not None
    records = [json.loads(l) for l in log.read_text().splitlines()]
    assert len(records) == 2, "one record before the attempt, one after"
    assert records[0]["outcome"] == "started"


def test_a_refused_bump_still_records_an_outcome_and_rolls_back(repo: Path, tmp_path: Path):
    """'The job ran and refused' must never look like 'the job never ran'."""
    before = snapshot(repo)
    log = tmp_path / "outcomes.jsonl"
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=log,
                            target=NEW, validate=lambda: False)
    assert out.outcome == "refused"
    assert "validation FAILED" in out.detail
    assert snapshot(repo) == before, "a refused scheduled run leaves the tree byte-identical"
    assert log.read_text().count("\n") == 2


def test_an_already_current_pin_is_a_recorded_noop(repo: Path, tmp_path: Path):
    log = tmp_path / "outcomes.jsonl"
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=log,
                            target=OLD, validate=lambda: True)
    assert out.outcome == "noop-already-current"
    assert out.files_changed == 0
    assert out.source_sha == out.target_sha == OLD


def test_a_preexisting_inconsistency_is_refused_rather_than_masked(repo: Path, tmp_path: Path):
    """Bumping an already-split tree would hide the split behind a fresh pin."""
    (repo / "Cargo.lock").write_text(
        f'[[package]]\nsource = "git+{auto_bump.REVERIE_REMOTE}?rev={OTHER}#dddd"\n')
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=tmp_path / "o.jsonl",
                            target=NEW, validate=lambda: True)
    assert out.outcome == "refused-preexisting-inconsistency"
    assert out.source_sha is None, "a split tree has no single source pin"


def test_the_budget_is_enforced_and_typed(repo: Path, tmp_path: Path):
    """The job returns under its own power rather than being killed."""
    ticks = iter([0.0, 9999.0, 9999.0, 9999.0])
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=tmp_path / "o.jsonl",
                            target=NEW, validate=lambda: True,
                            budget_seconds=10.0, clock=lambda: next(ticks))
    assert out.outcome == "budget-exceeded"


def test_an_unexpected_exception_is_recorded_not_swallowed(repo: Path, tmp_path: Path):
    def boom() -> bool:
        raise RuntimeError("validator harness died")

    log = tmp_path / "o.jsonl"
    out = bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=log, target=NEW, validate=boom)
    assert out.outcome == "refused"          # auto_bump converts it, fail-closed
    records = [json.loads(l) for l in log.read_text().splitlines()]
    assert records[-1]["outcome"] == "refused"


def test_only_a_genuine_error_makes_the_unit_fail(repo: Path, tmp_path: Path, monkeypatch):
    """A refusal is the safety property working; it must not turn the timer red."""
    log = tmp_path / "o.jsonl"
    rc_refused = bump_job.main(["--repo", str(repo), "--outcome-log", str(log),
                                "--target", NEW, "--ci-hub", "/nonexistent-cihub"])
    assert rc_refused == 0, "a refusal must not page anyone"
    last = json.loads(log.read_text().splitlines()[-1])
    assert last["outcome"] in ("refused", "noop-already-current", "error")


def test_the_liteinst_pin_survives_a_scheduled_bump(repo: Path, tmp_path: Path):
    bump_job.run_once(repo, ci_hub=Path("x"), outcome_log=tmp_path / "o.jsonl",
                      target=NEW, validate=lambda: True)
    text = (repo / "Cargo.toml").read_text()
    assert OTHER in text and f'liteinst2 = {{ git = "https://github.com/rrnewton/liteinst2", rev = "{OTHER}"' in text


# ---- the candidate commit must not survive a refusal ------------------------

def test_a_refused_candidate_commit_is_undone_and_head_restored(repo: Path):
    """FOUND BY RUNNING IT FOR REAL, not by these fixtures.

    auto_bump's rollback restores FILES, but the gate had already committed
    them, so a refusal left HEAD on the candidate with the tree reverted under
    it — ten dirty paths and a history the next run would stack onto.
    """
    head_before = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                 capture_output=True, text=True).stdout.strip()

    def runner(cmd: list[str]) -> tuple[int, str]:
        if "validate-status" in cmd:
            return 4, json.dumps({"newest_qualifying": None})
        proc = subprocess.run(cmd, capture_output=True, text=True)
        return proc.returncode, proc.stdout

    g = gate.real_validator(repo, ci_hub=Path("x"), launch=False, runner=runner)
    with pytest.raises(auto_bump.BumpRefused):
        auto_bump.auto_safe_bump(repo, target=NEW, validate=g)

    head_after = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "-C", str(repo), "status", "--porcelain"],
                           capture_output=True, text=True).stdout.strip()
    assert head_after == head_before, "the candidate commit outlived its refusal"
    assert dirty == "", "a refused run must leave the checkout clean, not reverse-dirty"
    assert g.state.get("candidate_undone") is True


def test_the_undo_refuses_when_the_tip_is_no_longer_ours(repo: Path):
    """The reset is guarded by a bound proxy, never by HEAD~1.

    If another commit landed on top, the tip is not the candidate and resetting
    would delete someone else's work — so it must decline instead.
    """
    def runner(cmd: list[str]) -> tuple[int, str]:
        if "validate-status" in cmd:
            # Someone else commits between our commit and the verdict.
            (repo / "OTHER.md").write_text("another agent's work\n")
            subprocess.run(["git", "-C", str(repo), "add", "OTHER.md"], capture_output=True)
            subprocess.run(["git", "-C", str(repo), "-c", "user.email=o@o", "-c",
                            "user.name=o", "commit", "-qm", "other"], capture_output=True)
            return 4, json.dumps({"newest_qualifying": None})
        proc = subprocess.run(cmd, capture_output=True, text=True)
        return proc.returncode, proc.stdout

    g = gate.real_validator(repo, ci_hub=Path("x"), launch=False, runner=runner)
    with pytest.raises(auto_bump.BumpRefused):
        auto_bump.auto_safe_bump(repo, target=NEW, validate=g)
    assert g.state.get("candidate_undone") is False
    assert (repo / "OTHER.md").exists(), "the other agent's commit must survive"
