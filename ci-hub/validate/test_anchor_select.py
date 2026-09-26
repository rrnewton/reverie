#!/usr/bin/env python3
"""BRACKETS for anchor_select.py -- both sides, with counts.

Every gate is bracketed twice: the violating case is PLANTED and refused, and the
qualifying case is PLANTED and fires. A refusal-only test cannot distinguish a
working gate from an inert one that refuses everything.

No network, no validate run, no ledger mutation: each test builds its own
throwaway ledger and a throwaway git repository in a temp dir.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(os.path.abspath(__file__)).parent
sys.path.insert(0, str(HERE))

import anchor_select as A  # noqa: E402

PREDICATE = HERE / "qualifying-receipt.json"


class FloorAuthorityBrackets(unittest.TestCase):
    def test_missing_floor_authority_is_indeterminate_not_clear(self):
        with mock.patch.object(A, "GATE_FLOORS", Path("/definitely/absent/gate_floors.py")):
            self.assertIsNone(A.clears_floors(Path("/tmp"), "a" * 40))

    def test_floor_authority_exit_zero_still_clears(self):
        completed = subprocess.CompletedProcess(["gate_floors"], 0, "{}", "")
        with mock.patch.object(A, "GATE_FLOORS", Path(__file__)), \
             mock.patch.object(A.subprocess, "run", return_value=completed):
            self.assertIs(A.clears_floors(Path("/tmp"), "a" * 40), True)

    def test_floor_authority_refusal_stays_a_refusal(self):
        completed = subprocess.CompletedProcess(["gate_floors"], 2, "", "refused")
        with mock.patch.object(A, "GATE_FLOORS", Path(__file__)), \
             mock.patch.object(A.subprocess, "run", return_value=completed):
            self.assertIs(A.clears_floors(Path("/tmp"), "a" * 40), False)


def git_output(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


#: A real Hermit commit whose producer files seed the fixture repos. No
#: registered producer map exists any more
#: (https://github.com/rrnewton/dev-hermit/issues/414).
PRODUCER_SEED_SHA = "ce1aca3aba3a4bb1db244e1ca2ab6903ea4bb7a8"


def seed_producer_files(path: Path) -> None:
    """Materialize a real Hermit commit's producer files in a fixture repo."""
    source = HERE.parents[1] / "hermit"
    definition = A.qualifying_receipt.observe_producer_definition(
        {}, PRODUCER_SEED_SHA, repositories=[source]
    )["definition"]
    for relative, expected_blob in definition.items():
        content = subprocess.run(
            ["git", "-C", str(source), "cat-file", "blob", expected_blob],
            capture_output=True,
            check=True,
        ).stdout
        destination = path / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)


def single_commit_repo(
    path: Path, name: str, *, producer_definition: bool = False
) -> tuple[Path, str]:
    """Create a real commit whose SHA/tree may be claimed as fixture evidence."""
    path.mkdir()
    git_output(path, "init", "-q", "-b", "main")
    git_output(path, "config", "user.email", "anchor-fixture@example.invalid")
    git_output(path, "config", "user.name", "anchor-fixture")
    if producer_definition:
        seed_producer_files(path)
    (path / f"{name}.txt").write_text(f"{name}\n")
    git_output(path, "add", "-A")
    git_output(path, "commit", "-q", "-m", name)
    return path, git_output(path, "rev-parse", "HEAD")


def base_evidence(
    hermit: Path, base: str, reverie: Path, reverie_base: str
) -> dict[str, str]:
    """Build exact receipt provenance from objects that exist in fixture repos."""
    return {
        "base_sha": base,
        "base_tree": git_output(hermit, "rev-parse", f"{base}^{{tree}}"),
        "reverie_base_sha": reverie_base,
        "reverie_base_tree": git_output(
            reverie, "rev-parse", f"{reverie_base}^{{tree}}"
        ),
    }


def _apply_case(row: dict, case: dict) -> dict:
    """Apply a release-payload-cases.json case: drop the pair and every absent
    key, then set exactly the case's fields."""
    for key in ("release_builder", "e2e_payload", *case.get("absent", ())):
        row.pop(key, None)
    row.update(case["fields"])
    return row


def qualifying_row(commit: str, *, evidence: dict[str, str], **over) -> dict:
    """A receipt that satisfies every clause of the shared predicate."""
    row = {
        "commit": commit,
        "commit_anchored": True,
        "tree_dirty": False,
        "profile": "full",
        "selection_mode": "full",
        "result": "pass",
        "failures": 0,
        "checks": 1,
        "gates_run": 1,
        "gates_expected": 1,
        "gates": [{"name": "test", "result": "pass", "exit_code": 0}],
        "executed_tests": 412,
        "passed_tests": 412,
        "schema_version": 5,
        "finished_at": "2026-08-05T00:00:00Z",
        "real_seconds": 528,
        "host": "testbox",
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "coverage": {"planned_test_nodes": 19, "zero_executed_nodes": [], "absent_nodes": []},
    }
    row.update(evidence)
    row.update(over)
    return row


class PredicateBrackets(unittest.TestCase):
    """Boundary 1: which receipts may be an anchor at all."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        root = Path(cls.tmp.name)
        cls.repo, cls.commit = single_commit_repo(
            root / "hermit", "anchor", producer_definition=True
        )
        cls.reverie, cls.reverie_commit = single_commit_repo(
            root / "reverie", "reverie"
        )
        cls.evidence = base_evidence(
            cls.repo, cls.commit, cls.reverie, cls.reverie_commit
        )

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        self.predicate = A.load_predicate(PREDICATE)

    def row(self, **over):
        return qualifying_row(self.commit, evidence=self.evidence, **over)

    def assert_refused(self, row, fragment):
        ok, reason = A.row_qualifies(row, self.predicate)
        self.assertFalse(ok, f"expected refusal, got qualify (reason={reason})")
        self.assertIn(fragment, reason)

    # --- POSITIVE bracket: the gate is not inert -------------------------------
    def test_qualifying_receipt_accepted(self):
        ok, reason = A.row_qualifies(self.row(), self.predicate)
        self.assertTrue(ok, f"a fully-qualifying receipt was refused: {reason}")

    def test_schema5_receipt_without_base_sha_refused(self):
        row = self.row()
        del row["base_sha"]
        self.assert_refused(row, "base-sha-missing")

    # --- NEGATIVE brackets, one per clause ------------------------------------
    def test_selective_receipt_refused(self):
        """THE 1-HOP RULE. An incremental receipt is never an anchor, so no chain
        of incrementals can form. This is the load-bearing clause: if it is ever
        relaxed, green-inheritance silently becomes N-hop."""
        self.assert_refused(self.row(selection_mode="selective"), "1-hop")

    def test_only_mode_receipt_refused(self):
        self.assert_refused(self.row(selection_mode="only"), "1-hop")

    def test_buck_release_payload_receipt_refused(self):
        """A Buck-mode green ran the release payload, without debug assertions
        or overflow checks; it is supplemental evidence, not an anchor."""
        cases = json.loads(
            (HERE / "tests" / "fixtures" / "release-payload-cases.json").read_text()
        )["cases"]
        self.assertEqual(7, sum(case["expected"] is not None for case in cases))
        for case in cases:
            with self.subTest(case=case["label"]):
                row = _apply_case(self.row(), case)
                if case["expected"] == "cargo":
                    ok, reason = A.row_qualifies(row, self.predicate)
                    self.assertTrue(ok, f"a Cargo receipt was refused: {reason}")
                else:
                    self.assert_refused(row, f"release_builder={row.get('release_builder')!r}")

    def test_compat_only_profile_refused(self):
        """The owner's named hazard: ~164 portable-strict-compat-only rows in the
        live ledger say result=pass. Keying on result would anchor on them."""
        self.assert_refused(
            self.row(profile="portable-strict-compat-only", executed_tests=2),
            "profile",
        )

    def test_dirty_tree_refused(self):
        self.assert_refused(self.row(tree_dirty=True), "tree_dirty")

    def test_unanchored_refused(self):
        self.assert_refused(self.row(commit_anchored=False), "commit_anchored")

    def test_zero_executed_refused(self):
        self.assert_refused(self.row(executed_tests=0), "executed_tests==0")

    def test_count_capable_without_coverage_refused(self):
        row = self.row()
        del row["coverage"]
        self.assert_refused(row, "coverage")

    def test_count_capable_with_absent_nodes_refused(self):
        row = self.row()
        row["coverage"] = {"planned_test_nodes": 19, "zero_executed_nodes": [],
                           "absent_nodes": ["e2e.manifest_backend_parity_c"]}
        self.assert_refused(row, "coverage")

    def test_failing_receipt_refused(self):
        self.assert_refused(self.row(result="fail", failures=3), "result")


class GitRepoBrackets(unittest.TestCase):
    """Boundary 3: ancestry. Built on a real throwaway git repository."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.repo = Path(cls.tmp.name) / "repo"
        cls.repo.mkdir()
        cls.git("init", "-q", "-b", "main")
        cls.git("config", "user.email", "t@t")
        cls.git("config", "user.name", "t")
        seed_producer_files(cls.repo)
        cls.shas = {}
        for name in ("base", "mid", "tip"):
            (cls.repo / f"{name}.txt").write_text(name)
            cls.git("add", "-A")
            cls.git("commit", "-q", "-m", name)
            cls.shas[name] = cls.git("rev-parse", "HEAD").stdout.strip()
        # A sibling line that is NOT an ancestor of tip.
        cls.git("checkout", "-q", "-b", "side", cls.shas["base"])
        (cls.repo / "side.txt").write_text("side")
        cls.git("add", "side.txt")
        cls.git("commit", "-q", "-m", "side")
        cls.shas["side"] = cls.git("rev-parse", "HEAD").stdout.strip()
        cls.git("checkout", "-q", "main")
        cls.reverie, cls.reverie_commit = single_commit_repo(
            Path(cls.tmp.name) / "reverie", "reverie"
        )
        cls.foreign_repo, cls.foreign_commit = single_commit_repo(
            Path(cls.tmp.name) / "foreign-hermit",
            "foreign",
            producer_definition=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    @classmethod
    def git(cls, *args):
        return subprocess.run(["git", "-C", str(cls.repo), *args],
                              capture_output=True, text=True, check=False)

    def write_ledger(self, rows) -> Path:
        path = Path(self.tmp.name) / "ledger.jsonl"
        path.write_text("".join(json.dumps(r) + "\n" for r in rows))
        return path

    def row(self, commit: str, *, repo: Path | None = None, **over):
        source = repo or self.repo
        evidence = base_evidence(
            source, commit, self.reverie, self.reverie_commit
        )
        return qualifying_row(commit, evidence=evidence, **over)

    def pick(self, rows, target="tip"):
        return A.select_anchor(
            checkout=self.repo,
            ledger_path=self.write_ledger(rows),
            predicate_path=PREDICATE,
            target_ref=self.shas[target],
            apply_floor=False,
        )

    # --- POSITIVE: an ancestor anchor is found and is the NEAREST one ----------
    def test_nearest_ancestor_wins(self):
        report = self.pick([self.row(self.shas["base"]),
                            self.row(self.shas["mid"])])
        self.assertEqual(report["anchor"]["sha"], self.shas["mid"])
        self.assertEqual(report["anchor"]["distance_commits"], 1)
        self.assertEqual(report["anchor"]["hop"], 1)
        self.assertEqual(report["eligible_anchors"], 2)

    def test_farther_ancestor_used_when_it_is_the_only_one(self):
        report = self.pick([self.row(self.shas["base"])])
        self.assertEqual(report["anchor"]["sha"], self.shas["base"])
        self.assertEqual(report["anchor"]["distance_commits"], 2)

    # --- NEGATIVE: a non-ancestor green is refused ----------------------------
    def test_non_ancestor_green_refused(self):
        """A green on a sibling line is NOT an anchor. select-tests.rs uses a
        three-dot diff, which would silently relocate the anchor to
        merge-base(side, tip) = base -- a commit with no receipt."""
        report = self.pick([self.row(self.shas["side"])])
        self.assertEqual(report["verdict"], A.VERDICT_NO_ANCHOR)
        self.assertEqual(report["candidates_non_ancestor"], 1)

    def test_absent_commit_refused(self):
        report = self.pick([
            self.row(
                self.foreign_commit,
                repo=self.foreign_repo,
                cwd=str(self.foreign_repo),
            )
        ])
        self.assertEqual(report["verdict"], A.VERDICT_NO_ANCHOR)
        self.assertEqual(report["candidates_not_present_locally"], 1)

    def test_no_qualifying_receipt_means_no_anchor(self):
        report = self.pick([self.row(self.shas["mid"], selection_mode="selective"),
                            self.row(self.shas["base"], profile="portable-strict-compat-only")])
        self.assertEqual(report["verdict"], A.VERDICT_NO_ANCHOR)
        self.assertEqual(report["qualifying_receipts"], 0)

    def test_max_scan_bounds_the_search(self):
        report = self.pick([self.row(self.shas["base"])])
        self.assertEqual(report["anchor"]["distance_commits"], 2)
        bounded = A.select_anchor(
            checkout=self.repo,
            ledger_path=self.write_ledger([self.row(self.shas["base"])]),
            predicate_path=PREDICATE,
            target_ref=self.shas["tip"],
            apply_floor=False,
            max_scan=1,
        )
        self.assertEqual(bounded["verdict"], A.VERDICT_NO_ANCHOR)

    # --- the diff the selection runs on ---------------------------------------
    def test_two_dot_diff_against_verified_ancestor(self):
        files = A.changed_files(self.repo, self.shas["base"], self.shas["tip"], False)
        self.assertEqual(files, ["mid.txt", "tip.txt"])

    def test_two_dot_diff_against_non_ancestor_is_wider_not_narrower(self):
        """Fail-SAFE direction check. Three-dot against `side` would report only
        tip's own files (relocating to the merge-base); two-dot additionally
        reports side.txt as removed, i.e. MORE to test, never less."""
        two = set(A.changed_files(self.repo, self.shas["side"], self.shas["tip"], False))
        three = subprocess.run(
            ["git", "-C", str(self.repo), "diff", "--name-only",
             f"{self.shas['side']}...{self.shas['tip']}"],
            capture_output=True, text=True, check=False).stdout.split()
        self.assertTrue(set(three).issubset(two))
        self.assertIn("side.txt", two)
        self.assertNotIn("side.txt", set(three))


class DecayCurveShape(unittest.TestCase):
    """The curve walks first-parent and reports one point per distance. A fake
    selector keeps this hermetic (no hermit checkout, no rust-script)."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        for args in (("init", "-q", "-b", "main"), ("config", "user.email", "t@t"),
                     ("config", "user.name", "t")):
            subprocess.run(["git", "-C", str(self.repo), *args], check=True,
                           capture_output=True)
        self.shas = []
        for name in ("c1", "c2", "c3"):
            (self.repo / f"{name}.txt").write_text(name)
            subprocess.run(["git", "-C", str(self.repo), "add", f"{name}.txt"],
                           check=True, capture_output=True)
            subprocess.run(["git", "-C", str(self.repo), "commit", "-q", "-m", name],
                           check=True, capture_output=True)
            self.shas.append(subprocess.run(
                ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
                capture_output=True, text=True, check=True).stdout.strip())
        self._real_selector = A.run_selector
        self._real_universe = A.universe

    def tearDown(self):
        A.run_selector = self._real_selector
        A.universe = self._real_universe
        self.tmp.cleanup()

    def install_fake(self, decide):
        A.universe = lambda selector, checkout: {"nodes": 10, "cells": 20, "shards": 4}
        A.run_selector = lambda selector, checkout, files: decide(files)

    def test_cliff_is_reported_at_the_first_full(self):
        def decide(files):
            if "c3.txt" in files:
                return {"decision": "full", "node_count": 10, "cell_count": 20,
                        "reasons": ["c3.txt → force_full"]}
            return {"decision": "selective", "node_count": 3, "cell_count": 5, "reasons": []}
        self.install_fake(decide)
        curve = A.decay_curve(checkout=self.repo, selector=Path("unused"),
                              anchor=self.shas[0], target=self.shas[2])
        self.assertEqual(curve["commits"], 2)
        self.assertEqual([p["distance_commits"] for p in curve["points"]], [1, 2])
        self.assertEqual(curve["points"][0]["decision"], "selective")
        self.assertEqual(curve["points"][0]["node_fraction"], 0.3)
        self.assertEqual(curve["first_full_distance"]["distance_commits"], 2)
        self.assertIn("force_full", curve["first_full_distance"]["cause"])

    def test_no_cliff_when_nothing_forces_full(self):
        self.install_fake(lambda files: {"decision": "selective", "node_count": 3,
                                         "cell_count": 5, "reasons": []})
        curve = A.decay_curve(checkout=self.repo, selector=Path("unused"),
                              anchor=self.shas[0], target=self.shas[2])
        self.assertIsNone(curve["first_full_distance"])

    def test_wall_fraction_is_never_fabricated(self):
        self.install_fake(lambda files: {"decision": "selective", "node_count": 3,
                                         "cell_count": 5, "reasons": []})
        curve = A.decay_curve(checkout=self.repo, selector=Path("unused"),
                              anchor=self.shas[0], target=self.shas[2])
        self.assertTrue(all(p["wall_fraction"] is None for p in curve["points"]))
        self.assertIn("per-node duration", curve["wall_fraction_blocked_on"])


class ReanchorCauseNaming(unittest.TestCase):
    """The cause is read from the SELECTOR's reasons, never from a second copy of
    the force_full policy (a mirrored copy misnamed the cause on its first live
    run -- see the note in anchor_select.py)."""

    def test_names_the_force_full_reason(self):
        self.assertEqual(
            A.name_reanchor_cause(["detcore/src/x.rs -> footprint", "ci/run-node.sh → force_full"]),
            "ci/run-node.sh → force_full")

    def test_falls_back_to_the_unmapped_reason(self):
        self.assertIn("unmapped", A.name_reanchor_cause(
            ["2 unmapped path(s) (e.g. new/thing.rs) → conservative full suite"]))

    def test_returns_none_without_a_force_full_or_unmapped_reason(self):
        self.assertIsNone(A.name_reanchor_cause(["no changed-file information available"]))

    def test_returns_none_on_empty_reasons(self):
        self.assertIsNone(A.name_reanchor_cause([]))


if __name__ == "__main__":
    unittest.main(verbosity=2)
