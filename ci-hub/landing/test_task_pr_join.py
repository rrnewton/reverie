#!/usr/bin/env python3
"""Mutation bar for task_pr_join.compute_join -- BOTH directions.

The join's stakes are asymmetric (an UNMATCHED PR invites a look; a WRONGLY
matched PR gets closed), so a negative-only test is worthless: a join that
matches NOTHING would pass it. Every test here therefore brackets both sides and
asserts counts, and the live-population test asserts a nonzero-but-strict-subset
match on realistic fixtures.

Run: python3 ci-hub/landing/test_task_pr_join.py
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from task_pr_join import compute_join, BATCH_FANOUT_THRESHOLD, _task_tags  # noqa: E402
import task_pr_record  # noqa: E402


def _task(tid, title="", desc="", notes=None, tags=None, status="in_progress"):
    return {"id": tid, "title": title, "desc": desc, "notes": notes or [],
            "tags": tags or [], "status": status}


def _pr(repo, number, branch, sha, title="", body="", files=None, draft=False):
    return {
        "repo": repo, "number": number, "headRefName": branch,
        "headRefOid": sha, "title": title, "body": body,
        "isDraft": draft,
        "url": f"https://github.com/{repo}/pull/{number}",
        "files": [{"path": p} for p in (files or [])],
    }


def _num_in(pop, repo, number):
    return any(m["repo"] == repo and m["number"] == number for m in pop)


def _tid_in(pop, repo, number, tid):
    for m in pop:
        if m["repo"] == repo and m["number"] == number:
            names = {e["task"] for e in
                     m.get("dedicated_confirmed", []) + m["confirmed"]
                     + m["probable"] + m["weak"]}
            return tid in names
    return False


FAILS = []


def test_valid_tags_are_preserved():
    assert _task_tags({"id": "x", "tags": '["implemented"]'}) == ["implemented"]


def test_malformed_tags_refuse_the_join():
    try:
        _task_tags({"id": "x", "tags": "[not json"})
    except RuntimeError as error:
        assert "malformed tags" in str(error)
    else:
        raise AssertionError("malformed task tags were accepted as an empty list")


def test_non_string_tags_refuse_the_join():
    for tags in ("[1]", [1]):
        try:
            _task_tags({"id": "x", "tags": tags})
        except RuntimeError as error:
            assert "list of strings" in str(error)
        else:
            raise AssertionError("non-string task tags were accepted")


def check(cond, msg):
    if cond:
        print(f"  ok: {msg}")
    else:
        print(f"  FAIL: {msg}")
        FAILS.append(msg)


def test_positive_dedicated_match():
    print("test_positive_dedicated_match")
    repo = "rrnewton/hermit"
    sha = "abc123def4567890abc123def4567890abc12345"
    tasks = [_task(
        "land-ppoll-fix", title="land the ppoll determinization fix",
        notes=[f"Implemented: https://github.com/{repo}/pull/1234 "
               f"| branch fix-ppoll-syscall @ {sha}"])]
    prs = [_pr(repo, 1234, "fix-ppoll-syscall", sha, title="ppoll fix",
               files=["detcore/src/syscalls/ppoll.rs"])]
    r = compute_join(tasks, prs)
    # POSITIVE side fires: the planted task with a real PR is CONFIRMED+dedicated.
    check(_num_in(r["population_A_matched"], repo, 1234),
          "planted task with real PR url+branch+sha -> population A (matched)")
    check(_tid_in(r["population_A_matched"], repo, 1234, "land-ppoll-fix"),
          "the matched owner is the planted task, not another")


def test_negative_closed_renamed_not_bound_to_neighbour():
    print("test_negative_closed_renamed_not_bound_to_neighbour")
    repo = "rrnewton/hermit"
    # Task named PR #900 (since CLOSED) with an old branch/sha. #900 is not in
    # the open set. The live neighbour #901 was renamed and has a fresh sha; the
    # task does NOT name its identity. #901 must NOT bind to the stale task.
    tasks = [_task(
        "stale-owner", title="fix the kvm winsize divergence",
        notes=[f"Implemented: https://github.com/{repo}/pull/900 "
               f"| branch kvm-winsize-v1 @ dead00beef1dead00beef1dead00beef1dead001"])]
    prs = [_pr(repo, 901, "kvm-winsize-v2",
               "feed00c0ffee2feed00c0ffee2feed00c0ffee23",
               title="unrelated scheduler tweak",
               files=["detcore/src/scheduler.rs"])]
    r = compute_join(tasks, prs)
    check(not _num_in(r["population_A_matched"], repo, 901),
          "renamed neighbour #901 NOT bound into population A")
    check(not _tid_in(r["population_A_matched"], repo, 901, "stale-owner"),
          "neighbour #901 not confidently bound to the stale task")
    check(_num_in(r["population_B_unmentioned"], repo, 901),
          "neighbour #901 reported UNMATCHED (B2 sharpest hazard)")


def test_batch_task_demotion():
    print("test_batch_task_demotion")
    repo = "rrnewton/hermit"
    n = BATCH_FANOUT_THRESHOLD + 2  # strictly over the threshold
    # One drain rollup that names many PRs by url+branch (real identity), plus
    # one dedicated task owning a single PR.
    swept_nums = list(range(10, 10 + n))
    notes = [f"drain: https://github.com/{repo}/pull/{k} branch feature-branch-{k}"
             for k in swept_nums]
    tasks = [
        _task("drain-rollup", title="drain the head PRs", notes=notes),
        _task("solo-owner", title="own one pr",
              notes=[f"Implemented: https://github.com/{repo}/pull/20 branch feature-twenty"]),
    ]
    prs = [_pr(repo, k, f"feature-branch-{k}", f"{k:040x}") for k in swept_nums]
    prs.append(_pr(repo, 20, "feature-twenty", f"{20:040x}"))
    r = compute_join(tasks, prs)
    for k in swept_nums:
        check(not _num_in(r["population_A_matched"], repo, k),
              f"swept PR #{k} (fanout>{BATCH_FANOUT_THRESHOLD}) NOT in A")
    swept_all_in_b1 = all(_num_in(r["population_B_mentioned_only"], repo, k)
                          for k in swept_nums)
    check(swept_all_in_b1, "all swept PRs land in B1 mentioned-only (LOOK)")
    check(_num_in(r["population_A_matched"], repo, 20),
          "the dedicated solo PR #20 IS in population A")


def test_live_population_nonzero_and_strict_subset():
    print("test_live_population_nonzero_and_strict_subset")
    repo = "rrnewton/hermit"
    # A realistic mix: 2 dedicated owners, 1 batch sweep over 4, 1 orphan PR.
    tasks = [
        _task("own-a", notes=[f"https://github.com/{repo}/pull/1 branch feature-alpha"]),
        _task("own-b", notes=[f"https://github.com/{repo}/pull/2 branch feature-beta"]),
        _task("sweep", notes=[f"https://github.com/{repo}/pull/{k} branch feature-branch-{k}"
                              for k in (3, 4, 5, 6)]),
    ]
    prs = [
        _pr(repo, 1, "feature-alpha", f"{1:040x}"),
        _pr(repo, 2, "feature-beta", f"{2:040x}"),
        *[_pr(repo, k, f"feature-branch-{k}", f"{k:040x}") for k in (3, 4, 5, 6)],
        _pr(repo, 99, "orphan-branch-xyz", f"{99:040x}"),  # nothing references it
    ]
    r = compute_join(tasks, prs)
    matched = len(r["population_A_matched"])
    total = r["denominators"]["open_prs"]
    # NONZERO (kills the match-nothing trap) AND STRICT SUBSET (kills match-all).
    check(matched == 2, f"exactly the 2 dedicated PRs matched (got {matched})")
    check(0 < matched < total, f"matched is nonzero strict subset ({matched}/{total})")
    check(_num_in(r["population_B_unmentioned"], repo, 99),
          "true orphan #99 is B2 UNMENTIONED")


def test_exact_record_overrides_dependency_mentions():
    print("test_exact_record_overrides_dependency_mentions")
    repo = "rrnewton/hermit"
    url = f"https://github.com/{repo}/pull/2811"
    tasks = [
        _task(
            "actual-owner",
            notes=[task_pr_record.render("actual-owner", repo, 2811, url, "agent-a")],
        ),
        _task("dependency-a", notes=[f"blocked on {url} branch typed-results"]),
        _task("dependency-b", notes=[f"reviewed {url}; head {'a' * 40}"]),
    ]
    prs = [_pr(repo, 2811, "typed-results", "a" * 40)]
    r = compute_join(tasks, prs)
    match = r["population_A_matched"][0]
    check(len(match["dedicated_confirmed"]) == 1,
          "one exact task/PR record prevents dependency prose becoming ownership")
    check(match["dedicated_confirmed"][0]["task"] == "actual-owner",
          "the recorded task is the sole owner")
    check(match["ownership_source"] == "record",
          "the report names the exact record as its source")


def test_conflicting_exact_records_refuse_ownership():
    print("test_conflicting_exact_records_refuse_ownership")
    repo = "rrnewton/hermit"
    url = f"https://github.com/{repo}/pull/2811"
    tasks = [
        _task(name, notes=[task_pr_record.render(name, repo, 2811, url, name)])
        for name in ("owner-a", "owner-b")
    ]
    r = compute_join(tasks, [_pr(repo, 2811, "typed-results", "a" * 40)])
    check(not r["population_A_matched"],
          "two exact owners do not silently select either task")
    check(r["population_B_mentioned_only"][0]["ownership_conflict"],
          "the task/PR ownership conflict is named")
    check(not r["population_B_mentioned_only"][0]["swept_only"],
          "an exact ownership conflict is not mislabeled as a batch sweep")


def test_malformed_record_cannot_fall_back_to_prose():
    print("test_malformed_record_cannot_fall_back_to_prose")
    repo = "rrnewton/hermit"
    url = f"https://github.com/{repo}/pull/2811"
    malformed = task_pr_record.render(
        "attempted-owner", repo, 2811, url, "agent-a"
    ) + "\nfuture_field: accepted-by-accident"
    tasks = [_task("attempted-owner", notes=[malformed, f"Implemented: {url}"])]
    r = compute_join(tasks, [_pr(repo, 2811, "typed-results", "a" * 40)])
    check(not r["population_A_matched"],
          "malformed current record cannot regain authority through prose")
    check(
        r["invalid_ownership_records"][0]["error"]
        == "unexpected task/PR ownership field(s): future_field",
        "an added record field fails by its exact name",
    )


def main():
    for t in (test_valid_tags_are_preserved,
              test_malformed_tags_refuse_the_join,
              test_non_string_tags_refuse_the_join,
              test_positive_dedicated_match,
              test_negative_closed_renamed_not_bound_to_neighbour,
              test_batch_task_demotion,
              test_live_population_nonzero_and_strict_subset,
              test_exact_record_overrides_dependency_mentions,
              test_conflicting_exact_records_refuse_ownership,
              test_malformed_record_cannot_fall_back_to_prose):
        t()
    print()
    if FAILS:
        print(f"FAILED: {len(FAILS)} check(s)")
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
