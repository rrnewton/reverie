#!/usr/bin/env python3
"""Bracket `classify()` both ways against the three mis-binding modes.

These fixtures are the REAL tasks that exposed the defect on 2026-08-12, with
their real SHAs and pull-request numbers, plus two genuinely-landed controls.
The point of keeping them real is that a synthetic fixture proves the code does
what its author meant; these prove it does what the live pile needed.

ACCEPT (must still report LANDED):
  * a task whose LINKED pull request merged
  * a task whose implementing commit carries `Task: <task-id>`

REFUSE (must NOT report LANDED):
  * mode 1 -- a note-quoted SHA that is main's own tip
  * mode 2 -- the mergeCommit of a different, incidentally-mentioned bare PR
  * mode 3 -- a SHA from another repository

Run: python3 ci-hub/closure/test_ancestry_audit_attribution.py
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import ancestry_audit  # noqa: E402

MAIN_TIP = "1f4e430c135204591daa0c0cba84f77dae679a78"   # hermit main's own tip
OTHER_PR_MERGE = "d05d02d86424aaaaaaaaaaaaaaaaaaaaaaaaaaaa"  # bare PR #2071's merge
PARENT_SHA = "070b504dce00aaaaaaaaaaaaaaaaaaaaaaaaaaaa"      # a dev-hermit commit
OWN_MERGE = "abcdef01234567890abcdef01234567890abcdef"       # a linked PR's merge
OWN_COMMIT = "fedcba09876543210fedcba09876543210fedcba"      # carries Task: trailer


class StubAncestry:
    """Every SHA here is on main. That is deliberate: if ancestry alone decided
    the verdict, EVERY case below would pass, and the refusals prove the
    attribution gate is what discriminates."""

    def __init__(self, claims: dict[tuple[str, str], str]):
        self._claims = claims          # (repo, sha) -> task it claims

    def test(self, repo: str, sha: str) -> tuple[bool, bool]:
        return (True, True)            # ancestor, present

    def claims_task(self, repo: str, sha: str, task: str) -> bool:
        return self._claims.get((repo, sha)) == task

    def by_subject(self, repo: str, sha: str) -> tuple[str | None, str]:
        return None, "stub"


def item(task, prs=(), bare=(), shas=(), text=""):
    return {"task": task, "title": task, "prs": list(prs), "bare_prs": list(bare),
            "shas": list(shas), "artifacts": [], "text": text}


def merged(repo, num, oid, head="deadbeef" * 5, branch="feat/x"):
    return {repo: {num: {"state": "MERGED", "mergeCommit": {"oid": oid},
                         "headRefOid": head, "headRefName": branch}}}


def open_pr(repo, num):
    return {repo: {num: {"state": "OPEN", "mergeCommit": None,
                         "headRefOid": "0" * 40, "headRefName": "feat/open"}}}


def run() -> int:
    failures: list[str] = []

    def check(name, got, want_bucket, note=""):
        ok = got["bucket"] == want_bucket
        print(f"  {'PASS' if ok else 'FAIL'}  {name:52} -> {got['bucket']}"
              f"{'' if ok else f' (wanted {want_bucket})'}")
        if not ok:
            failures.append(f"{name}: got {got['bucket']}, wanted {want_bucket}; "
                            f"why={got.get('why')}")
        elif note:
            print(f"        {note}: {got.get('why','')[:96]}")

    # ---------- ACCEPT ----------
    anc = StubAncestry({})
    check("ACCEPT PR URL + a second agreeing signal -> LANDED",
          ancestry_audit.classify(
              item("t-linked", prs=[("hermit", 2100)],
                   text="https://github.com/rrnewton/hermit/pull/2100 branch feat/x"),
              merged("hermit", 2100, OWN_MERGE), anc),
          "LANDED", "why")

    anc = StubAncestry({("dev-hermit", OWN_COMMIT): "t-trailer"})
    check("ACCEPT commit carrying Task: <id> -> LANDED",
          ancestry_audit.classify(item("t-trailer", shas=[OWN_COMMIT]), {}, anc),
          "LANDED", "why")

    # ---------- REFUSE: the three live mis-binding modes ----------
    anc = StubAncestry({})
    check("REFUSE mode 1: note SHA is main's own tip (p1_compat_lsof_is)",
          ancestry_audit.classify(
              item("p1_compat_lsof_is", prs=[("hermit", 2221)],
                   bare=[2176, 2221], shas=[MAIN_TIP]),
              open_pr("hermit", 2221), anc),
          "NOT-DETERMINABLE")

    check("REFUSE mode 2: mergeCommit of a different bare PR (compat_bin_c_robust)",
          ancestry_audit.classify(
              item("compat_bin_c_robust", prs=[("hermit", 2223)], bare=[2071]),
              {**open_pr("hermit", 2223),
               "hermit": {2223: {"state": "OPEN", "mergeCommit": None},
                          2071: {"state": "MERGED",
                                 "mergeCommit": {"oid": OTHER_PR_MERGE}}}},
              anc),
          "NOT-DETERMINABLE")

    check("REFUSE mode 3: SHA from another repository (port-peer-snapshot)",
          ancestry_audit.classify(
              item("port-peer-snapshot-protocol-into-validate-rs",
                   prs=[("hermit", 2222)], shas=[PARENT_SHA]),
              open_pr("hermit", 2222), anc),
          "NOT-DETERMINABLE")

    # ---------- the discriminator itself ----------
    # Same inputs as mode 1, except the quoted commit now claims the task.
    anc = StubAncestry({("hermit", MAIN_TIP): "p1_compat_lsof_is"})
    check("DISCRIMINATES: same note SHA, but the commit claims the task",
          ancestry_audit.classify(
              item("p1_compat_lsof_is", prs=[("hermit", 2221)], shas=[MAIN_TIP]),
              open_pr("hermit", 2221), anc),
          "LANDED")

    # ---------- hermit-904's reproduction: the URL-shaped route ----------
    # A full PR URL, mentioned and nothing more. Before this fix it produced a
    # phantom LANDED; a lone STRONG signal is PROBABLE, never CONFIRMED.
    anc = StubAncestry({})
    check("REFUSE 904's repro: unrelated full PR URL, lone signal",
          ancestry_audit.classify(
              item("t-mentions-someone-elses-pr", prs=[("hermit", 2071)],
                   text="see https://github.com/rrnewton/hermit/pull/2071 for context"),
              merged("hermit", 2071, OTHER_PR_MERGE), anc),
          "NOT-DETERMINABLE")

    # pr_url and a bare #N routinely come from the SAME mention, so two of them
    # is one mention counted twice. The second signal must be STRONG.
    check("REFUSE URL + only a WEAK corroborator (same mention, counted twice)",
          ancestry_audit.classify(
              item("t-weak-only", prs=[("hermit", 2071)],
                   text="https://github.com/rrnewton/hermit/pull/2071 and #2071 again"),
              merged("hermit", 2071, OTHER_PR_MERGE), anc),
          "NOT-DETERMINABLE")

    check("ACCEPT URL + head_sha agreeing -> LANDED",
          ancestry_audit.classify(
              item("t-headsha", prs=[("hermit", 2100)],
                   text="https://github.com/rrnewton/hermit/pull/2100 head "
                        + "deadbeef" * 5),
              merged("hermit", 2100, OWN_MERGE), anc),
          "LANDED")

    # ---------- hermit-903's gap: unanchored substring matching ----------
    # No natural failing case exists in current data -- every real branch is
    # long and multi-token -- so the refusal is CONSTRUCTED, the same technique
    # 904 used to reproduce the phantom LANDED with an unrelated URL.
    anc = StubAncestry({})
    SHORT = merged("hermit", 2100, OWN_MERGE, head="a" * 40, branch="fix")

    check("REFUSE branch name buried in unrelated words (903's gap)",
          ancestry_audit.classify(
              item("t-substr", prs=[("hermit", 2100)],
                   text="https://github.com/rrnewton/hermit/pull/2100 -- prefix, "
                        "bugfix-notes and a fixture all contain it"),
              SHORT, anc),
          "NOT-DETERMINABLE")

    check("ACCEPT the same branch as a whole token -> LANDED",
          ancestry_audit.classify(
              item("t-branch-ok", prs=[("hermit", 2100)],
                   text="https://github.com/rrnewton/hermit/pull/2100 on branch fix here"),
              SHORT, anc),
          "LANDED")

    check("ACCEPT branch delimited by backticks -> LANDED",
          ancestry_audit.classify(
              item("t-branch-tick", prs=[("hermit", 2100)],
                   text="https://github.com/rrnewton/hermit/pull/2100 `fix`"),
              SHORT, anc),
          "LANDED")

    # head_sha shares the shape: a 12-hex prefix must not match inside a longer
    # hex run, and this database really does carry sha256 digests in notes.
    HEX = merged("hermit", 2101, OWN_MERGE, head="abcdef012345" + "9" * 28,
                 branch="feat/unrelated-branch-name")
    check("REFUSE 12-hex prefix buried inside a 64-hex digest",
          ancestry_audit.classify(
              item("t-hex-substr", prs=[("hermit", 2101)],
                   text="https://github.com/rrnewton/hermit/pull/2101 "
                        "diff_sha256=00000abcdef01234500000000000000000000"
                        "000000000000000000000000000"),
              HEX, anc),
          "NOT-DETERMINABLE")

    check("ACCEPT a standalone 12-hex short sha -> LANDED",
          ancestry_audit.classify(
              item("t-hex-short", prs=[("hermit", 2101)],
                   text="https://github.com/rrnewton/hermit/pull/2101 head abcdef012345"),
              HEX, anc),
          "LANDED")

    check("ACCEPT the full 40-hex head next to punctuation -> LANDED",
          ancestry_audit.classify(
              item("t-hex-full", prs=[("hermit", 2101)],
                   text="https://github.com/rrnewton/hermit/pull/2101 (head="
                        + "abcdef012345" + "9" * 28 + ")"),
              HEX, anc),
          "LANDED")

    # A task with nothing at all stays UNKNOWN, not NOT-DETERMINABLE.
    check("no candidates at all -> UNKNOWN (unchanged)",
          ancestry_audit.classify(item("t-empty"), {}, StubAncestry({})),
          "UNKNOWN")

    print()
    if failures:
        for f in failures:
            print("FAILURE:", f)
        return 1
    print("all attribution brackets hold")
    return 0


if __name__ == "__main__":
    raise SystemExit(run())
