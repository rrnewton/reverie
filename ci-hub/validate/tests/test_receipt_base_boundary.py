#!/usr/bin/env python3
"""Two-sided brackets for receipt-carried base evidence at merge time."""

from __future__ import annotations

import json
import re
import os
from pathlib import Path
import subprocess
import sys


sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import qualifying_receipt as qr  # noqa: E402


ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "base-fixture",
    "GIT_AUTHOR_EMAIL": "base@example.invalid",
    "GIT_COMMITTER_NAME": "base-fixture",
    "GIT_COMMITTER_EMAIL": "base@example.invalid",
}


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
        env=ENV,
    ).stdout.strip()


def commit(repo: Path, message: str) -> str:
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", message)
    return git(repo, "rev-parse", "HEAD")


def make_repo(path: Path) -> Path:
    path.mkdir()
    git(path, "init", "-q")
    return path


def row_for(
    hermit: Path, head: str, base: str, reverie: Path, reverie_base: str
) -> dict:
    return {
        "commit": head,
        "commit_anchored": True,
        "tree_dirty": False,
        "selection_mode": "full",
        "profile": "full",
        "result": "pass",
        "raw_result": "pass",
        "exit_code": 0,
        "failures": 0,
        "executed_tests": 1,
        "passed_tests": 1,
        "filtered_tests": 0,
        "schema_version": 5,
        "repo": "hermit",
        "tree": qr.git_tree(str(hermit), head),
        "started_at": "2026-08-08T00:00:00Z",
        "finished_at": "2026-08-08T00:01:00Z",
        "host": "base-boundary-fixture",
        "slot": "fixture-slot",
        "log_file": "/tmp/base-boundary-fixture.log",
        "checks": 1,
        "gates_run": 1,
        "gates_expected": 1,
        "gates": [{"name": "fixture", "result": "pass", "exit_code": 0}],
        "coverage": {
            "planned_test_nodes": 1,
            "executed_test_nodes": 1,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "base_sha": base,
        "base_tree": qr.git_tree(str(hermit), base),
        "reverie_base_sha": reverie_base,
        "reverie_base_tree": qr.git_tree(str(reverie), reverie_base),
    }


def running_consumer(
    row: dict,
    hermit: Path,
    current_base: str,
    reverie: Path,
    current_reverie: str,
) -> subprocess.CompletedProcess[str]:
    root = Path(__file__).resolve().parents[3]
    return subprocess.run(
        [
            str(root / "ci-hub/ci-hub"),
            "receipt-digest",
            "--sha",
            row["commit"],
            "--require-qualifying",
            "--current-base",
            current_base,
            "--current-reverie-base",
            current_reverie,
            "--repo-checkout",
            str(hermit),
            "--reverie-checkout",
            str(reverie),
        ],
        input=json.dumps(row),
        capture_output=True,
        text=True,
        env=ENV,
    )


def test_merge_boundary_brackets_both_ways_and_preserves_schema4(tmp_path: Path) -> None:
    hermit = make_repo(tmp_path / "hermit")
    (hermit / "src").mkdir()
    (hermit / "src/lib.rs").write_text("pub fn one() {}\n")
    old_base = commit(hermit, "old base")
    (hermit / "src/lib.rs").write_text("pub fn two() {}\n")
    current_base = commit(hermit, "current base")
    (hermit / "src/pr.rs").write_text("pub fn pr_change() {}\n")
    pr_head = commit(hermit, "legitimately based PR head")
    git(hermit, "checkout", "-q", old_base)
    (hermit / "src/unsafe.rs").write_text("pub fn unsafe_base() {}\n")
    unsafe_head = commit(hermit, "head that does not contain current base")
    git(hermit, "checkout", "-q", pr_head)

    reverie = make_repo(tmp_path / "reverie")
    (reverie / "reverie-dbi/src").mkdir(parents=True)
    (reverie / "reverie-dbi/src/lib.rs").write_text("pub fn dbi() {}\n")
    (reverie / "AGENTS.md").write_text("old policy\n")
    old_reverie = commit(reverie, "build content")
    (reverie / "AGENTS.md").write_text("new policy only\n")
    current_reverie = commit(reverie, "policy-only delta")
    git(reverie, "checkout", "-qb", "divergent", old_reverie)
    (reverie / "reverie-dbi/src/lib.rs").write_text("pub fn divergent() {}\n")
    divergent_reverie = commit(reverie, "divergent line")
    git(reverie, "checkout", "-q", current_reverie)

    pred = qr.active()

    positive = row_for(hermit, pr_head, current_base, reverie, current_reverie)
    assert qr.base_boundary_verdict(
        positive,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.SATISFIED
    assert running_consumer(
        positive, hermit, current_base, reverie, current_reverie
    ).returncode == 0

    missing_passed = dict(positive)
    missing_passed.pop("passed_tests")
    assert qr.row_qualification(missing_passed, pr_head, pred) == (
        False,
        qr.PASSED_TESTS_REFUSAL_REASON,
    )
    assert running_consumer(
        missing_passed, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    # A recorded Reverie base remains current while it is on live main's
    # history. This is the same stable ancestry policy used by Hermit's pin
    # checker: a later content-changing commit must not invalidate the receipt.
    ancestor_reverie = row_for(hermit, pr_head, current_base, reverie, old_reverie)
    assert qr.base_boundary_verdict(
        ancestor_reverie,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.SATISFIED
    assert running_consumer(
        ancestor_reverie, hermit, current_base, reverie, current_reverie
    ).returncode == 0

    # A resolvable SHA on a divergent line is not contained by Reverie main.
    # It must refuse even though its recorded tree is internally consistent.
    divergent = row_for(
        hermit, pr_head, current_base, reverie, divergent_reverie
    )
    assert qr.base_boundary_verdict(
        divergent,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.REVERIE_BASE_NOT_CURRENT
    assert running_consumer(
        divergent, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    # An ancestor does not satisfy the boundary when its recorded tree is
    # corrupt. Tree equality stays exact at the commit the receipt names.
    wrong_reverie_tree = dict(ancestor_reverie)
    wrong_reverie_tree["reverie_base_tree"] = qr.git_tree(
        str(reverie), current_reverie
    )
    assert qr.base_boundary_verdict(
        wrong_reverie_tree,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.REVERIE_TREE_MISMATCH
    assert running_consumer(
        wrong_reverie_tree, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    # A receipt keeps its historical admitted base while main may advance into
    # the already-validated head.  This is the no-op-rebase case: the head is
    # still exact and current, so no rebase provenance row exists or is needed.
    advanced_main = row_for(hermit, pr_head, old_base, reverie, current_reverie)
    assert qr.base_boundary_verdict(
        advanced_main,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.SATISFIED
    assert running_consumer(
        advanced_main, hermit, current_base, reverie, current_reverie
    ).returncode == 0

    # The historical base remains exact evidence.  Pointing its tree field at
    # the newer main tree must refuse even though both commits are ancestors of
    # the validated head.
    wrong_base_tree = dict(advanced_main)
    wrong_base_tree["base_tree"] = qr.git_tree(str(hermit), current_base)
    assert qr.base_boundary_verdict(
        wrong_base_tree,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.BASE_TREE_MISMATCH
    assert running_consumer(
        wrong_base_tree, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    # Current-main containment cannot substitute for containment of the base
    # the receipt itself names.
    unrelated_recorded_base = row_for(
        hermit, pr_head, unsafe_head, reverie, current_reverie
    )
    assert qr.base_boundary_verdict(
        unrelated_recorded_base,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.BASE_NOT_CONTAINED
    assert running_consumer(
        unrelated_recorded_base, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    unsafe = row_for(hermit, unsafe_head, current_base, reverie, current_reverie)
    assert qr.base_boundary_verdict(
        unsafe,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.BASE_NOT_CONTAINED
    assert running_consumer(
        unsafe, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    missing = dict(positive)
    missing.pop("base_sha")
    assert qr.base_evidence_verdict(missing, pred) is qr.BaseVerdict.BASE_SHA_MISSING
    assert running_consumer(
        missing, hermit, current_base, reverie, current_reverie
    ).returncode != 0

    # Honest grandfathering: pre-obligation rows remain unknown, never false.
    grandfathered = dict(positive)
    grandfathered["schema_version"] = 4
    # A grandfathered schema-4 row carries NO coverage object; since d58edca
    # schema-4 + non-null coverage is the refused invalid shape, not history.
    grandfathered.pop("coverage", None)
    for field in (
        "base_sha",
        "base_tree",
        "reverie_base_sha",
        "reverie_base_tree",
    ):
        grandfathered.pop(field)
    assert qr.base_boundary_verdict(
        grandfathered,
        pred,
        current_base=current_base,
        current_reverie_base=current_reverie,
        repo_checkout=str(hermit),
        reverie_checkout=str(reverie),
    ) is qr.BaseVerdict.GRANDFATHERED_UNKNOWN
    assert running_consumer(
        grandfathered, hermit, current_base, reverie, current_reverie
    ).returncode == 0


def test_being_behind_main_is_not_a_landing_obstacle(tmp_path):
    """Currency is ancestry, and the two directions must be told apart.

    The Hermit leg of the boundary used to require the CURRENT `main` tip to be
    contained in the validated commit, so an ordinary branch that was merely
    behind was refused with the same verdict as one sitting on a base that is
    not `main` history at all. Those are different situations and only the
    second is a hazard.

    This is deliberately pure Python: it drives `base_boundary_verdict`
    directly rather than the Rust consumer, so it runs in a checkout without
    submodules. The consumer-side brackets live in the test above.
    """
    hermit = make_repo(tmp_path / "hermit")
    reverie = make_repo(tmp_path / "reverie")
    (reverie / "r.txt").write_text("r\n")
    reverie_base = commit(reverie, "reverie base")

    (hermit / "a.txt").write_text("root\n")
    root = commit(hermit, "root")

    # The receipt's base, and a validated head that contains it.
    git(hermit, "checkout", "-q", "-B", "feature", root)
    (hermit / "x.txt").write_text("x\n")
    recorded_base = commit(hermit, "recorded base")
    (hermit / "h.txt").write_text("h\n")
    validated_head = commit(hermit, "validated head")

    # One `main` that contains the recorded base and has moved on past the
    # validated head -- the ordinary case of simply being behind.
    git(hermit, "checkout", "-q", "-B", "ahead", recorded_base)
    (hermit / "g.txt").write_text("g\n")
    main_ahead = commit(hermit, "main advances past the head")

    # Another `main` that does NOT contain the recorded base at all, which is
    # the real hazard: the receipt speaks for a tree off `main` history.
    git(hermit, "checkout", "-q", "-B", "elsewhere", root)
    (hermit / "m.txt").write_text("m\n")
    main_elsewhere = commit(hermit, "main advances elsewhere")

    pred = qr.active()
    row = row_for(hermit, validated_head, recorded_base, reverie, reverie_base)

    def verdict(current_base):
        return qr.base_boundary_verdict(
            row,
            pred,
            current_base=current_base,
            current_reverie_base=reverie_base,
            repo_checkout=str(hermit),
            reverie_checkout=str(reverie),
        )

    # POSITIVE: behind is fine when the recorded base is on `main` history.
    assert verdict(main_ahead) is qr.BaseVerdict.SATISFIED
    # NEGATIVE, and it must be a DIFFERENT verdict rather than the same one:
    # a base that is not `main` history still refuses.
    assert verdict(main_elsewhere) is qr.BaseVerdict.BASE_NOT_CURRENT
    assert verdict(main_ahead) is not verdict(main_elsewhere)


def test_ancestry_alone_would_admit_an_arbitrarily_stale_receipt(tmp_path, monkeypatch):
    """Clause 2 of the cited rule, applied here: ancestry is not the whole rule.

    Every base ever merged stays an ancestor forever, so an ancestry-only
    currency check admits a receipt of any age. `scripts/check-gitlink-currency.sh`
    reached the same conclusion for gitlinks -- "ancestry ALONE is what permitted
    374 commits" -- and paired ancestry with a bounded lag.

    The edge is driven through a SMALL EXPLICIT BOUND rather than the default, so
    this pins the SHAPE of the rule without re-encoding its magnitude. A fixture
    sized from the default would both take ten seconds and silently accept any
    recalibration of it; the default's own value is pinned separately below,
    against its source rather than against a copy.
    """
    hermit = make_repo(tmp_path / "hermit")
    reverie = make_repo(tmp_path / "reverie")
    (reverie / "r.txt").write_text("r\n")
    reverie_base = commit(reverie, "reverie base")

    (hermit / "a.txt").write_text("root\n")
    root = commit(hermit, "root")
    git(hermit, "checkout", "-q", "-B", "feature", root)
    (hermit / "h.txt").write_text("h\n")
    validated_head = commit(hermit, "validated long ago")

    # `main` marches past the recorded base. The recorded base stays an ancestor
    # of every one of these, which is exactly why ancestry alone is insufficient.
    git(hermit, "checkout", "-q", "-B", "mainline", root)
    tips = {}
    for index in range(1, 8):
        (hermit / f"m{index}.txt").write_text(f"{index}\n")
        tips[index] = commit(hermit, f"main {index}")

    pred = qr.active()
    row = row_for(hermit, validated_head, root, reverie, reverie_base)

    def verdict(current_base):
        return qr.base_boundary_verdict(
            row,
            pred,
            current_base=current_base,
            current_reverie_base=reverie_base,
            repo_checkout=str(hermit),
            reverie_checkout=str(reverie),
        )

    monkeypatch.setenv("RECEIPT_BASE_MAX_LAG_COMMITS", "5")
    # Ordinary lag is fine; that is the whole point of lifting tip equality.
    assert verdict(tips[1]) is qr.BaseVerdict.SATISFIED
    # The edge is exact rather than approximate.
    assert verdict(tips[5]) is qr.BaseVerdict.SATISFIED
    assert verdict(tips[6]) is qr.BaseVerdict.BASE_TOO_STALE
    # And it is a REFUSING verdict, not a warning.
    assert not qr.base_accepted(qr.BaseVerdict.BASE_TOO_STALE)

    # Disabling the bound restores pure ancestry, which is the behaviour this
    # test exists to say is NOT the default.
    monkeypatch.setenv("RECEIPT_BASE_MAX_LAG_COMMITS", "0")
    assert verdict(tips[7]) is qr.BaseVerdict.SATISFIED
    # A malformed or negative override is fatal rather than silently ignored: a
    # bound that quietly reverts to its default measures something other than
    # what the operator asked for.
    for bad in ("not-a-number", "-1", "5.0"):
        monkeypatch.setenv("RECEIPT_BASE_MAX_LAG_COMMITS", bad)
        assert verdict(tips[1]) is qr.BaseVerdict.POLICY_UNAVAILABLE


def test_the_default_lag_bound_still_matches_the_source_it_was_borrowed_from():
    """The default is a SECOND COPY of a number, so it is checked against the first.

    `RECEIPT_BASE_MAX_LAG_COMMITS_DEFAULT` is borrowed from the FAIL threshold in
    `scripts/check-gitlink-currency.sh`. Two copies of a constant with nothing
    holding them together is the drift this whole change exists to argue against,
    so the copy is pinned to its source rather than to a literal repeated here.
    """
    source = (
        Path(__file__).resolve().parents[3] / "scripts" / "check-gitlink-currency.sh"
    )
    text = source.read_text()
    match = re.search(r"^FAIL=\$\{GITLINK_FAIL_COMMITS:-([0-9]+)\}", text, re.M)
    assert match, f"cannot find the FAIL default in {source}"
    assert qr.RECEIPT_BASE_MAX_LAG_COMMITS_DEFAULT == int(match.group(1)), (
        "the borrowed lag bound and its source have drifted apart; "
        "re-derive the churn and change both deliberately, or stop borrowing"
    )
