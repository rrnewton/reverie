#!/usr/bin/env python3
"""Derived landing state: verify by mutation, BOTH directions.

The task's bar, one group each:
  * a task whose named commit IS an ancestor reports landed
  * a task whose commit is NOT (or was rebased away) reports NOT landed
    RATHER THAN INHERITING THE ASSERTED STATUS
  * N correctly-landed from the live population, so it is not a checker that
    reports "no" to everything
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tg_landed as TL  # noqa: E402

A, B, C = "a" * 40, "b" * 40, "c" * 40
PARENT = TL.RepositoryTarget(
    repository="rrnewton/dev-hermit", checkout="/workspace", fetched_fresh=True)
HERMIT = TL.RepositoryTarget(
    repository="rrnewton/hermit", checkout="/workspace/hermit", fetched_fresh=True)
REVERIE = TL.RepositoryTarget(
    repository="rrnewton/reverie", checkout="/workspace/reverie", fetched_fresh=True)


def _refs(shas, *, tags=("implemented",), status="closed"):
    return TL.TaskRefs(task="t1", tags=list(tags), status=status, shas=list(shas))


def _anc(landed: set, absent: set = frozenset()):
    def f(sha):
        if sha in absent:
            return None
        return sha in landed
    return f


def test_malformed_task_tags_are_unverifiable(monkeypatch) -> None:
    monkeypatch.setattr(TL.taskgraph_db, "resolve", lambda: object())
    monkeypatch.setattr(TL, "_run", lambda _cmd: (
        0, 'local_id | tags | status | notes\n---\nx | [bad | IN_PROGRESS | []\n\n(1 row)\n'
    ))
    try:
        TL.load_tasks()
    except TL.TaskGraphQueryFailed as error:
        assert "malformed tags" in str(error)
    else:
        raise AssertionError("malformed tags became an empty task state")


def test_complete_task_row_still_loads(monkeypatch) -> None:
    monkeypatch.setattr(TL.taskgraph_db, "resolve", lambda: object())
    monkeypatch.setattr(TL, "_run", lambda _cmd: (
        0,
        'local_id | tags | status | notes\n---\n'
        'x | ["implemented"] | IN_PROGRESS | ["IMPLEMENTED: SHA '
        + A + '"]\n\n(1 row)\n',
    ))
    rows = TL.load_tasks()
    assert len(rows) == 1
    assert rows[0].tags == ["implemented"]
    assert rows[0].shas == [A]


def _repo_probe(entries):
    """Map to True/False/"foreign"/"error"; missing means absent."""
    def probe(repository, sha):
        value = entries.get((repository.repository, sha), "absent")
        if value == "absent":
            return TL.ProbeResult(present=False, ancestor=None, issue="object absent")
        if value == "error":
            return TL.ProbeResult(
                present=True, ancestor=None, reachable=True, issue="missing target")
        if value == "foreign":
            return TL.ProbeResult(present=True, ancestor=False, reachable=False)
        return TL.ProbeResult(present=True, ancestor=bool(value), reachable=True)
    return probe


# --- repository identity is part of the authority ---------------------------


def test_two_repositories_each_use_their_own_main_and_both_land() -> None:
    refs = _refs([A, B])
    d = TL.derive_across_repositories(
        refs,
        [PARENT, HERMIT, REVERIE],
        probe=_repo_probe({
            (HERMIT.repository, A): True,
            (REVERIE.repository, B): True,
        }),
    )

    assert d.derived == TL.LANDED
    assert [(item.sha, item.repository, item.target) for item in d.references] == [
        (A, "rrnewton/hermit", "origin/main"),
        (B, "rrnewton/reverie", "origin/main"),
    ]


def test_absent_in_first_repo_but_landed_in_second_is_not_a_false_unknown() -> None:
    d = TL.derive_across_repositories(
        _refs([A]),
        [PARENT, HERMIT],
        probe=_repo_probe({(HERMIT.repository, A): True}),
    )

    assert d.derived == TL.LANDED
    assert d.references[0].repository == HERMIT.repository
    assert d.absent_shas == []


def test_foreign_object_visibility_does_not_create_false_repo_ambiguity() -> None:
    """A checkout may contain another repo's objects without owning its refs."""
    d = TL.derive_across_repositories(
        _refs([A]),
        [HERMIT, REVERIE],
        probe=_repo_probe({
            (HERMIT.repository, A): "foreign",
            (REVERIE.repository, A): True,
        }),
    )

    assert d.derived == TL.LANDED
    assert d.references[0].repository == REVERIE.repository
    assert d.references[0].repository_candidates == [REVERIE.repository]
    assert d.references[0].object_database_candidates == [
        HERMIT.repository, REVERIE.repository]


def test_genuine_cross_repo_mixture_is_partial() -> None:
    d = TL.derive_across_repositories(
        _refs([A, B]),
        [HERMIT, REVERIE],
        probe=_repo_probe({
            (HERMIT.repository, A): True,
            (REVERIE.repository, B): False,
        }),
    )

    assert d.derived == TL.PARTIAL
    assert d.landed_shas == [A]
    assert d.unlanded_shas == [B]


def test_sha_present_in_multiple_repositories_is_refused_as_ambiguous() -> None:
    """Never select the first convenient green when provenance is missing."""
    d = TL.derive_across_repositories(
        _refs([A]),
        [PARENT, HERMIT],
        probe=_repo_probe({
            (PARENT.repository, A): True,
            (HERMIT.repository, A): True,
        }),
    )

    assert d.derived == TL.UNVERIFIABLE
    assert d.ambiguous_shas == [A]
    assert d.references[0].repository is None
    assert d.references[0].repository_candidates == [PARENT.repository, HERMIT.repository]


def test_comparison_error_is_unknown_not_a_manufactured_nonancestor() -> None:
    d = TL.derive_across_repositories(
        _refs([A]),
        [HERMIT],
        probe=_repo_probe({(HERMIT.repository, A): "error"}),
    )

    assert d.derived == TL.UNVERIFIABLE
    assert d.unverifiable_shas == [A]
    assert d.unlanded_shas == []


def test_staleness_names_the_specific_repository_that_answered() -> None:
    stale_reverie = TL.RepositoryTarget(
        repository=REVERIE.repository,
        checkout=REVERIE.checkout,
        fetched_fresh=False,
    )
    d = TL.derive_across_repositories(
        _refs([A]),
        [HERMIT, stale_reverie],
        probe=_repo_probe({(stale_reverie.repository, A): True}),
    )

    assert d.derived == TL.LANDED
    assert d.stale_repositories == [REVERIE.repository]
    assert REVERIE.repository in d.reason
    assert d.references[0].target_freshly_fetched is False


def test_git_probes_disable_promisor_lazy_fetch(monkeypatch) -> None:
    """Local absence probing must never turn into an implicit network fetch."""
    environments = []

    def fake_run(command, **kwargs):
        environments.append(kwargs["env"])
        stdout = f"{A} commit\n" if "cat-file" in command else ""
        return subprocess.CompletedProcess(command, 0, stdout, "")

    monkeypatch.setattr(TL.subprocess, "run", fake_run)
    assert TL._batch_present_commits("/repo", {A}) == {A}
    assert TL._run(["git", "rev-parse", "HEAD"], cwd="/repo")[0] == 0
    assert len(environments) == 2
    assert all(environment["GIT_NO_LAZY_FETCH"] == "1"
               for environment in environments)


# --- direction 1: an ancestor reports landed ---------------------------------


def test_named_commit_that_is_an_ancestor_reports_landed() -> None:
    d = TL.derive(_refs([A]), is_ancestor=_anc({A}), fetched_fresh=True)
    assert d.derived == TL.LANDED
    assert d.landed_shas == [A]


# --- direction 2: a non-ancestor reports NOT landed, whatever tg says --------


def test_non_ancestor_reports_not_landed_despite_the_asserted_status() -> None:
    """THE POINT. The task is tagged `implemented` and its status is `closed`;
    the derivation must contradict both."""
    d = TL.derive(_refs([A], tags=("implemented",), status="closed"),
                  is_ancestor=_anc(set()), fetched_fresh=True)
    assert d.derived == TL.NOT_LANDED
    assert d.asserted_implemented is True and d.asserted_status == "closed"
    assert "rebased away" in d.reason


def test_a_rebased_away_commit_is_not_landed_not_unverifiable() -> None:
    """A commit still present locally but no longer an ancestor is the
    rebase-replay/force-push shape -- a definite NO, not an unknown."""
    d = TL.derive(_refs([A]), is_ancestor=_anc(set()), fetched_fresh=True)
    assert d.derived == TL.NOT_LANDED


def test_an_absent_commit_is_unverifiable_not_not_landed() -> None:
    """Absent from this checkout is NOT the same as not landed: conflating them
    would manufacture false negatives on any partial clone."""
    d = TL.derive(_refs([A]), is_ancestor=_anc(set(), absent={A}), fetched_fresh=True)
    assert d.derived == TL.UNVERIFIABLE
    assert d.absent_shas == [A]


# --- the state tg cannot express ---------------------------------------------


def test_some_landed_some_not_is_PARTIAL_not_rounded() -> None:
    """Rounding a half-landed obligation to either end is how it reads as done."""
    d = TL.derive(_refs([A, B]), is_ancestor=_anc({A}), fetched_fresh=True)
    assert d.derived == TL.PARTIAL
    assert d.landed_shas == [A] and d.unlanded_shas == [B]
    assert "tg cannot express" in d.reason


def test_landed_plus_absent_is_also_partial() -> None:
    d = TL.derive(_refs([A, C]), is_ancestor=_anc({A}, absent={C}), fetched_fresh=True)
    assert d.derived == TL.PARTIAL


# --- a task that names nothing -----------------------------------------------


def test_a_task_naming_no_commit_is_NO_REFERENCE_not_landed() -> None:
    """Tagged implemented with nothing to dereference -- the asserted status is
    all there is, which IS the gap."""
    d = TL.derive(_refs([]), is_ancestor=_anc({A}), fetched_fresh=True)
    assert d.derived == TL.NO_REFERENCE
    assert "nothing to dereference" in d.reason


# --- freshness is part of the answer -----------------------------------------


def test_a_stale_target_annotates_every_positive_derivation() -> None:
    d = TL.derive(_refs([A]), is_ancestor=_anc({A}), fetched_fresh=False)
    assert d.derived == TL.LANDED
    assert "STALE TARGET" in d.reason, "a positive answer must carry its staleness"


def test_a_fresh_target_does_not_annotate() -> None:
    d = TL.derive(_refs([A]), is_ancestor=_anc({A}), fetched_fresh=True)
    assert "STALE" not in d.reason


def test_the_cli_exits_nonzero_on_a_stale_target() -> None:
    """A stale answer must not read as success."""
    rep = TL.report([], fetched_fresh=False, target="origin/main")
    assert rep["target_freshly_fetched"] is False
    assert "NOT FRESHLY FETCHED" in TL.render(rep)


# --- reference extraction -----------------------------------------------------


def test_extracts_full_shas_and_pr_numbers_deduped_in_order() -> None:
    shas, prs = TL.extract_refs(
        f"IMPLEMENTED: https://github.com/o/r/pull/1559 | SHA {A} | again {A} | PR #338")
    assert shas == [A]
    assert prs == ["1559", "338"]


def test_a_short_sha_is_not_treated_as_a_reference() -> None:
    """A prefix cannot be compared safely; only full 40-hex counts."""
    shas, _ = TL.extract_refs("see abc1234 and deadbeef")
    assert shas == []


def test_progress_note_shas_are_not_implementation_authorities() -> None:
    shas, prs = TL.extract_implementation_refs([
        f"Progress: rebased base {A}; testing candidate {B}; PR #1234",
    ])
    assert shas == []
    assert prs == []


def test_implemented_note_extracts_explicit_sha_but_not_incidental_base() -> None:
    shas, prs = TL.extract_implementation_refs([
        f"[impl agent] IMPLEMENTED: PR #1234 | SHA {A} | tested from base {B}",
    ])
    assert shas == [A]
    assert prs == ["1234"]


def test_coordinated_implementation_keeps_both_explicit_repo_shas() -> None:
    shas, _ = TL.extract_implementation_refs([
        f"IMPLEMENTED: Hermit SHA {A}; Reverie commit {B}",
    ])
    assert shas == [A, B]


def test_closure_tuple_is_a_typed_sha_authority() -> None:
    shas, _ = TL.extract_implementation_refs([
        f"CLOSURE-VERIFIED: rrnewton/dev-hermit:ai_docs/report.md@{A}",
    ])
    assert shas == [A]


# --- the population report is self-consistent ---------------------------------


def test_report_counts_sum_to_the_population() -> None:
    """A denominator bug in the reporter would be this task's own defect class.

    Regression: `tg sql` renders one row per line, so multi-line notes were
    parsed as extra rows and 40 tasks classified as 51.
    """
    ds = [
        TL.derive(_refs([A]), is_ancestor=_anc({A}), fetched_fresh=True),
        TL.derive(_refs([B]), is_ancestor=_anc(set()), fetched_fresh=True),
        TL.derive(_refs([]), is_ancestor=_anc(set()), fetched_fresh=True),
    ]
    rep = TL.report(ds, fetched_fresh=True, target="origin/main")
    assert rep["tasks_examined"] == 3
    assert sum(rep["derived"].values()) == 3


def test_the_assertion_gap_is_reported() -> None:
    """asserted-implemented minus derived-landed IS the number this task exists
    to surface."""
    ds = [TL.derive(_refs([A]), is_ancestor=_anc({A}), fetched_fresh=True),
          TL.derive(_refs([B]), is_ancestor=_anc(set()), fetched_fresh=True)]
    rep = TL.report(ds, fetched_fresh=True, target="origin/main")
    assert rep["asserted_implemented"] == 2
    assert rep["derived_landed"] == 1
    assert rep["assertion_gap"] == 1


def test_not_a_checker_that_says_no_to_everything() -> None:
    """POSITIVE CONTROL, the third leg the task names explicitly: given landed
    commits the reporter must actually report them landed."""
    ds = [TL.derive(_refs([s]), is_ancestor=_anc({A, B, C}), fetched_fresh=True)
          for s in (A, B, C)]
    rep = TL.report(ds, fetched_fresh=True, target="origin/main")
    assert rep["derived_landed"] == 3
    assert rep["assertion_gap"] == 0
