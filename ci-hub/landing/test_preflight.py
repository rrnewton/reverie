#!/usr/bin/env python3
"""Negative tests for the landing preflight — each check must REFUSE its real defect.

The task's acceptance bar is "each check has a NEGATIVE test proving it refuses
the bad case". Every negative here is modelled on the incident that produced the
rule, not on an invented input, and every negative is paired with a positive
control: a check that refuses everything is as useless as one that refuses
nothing, and only the pair distinguishes them.
"""

from __future__ import annotations

from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import preflight as P  # noqa: E402

HEAD = "a" * 40
OTHER = "b" * 40


# --- 1. the handed SHA is a cache -------------------------------------------


def test_stale_handed_sha_is_refused() -> None:
    """THE INCIDENT: four handed SHAs went stale in one night, one quoted into
    agent instructions for hours after main advanced twice."""
    r = P.check_sha_is_current(HEAD, OTHER)
    assert r.verdict == P.REFUSE
    assert "STALE" in r.reason
    assert r.detail["live_head"] == OTHER


def test_current_handed_sha_passes() -> None:
    assert P.check_sha_is_current(HEAD, HEAD).verdict == P.PASS


def test_unresolvable_head_is_unknown_not_pass() -> None:
    """An unanswerable question must not read as a satisfied one."""
    r = P.check_sha_is_current(HEAD, None)
    assert r.verdict == P.UNKNOWN
    assert r.ok is False, "UNKNOWN must not count as success"


def test_a_prefix_sha_is_refused_rather_than_compared() -> None:
    assert P.check_sha_is_current("a" * 12, HEAD).verdict == P.REFUSE


def test_case_difference_alone_is_not_staleness() -> None:
    """Guard against flags-everything: hex case is not a change of commit."""
    assert P.check_sha_is_current(HEAD.upper(), HEAD).verdict == P.PASS


# --- 2. a green must carry a nonzero executed count -------------------------


def _typed_reader(status: int, executed: int | None = None):
    output = '{"executed_tests":null}' if executed is None else (
        f'{{"executed_tests":{executed}}}'
    )
    return lambda _path, _sha: (status, output, "")


def test_zero_executed_tests_is_refused(tmp_path: Path) -> None:
    """THE INCIDENT: --features gating -- build succeeds, target runs, ZERO tests
    execute, SUCCESS reported. The verdict follows the typed reader's status."""
    row = tmp_path / "row.json"
    row.write_text("typed input")
    r = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=_typed_reader(1, 0)
    )
    assert r.verdict == P.REFUSE
    assert r.detail["typed_reader"]["executed_tests"] == 0


def test_a_real_green_passes(tmp_path: Path) -> None:
    row = tmp_path / "row.json"
    row.write_text("same input path")
    r = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=_typed_reader(0, 36)
    )
    assert r.verdict == P.PASS
    assert r.detail["typed_reader"]["executed_tests"] == 36


def test_typed_value_moves_verdict_while_human_text_is_fixed(tmp_path: Path) -> None:
    """A consumer that calls the reader but still follows prose fails this pair."""
    row = tmp_path / "row.json"
    row.write_text("running 36 tests\ntest result: ok. 36 passed\n")
    accepted = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=_typed_reader(0, 36)
    )
    refused = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=_typed_reader(1, 0)
    )
    assert accepted.verdict == P.PASS
    assert refused.verdict == P.REFUSE


def test_absent_typed_row_is_refused_even_when_banner_exists(tmp_path: Path) -> None:
    """Still-present prose is not a fallback when the typed channel is absent."""
    (tmp_path / "validate.log").write_text(
        "running 36 tests\ntest result: ok. 36 passed\n"
    )
    r = P.check_green_carries_executed_tests(
        str(tmp_path / "missing-row.json"), HEAD
    )
    assert r.verdict == P.REFUSE
    assert "ABSENT" in r.reason


def test_missing_receipt_sha_is_refused(tmp_path: Path) -> None:
    row = tmp_path / "row.json"
    row.write_text("typed input")
    r = P.check_green_carries_executed_tests(
        str(row), None, typed_reader=_typed_reader(0, 36)
    )
    assert r.verdict == P.REFUSE
    assert "SHA is ABSENT" in r.reason


def test_typed_reader_failure_is_unknown(tmp_path: Path) -> None:
    row = tmp_path / "row.json"
    row.write_text("typed input")
    r = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=lambda _path, _sha: (2, "", "bad row")
    )
    assert r.verdict == P.UNKNOWN
    assert r.ok is False


def test_typed_reader_malformed_output_is_unknown(tmp_path: Path) -> None:
    row = tmp_path / "row.json"
    row.write_text("typed input")
    r = P.check_green_carries_executed_tests(
        str(row), HEAD, typed_reader=lambda _path, _sha: (0, "not json", "")
    )
    assert r.verdict == P.UNKNOWN


# --- 3. landing is ancestry, never the MERGED flag --------------------------


def _anc(true_for: set[str]):
    return lambda sha: sha in true_for


def test_merged_flag_without_ancestry_is_refused() -> None:
    """THE INCIDENT: ~12 PRs orphaned by a later force-push on 2026-08-03 --
    still flagged MERGED, replay SHA no longer reachable."""
    r = P.check_landed_by_ancestry(
        pr_state="MERGED", merge_commit_oid="c" * 40,
        is_ancestor=_anc(set()), fetched_fresh=True)
    assert r.verdict == P.REFUSE
    assert "orphaned" in r.reason


def test_merged_with_ancestry_passes() -> None:
    oid = "c" * 40
    r = P.check_landed_by_ancestry(
        pr_state="MERGED", merge_commit_oid=oid,
        is_ancestor=_anc({oid}), fetched_fresh=True)
    assert r.verdict == P.PASS


def test_stale_remote_is_unknown_not_pass() -> None:
    """A stale ref answers about the past."""
    oid = "c" * 40
    r = P.check_landed_by_ancestry(
        pr_state="MERGED", merge_commit_oid=oid,
        is_ancestor=_anc({oid}), fetched_fresh=False)
    assert r.verdict == P.UNKNOWN
    assert r.ok is False


def test_merged_without_a_merge_commit_is_refused() -> None:
    r = P.check_landed_by_ancestry(
        pr_state="MERGED", merge_commit_oid=None,
        is_ancestor=_anc(set()), fetched_fresh=True)
    assert r.verdict == P.REFUSE
    assert "not landing evidence" in r.reason


def test_the_check_never_consults_the_pr_head() -> None:
    """THE INCIDENT: testing ancestry on the PR HEAD read 79 unlanded when 46 had
    landed, because a rebase replay makes the head NEVER an ancestor.

    Proven structurally: ancestry is TRUE for the merge commit and FALSE for the
    head, and the verdict follows the merge commit.
    """
    oid, head = "c" * 40, "d" * 40
    r = P.check_landed_by_ancestry(
        pr_state="MERGED", merge_commit_oid=oid, pr_head=head,
        is_ancestor=_anc({oid}), fetched_fresh=True)
    assert r.verdict == P.PASS, "must not have been decided on the head"
    asked: list[str] = []

    def spy(sha):
        asked.append(sha)
        return sha == oid

    P.check_landed_by_ancestry(pr_state="MERGED", merge_commit_oid=oid, pr_head=head,
                               is_ancestor=spy, fetched_fresh=True)
    assert asked == [oid], f"the head must never be tested for ancestry: {asked}"


# --- 4. the reverie patch override ------------------------------------------


PATCH_DIFF = '''\
diff --git a/Cargo.toml b/Cargo.toml
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -41,6 +41,10 @@
+[patch."https://github.com/rrnewton/reverie.git"]
+reverie = { path = "../reverie" }
'''


def test_added_reverie_patch_override_is_refused() -> None:
    """THE TRAP: live in worktrees/250-delegate/hermit Cargo.toml L44-53."""
    r = P.check_no_uncommitted_patch_override(PATCH_DIFF)
    assert r.verdict == P.REFUSE


def test_unrelated_diff_passes() -> None:
    assert P.check_no_uncommitted_patch_override(
        "diff --git a/src/x.rs b/src/x.rs\n+fn x() {}\n").verdict == P.PASS


def test_a_removed_override_is_not_flagged() -> None:
    """Keyed on ADDED lines: deleting an override is the fix, not the defect."""
    removal = PATCH_DIFF.replace("\n+[patch", "\n-[patch").replace(
        "\n+reverie =", "\n-reverie =")
    assert P.check_no_uncommitted_patch_override(removal).verdict == P.PASS


# --- 5. the byte-identical branch -------------------------------------------


def test_byte_identical_branch_is_refused() -> None:
    """THE TRAP: live on #355 -- work opened that already existed verbatim."""
    r = P.check_no_byte_identical_branch(
        candidate_tree="t1", remote_trees={"fix/a": "t1", "other": "t2"})
    assert r.verdict == P.REFUSE
    assert r.detail["branches"] == ["fix/a"]


def test_distinct_tree_passes() -> None:
    assert P.check_no_byte_identical_branch(
        candidate_tree="t9", remote_trees={"fix/a": "t1"}).verdict == P.PASS


# --- the gate as a whole -----------------------------------------------------


def test_unknown_blocks_the_gate(tmp_path: Path, monkeypatch) -> None:
    """UNKNOWN must not be launderable into success by the exit code."""
    row = tmp_path / "row.json"
    row.write_text("typed input")
    monkeypatch.setattr(P, "_read_typed_executed_tests", _typed_reader(0, 36))
    rc = P.main([
        "--sha", HEAD,
        "--ledger-event", str(row), "--receipt-sha", HEAD,
        "--no-network",
    ])
    assert rc == 1, "an unresolved head must block, not pass"


def test_all_pass_exits_zero(tmp_path: Path, monkeypatch) -> None:
    """POSITIVE CONTROL for the gate itself: it can actually succeed."""
    row = tmp_path / "row.json"
    row.write_text("typed input")
    monkeypatch.setattr(P, "_read_typed_executed_tests", _typed_reader(0, 36))
    rc = P.main([
        "--sha", HEAD, "--live-head", HEAD,
        "--ledger-event", str(row), "--receipt-sha", HEAD,
        "--no-network",
    ])
    assert rc == 0


def test_zero_test_row_fails_the_gate(tmp_path: Path, monkeypatch) -> None:
    row = tmp_path / "row.json"
    row.write_text("typed input")
    monkeypatch.setattr(P, "_read_typed_executed_tests", _typed_reader(1, 0))
    rc = P.main([
        "--sha", HEAD, "--live-head", HEAD,
        "--ledger-event", str(row), "--receipt-sha", HEAD,
        "--no-network",
    ])
    assert rc == 1
