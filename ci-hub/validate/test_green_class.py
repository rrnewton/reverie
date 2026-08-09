#!/usr/bin/env python3
"""BRACKETS for green_class.py, including the mutation sequence the owner named.

    "VERIFY BY MUTATION: rebase a hard-green PR -> its row becomes SOFT with the
     ancestor recorded. Add a commit -> it becomes NEITHER. A classifier that
     calls everything soft-green is the label-with-no-backing problem again."

That sequence is `MutationSequence` below, run against a real throwaway git
repository so the delta classification is derived from actual history rather than
from hand-written fixture fields. Every gate is bracketed on BOTH sides: the
violating case is planted and refused, and the qualifying case is planted and
fires. A refusal-only suite cannot distinguish a working gate from one that
refuses everything.

No network, no ledger mutation, no validate run.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(os.path.abspath(__file__)).parent
sys.path.insert(0, str(HERE))

import green_class as G  # noqa: E402

PREDICATE = json.loads((HERE / "qualifying-receipt.json").read_text())
A = "a" * 40
B = "b" * 40


def row(**over) -> dict:
    base = {"commit": A, "result": "pass", "profile": "full", "selection_mode": "full"}
    base.update(over)
    return base


def inherited(**over) -> dict:
    base = {
        "delta_kind": G.DELTA_REBASE_ONLY,
        "upstream_commits": 0,
        "branch_commits": 0,
        "patch_identical": True,
        "force_full_paths": [],
        "recorded_by": "test",
    }
    base.update(over)
    return base


class DefaultAndHard(unittest.TestCase):
    """The version-aware default, and that HARD is reachable at all."""

    def test_absent_field_defaults_to_hard(self):
        """Every row written before the field existed must stay green. Defaulting
        the other way is the fleet-wide flag day this contract forbids."""
        klass, reason = G.derive_class(row())
        self.assertEqual(klass, G.HARD)
        self.assertIn("defaulted to commit", reason)

    def test_explicit_same_sha_is_hard(self):
        klass, _ = G.derive_class(row(validated_head_sha=A))
        self.assertEqual(klass, G.HARD)

    def test_explicit_null_is_not_the_legacy_absent_default(self):
        for field in ("validated_head_sha", "inherited_from", "green_class"):
            with self.subTest(field=field):
                klass, reason = G.derive_class(row(**{field: None}))
                self.assertEqual(klass, G.REFUSED)
                self.assertIn("explicitly null", reason)

    def test_hard_is_accepted_for_landing_by_default(self):
        v = G.classify_row(row(), PREDICATE)
        self.assertTrue(v["accepted_for_landing"])
        self.assertEqual(v["accepts_green_class"], ["hard"])


class SoftClasses(unittest.TestCase):
    """Soft is reachable, and the decay rule separates the classes."""

    def test_rebase_only_is_the_strongest_soft(self):
        klass, reason = G.derive_class(
            row(validated_head_sha=B, inherited_from=inherited()))
        self.assertEqual(klass, G.SOFT_REBASE_ONLY)
        self.assertIn(B[:12], reason)  # the ancestor is NAMED in the reason

    def test_upstream_delta_is_weaker(self):
        klass, _ = G.derive_class(row(
            validated_head_sha=B,
            inherited_from=inherited(delta_kind=G.DELTA_REBASE_PLUS_UPSTREAM,
                                     upstream_commits=7)))
        self.assertEqual(klass, G.SOFT_UPSTREAM_DELTA)

    def test_force_full_path_downgrades_below_plain_upstream_delta(self):
        """The derived boundary: a pulled-in force_full path means whole-suite
        blast radius, so the ancestor's green covers none of it."""
        klass, reason = G.derive_class(row(
            validated_head_sha=B,
            inherited_from=inherited(delta_kind=G.DELTA_REBASE_PLUS_UPSTREAM,
                                     upstream_commits=3,
                                     force_full_paths=["ci/run-node.sh"])))
        self.assertEqual(klass, G.SOFT_FORCE_FULL)
        self.assertIn("ci/run-node.sh", reason)

    def test_no_soft_class_is_accepted_for_landing_by_default(self):
        for kind, extra in (
            (G.DELTA_REBASE_ONLY, {}),
            (G.DELTA_REBASE_PLUS_UPSTREAM, {"upstream_commits": 2}),
        ):
            v = G.classify_row(
                row(validated_head_sha=B, inherited_from=inherited(delta_kind=kind, **extra)),
                PREDICATE)
            self.assertFalse(v["accepted_for_landing"], f"{kind} must not land by default")


class Refusals(unittest.TestCase):
    """Every way a row can claim more than its provenance supports."""

    def assert_refused(self, r, fragment):
        klass, reason = G.derive_class(r)
        self.assertEqual(klass, G.REFUSED, f"expected refusal, got {klass}: {reason}")
        self.assertIn(fragment, reason)

    def test_soft_without_provenance_is_refused(self):
        """The core fake-green shape: a different validated head with nothing to
        justify carrying the green forward."""
        self.assert_refused(row(validated_head_sha=B), "NO inherited_from")

    def test_inheritance_claimed_at_the_same_head_is_refused(self):
        self.assert_refused(row(validated_head_sha=A, inherited_from=inherited()),
                            "contradiction")

    def test_label_disagreeing_with_provenance_is_refused(self):
        """The label is a cache. A writer stamping green_class=hard on a carried
        row is the original defect one level up."""
        self.assert_refused(
            row(validated_head_sha=B, inherited_from=inherited(), green_class=G.HARD),
            "disagrees with the class derived from provenance")

    def test_label_agreeing_with_provenance_is_allowed(self):
        """Positive side: the cache is permitted when it is correct — otherwise
        the rule would just be 'never write the label', which is a different
        design and would make the field useless for grepping."""
        klass, _ = G.derive_class(row(validated_head_sha=B,
                                      inherited_from=inherited(),
                                      green_class=G.SOFT_REBASE_ONLY))
        self.assertEqual(klass, G.SOFT_REBASE_ONLY)

    def test_unknown_delta_kind_is_refused(self):
        self.assert_refused(
            row(validated_head_sha=B, inherited_from=inherited(delta_kind="probably-fine")),
            "not one of")

    def test_rebase_only_contradicting_upstream_count_is_refused(self):
        self.assert_refused(
            row(validated_head_sha=B, inherited_from=inherited(upstream_commits=4)),
            "contradicts upstream_commits")

    def test_upstream_delta_without_a_count_is_refused(self):
        self.assert_refused(
            row(validated_head_sha=B,
                inherited_from=inherited(delta_kind=G.DELTA_REBASE_PLUS_UPSTREAM,
                                         upstream_commits=0)),
            "requires a positive upstream_commits")

    def test_missing_commit_is_refused(self):
        self.assert_refused({"result": "pass"}, "no commit")


class NeitherOnNewCommits(unittest.TestCase):
    """Owner rule: add a commit and it is NEITHER hard nor soft."""

    def test_new_branch_commits_is_not_green(self):
        klass, reason = G.derive_class(row(
            validated_head_sha=B,
            inherited_from=inherited(delta_kind=G.DELTA_NEW_BRANCH_COMMITS,
                                     branch_commits=1)))
        self.assertEqual(klass, G.NOT_GREEN)
        self.assertIn("gained 1 commit", reason)

    def test_branch_commits_override_a_rebase_only_claim(self):
        """A producer claiming rebase-only while reporting branch commits does not
        get the strong class — the counts win over the label."""
        klass, _ = G.derive_class(row(
            validated_head_sha=B,
            inherited_from=inherited(branch_commits=2)))
        self.assertEqual(klass, G.NOT_GREEN)

    def test_not_green_is_not_accepted_for_landing(self):
        v = G.classify_row(
            row(validated_head_sha=B,
                inherited_from=inherited(delta_kind=G.DELTA_NEW_BRANCH_COMMITS,
                                         branch_commits=1)),
            PREDICATE)
        self.assertFalse(v["accepted_for_landing"])


class AcceptancePolicy(unittest.TestCase):
    def test_widening_the_policy_admits_the_named_class_only(self):
        widened = dict(PREDICATE, accepts_green_class=[G.HARD, G.SOFT_REBASE_ONLY])
        soft_ok = G.classify_row(
            row(validated_head_sha=B, inherited_from=inherited()), widened)
        soft_weak = G.classify_row(
            row(validated_head_sha=B,
                inherited_from=inherited(delta_kind=G.DELTA_REBASE_PLUS_UPSTREAM,
                                         upstream_commits=1)), widened)
        self.assertTrue(soft_ok["accepted_for_landing"])
        self.assertFalse(soft_weak["accepted_for_landing"],
                         "widening to rebase-only must NOT admit upstream-delta")

    def test_malformed_policy_is_refused(self):
        with self.assertRaises(G.Refusal):
            G.accepted_classes({"accepts_green_class": "hard"})


class MutationSequence(unittest.TestCase):
    """THE OWNER'S MUTATION, on a real repository.

    hard-green PR  --rebase-->  SOFT with the ancestor recorded
                   --+commit-->  NEITHER
    """

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.repo = Path(cls.tmp.name) / "repo"
        cls.repo.mkdir()
        cls.git("init", "-q", "-b", "main")
        cls.git("config", "user.email", "t@t")
        cls.git("config", "user.name", "t")
        cls.write_commit("base.txt", "base", "base")
        cls.old_base = cls.head()

        # The PR branch: one commit on top of old_base.
        cls.git("checkout", "-q", "-b", "pr")
        cls.write_commit("feature.txt", "feature", "add feature")
        cls.ancestor = cls.head()          # the head that was HARD green

        # main moves on: two upstream commits, one of them force_full-class.
        cls.git("checkout", "-q", "main")
        cls.write_commit("upstream.txt", "u1", "upstream one")
        cls.write_commit("ci/run-node.sh", "#!/bin/sh\n", "upstream ci change")
        cls.new_base = cls.head()

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    @classmethod
    def git(cls, *args):
        return subprocess.run(["git", "-C", str(cls.repo), *args],
                              capture_output=True, text=True, check=False)

    @classmethod
    def write_commit(cls, rel, text, msg):
        path = cls.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        cls.git("add", rel)
        cls.git("commit", "-q", "-m", msg)

    @classmethod
    def head(cls):
        return cls.git("rev-parse", "HEAD").stdout.strip()

    def rebase_pr_onto(self, base):
        self.git("checkout", "-q", "pr")
        res = self.git("rebase", base)
        self.assertEqual(res.returncode, 0, f"rebase failed: {res.stderr}")
        return self.head()

    def test_step1_before_rebase_the_row_is_hard(self):
        r = row(commit=self.ancestor)
        self.assertEqual(G.derive_class(r)[0], G.HARD)

    def test_step2_rebase_makes_it_soft_with_the_ancestor_recorded(self):
        head = self.rebase_pr_onto("main")
        self.assertNotEqual(head, self.ancestor, "rebase must move the head")
        delta = G.classify_delta(self.repo, self.ancestor, head,
                                 self.old_base, self.new_base)
        self.assertEqual(delta["delta_kind"], G.DELTA_REBASE_PLUS_UPSTREAM)
        self.assertEqual(delta["branch_commits"], 0, "no new branch work")
        self.assertEqual(delta["upstream_commits"], 2)
        self.assertEqual(delta["force_full_paths"], ["ci/run-node.sh"])

        r = row(commit=head, validated_head_sha=self.ancestor, inherited_from=delta)
        klass, reason = G.derive_class(r)
        # Soft, and specifically the downgraded class: the upstream delta touched
        # a force_full path, so this is the weakest soft, not the strongest.
        self.assertEqual(klass, G.SOFT_FORCE_FULL)
        self.assertIn("ci/run-node.sh", reason)
        # The ancestor it came from is recorded on the row, per the owner's ask.
        self.assertEqual(r["validated_head_sha"], self.ancestor)
        self.git("reset", "-q", "--hard", self.ancestor)

    def test_step3_adding_a_commit_makes_it_neither(self):
        head = self.rebase_pr_onto("main")
        self.write_commit("extra.txt", "extra", "new work after the green")
        head2 = self.head()
        delta = G.classify_delta(self.repo, self.ancestor, head2,
                                 self.old_base, self.new_base)
        self.assertEqual(delta["delta_kind"], G.DELTA_NEW_BRANCH_COMMITS)
        self.assertEqual(delta["branch_commits"], 1)

        klass, _ = G.derive_class(
            row(commit=head2, validated_head_sha=self.ancestor, inherited_from=delta))
        self.assertEqual(klass, G.NOT_GREEN)
        self.git("reset", "-q", "--hard", head)
        self.git("reset", "-q", "--hard", self.ancestor)

    def test_a_pure_rebase_with_no_upstream_movement_is_the_strong_class(self):
        """Anti-vacuity for the decay rule: the classifier must be CAPABLE of
        returning the strongest soft class, or 'everything is weak' would pass
        every downgrade test for the wrong reason."""
        self.git("checkout", "-q", "-B", "pr2", self.old_base)
        self.write_commit("f2.txt", "f2", "feature two")
        ancestor2 = self.head()
        # Rebase onto the SAME base with --force-rebase: the commit is REWRITTEN
        # (new SHA) while its patch and its base are unchanged. That is the real
        # shape of the strongest soft class — the head moved, nothing else did.
        res = self.git("rebase", "--force-rebase", self.old_base)
        self.assertEqual(res.returncode, 0)
        rewritten = self.head()
        self.assertNotEqual(rewritten, ancestor2, "the head must actually move")
        delta = G.classify_delta(self.repo, ancestor2, rewritten,
                                 self.old_base, self.old_base)
        self.assertEqual(delta["delta_kind"], G.DELTA_REBASE_ONLY)
        self.assertEqual(delta["upstream_commits"], 0)
        klass, _ = G.derive_class(
            row(commit=rewritten, validated_head_sha=ancestor2, inherited_from=delta))
        self.assertEqual(klass, G.SOFT_REBASE_ONLY)

    def test_classify_delta_refuses_an_absent_commit(self):
        with self.assertRaises(G.Refusal):
            G.classify_delta(self.repo, "9" * 40, self.ancestor,
                             self.old_base, self.new_base)


class ForceFullDetection(unittest.TestCase):
    def test_detects_each_force_full_class_prefix(self):
        hits = G.force_full_paths_in(
            ["Cargo.lock", "ci/x.sh", "validate.sh", "scripts/validate.rs",
             ".github/workflows/ci.yml",
             "detcore/src/scheduler.rs", "README.md"])
        self.assertEqual(hits, [".github/workflows/ci.yml", "Cargo.lock",
                                "ci/x.sh", "scripts/validate.rs", "validate.sh"])

    def test_ordinary_product_paths_are_not_force_full(self):
        self.assertEqual(G.force_full_paths_in(["detcore/src/x.rs", "docs/a.md"]), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
