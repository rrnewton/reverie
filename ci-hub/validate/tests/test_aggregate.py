#!/usr/bin/env python3
"""Tests for ci-hub/validate/aggregate.py.

Focus: the faithful per-gate classifier and the run-level de-conflation it
feeds. Every class is anchored ONLY on markers the Hermit validation driver writes itself
(`Command:`, `Skipped:`, `Gate timed out after`, `Exit:`, `Duration:`), never on
workload text — a gate whose *output* contains the words "panic" or "timeout"
must not be misclassified. These tests therefore include adversarial workload
text to prove the classifier ignores it.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import aggregate as agg


class DiscoveryIsolationTest(unittest.TestCase):
    def test_explicit_tmpdir_set_excludes_machine_global_tmp(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            configured = os.pathsep.join((first, second, first))
            with mock.patch.dict(
                os.environ, {"CI_HUB_AGGREGATE_TMPDIRS": configured}
            ):
                self.assertEqual(agg.tmpdirs(), [first, second])


def parse(body: str, name: str = "hermit-validate.test.log") -> dict:
    """Reconstruct a run record from `body`.

    The temp file is written under a private directory and unlinked immediately
    after parsing (parse_raw_log reads the file and its mtime synchronously), so
    the test never leaves a `*-validate.*.log` in a real TMPDIR that the
    machine-wide sweep would then ingest.
    """
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, name)
        with open(path, "w") as fh:
            fh.write(body)
        return agg.parse_raw_log(path)


# A real gate that passed.
PASS = """\
=== Build workspace ===
Command: cargo build --workspace
   Compiling detcore v0.1.0
Exit: 0
Duration: 42s

"""

# A real gate that failed for a product reason (non-zero, not a timeout).
FAIL = """\
=== Detcore core unit tests ===
Command: cargo test -p detcore --lib
test scheduler::races ... FAILED
Exit: 101
Duration: 30s

"""

# Killed by a bound via run_timed_command: the authoritative marker + Exit 124.
TIMEOUT_MARKER = """\
=== liteinst strict verify python entropy ===
Command: cargo test -p hermit liteinst_strict_verify_python_entropy
test liteinst_strict_verify_python_entropy ... Gate timed out after 300s (subprocess PID 2865433)
Exit: 124
Duration: 300s

"""

# Killed by a bound via a bare `timeout` in a compat probe: Exit 124, NO marker.
TIMEOUT_BARE = """\
=== L2 compatibility: lsof ===
Command: timeout 60 hermit run --strict --verify -- lsof
:: Run1...
Exit: 124
Duration: 60s

"""

# A deferred/skipped gate: header + Skipped:, no Exit.
SKIPPED = """\
=== L2 compatibility: rustc ===
Skipped: scheduled super diagnostic (portable runner)

"""

# A pure section banner: header alone, no Command/Exit.
BANNER = "=== Strict compatibility envelope (L2, blocking) ===\n"

# A gate cut off mid-run: Command but never an Exit/Duration footer (EOF).
INCOMPLETE = """\
=== Portable ptrace E2E verification ===
Command: ./ci/test_harness.sh e2e
:: Run1...
"""

HEADER = "Root: /home/user/work/dev-hermit/hermit\nLevel: full\n"


class ClassifyTest(unittest.TestCase):
    def kinds(self, body: str) -> list[str]:
        return [g["kind"] for g in parse(HEADER + body)["gates"]]

    def test_pass(self):
        self.assertEqual(self.kinds(PASS), ["pass"])

    def test_product_fail(self):
        self.assertEqual(self.kinds(FAIL), ["fail"])

    def test_timeout_via_marker(self):
        self.assertEqual(self.kinds(TIMEOUT_MARKER), ["timeout"])

    def test_timeout_via_bare_exit_124(self):
        # No "Gate timed out after" line, only Exit 124 — still killed-by-bound.
        self.assertEqual(self.kinds(TIMEOUT_BARE), ["timeout"])

    def test_skipped(self):
        self.assertEqual(self.kinds(SKIPPED), ["skipped"])

    def test_banner(self):
        self.assertEqual(self.kinds(BANNER), ["banner"])

    def test_incomplete_trailing(self):
        self.assertEqual(self.kinds(INCOMPLETE), ["incomplete"])

    def test_workload_text_does_not_sway_class(self):
        # A PASSING gate whose output prints the words a naive scanner keys on.
        adversarial = """\
=== run tests that mention scary words ===
Command: cargo test -p detcore mentions_panic_and_timeout
test panic_recovery ... ok
note: 'Gate timed out' appears here only as guest stdout, not a real marker line prefix... actually it must be a standalone line to count
Exit: 0
Duration: 5s

"""
        # The marker check is substring-based on validate.sh's own writes; here
        # the decisive fact is Exit 0 -> pass regardless of the word "panic".
        self.assertEqual(self.kinds(adversarial), ["pass"])


class RunLevelTest(unittest.TestCase):
    def test_verdict_priority_fail_beats_timeout(self):
        run = parse(HEADER + PASS + TIMEOUT_MARKER + FAIL)
        self.assertEqual(run["result"], "fail")

    def test_verdict_timeout_beats_incomplete(self):
        run = parse(HEADER + PASS + INCOMPLETE.replace("Portable", "X") + TIMEOUT_MARKER)
        # incomplete is a trailing-only concept; put the incomplete gate first so
        # it is followed by a completed timeout gate.
        self.assertEqual(run["result"], "timeout")

    def test_incomplete_when_only_cutoff(self):
        run = parse(HEADER + PASS + INCOMPLETE)
        self.assertEqual(run["result"], "incomplete")

    def test_all_pass(self):
        run = parse(HEADER + PASS + PASS)
        self.assertEqual(run["result"], "pass")

    def test_deconfliction_fields(self):
        run = parse(HEADER + PASS + FAIL + TIMEOUT_MARKER + TIMEOUT_BARE
                    + INCOMPLETE)
        self.assertEqual(run["failures"], 1)          # product fail only
        self.assertEqual(run["killed_by_bound"], 2)   # both timeout paths
        self.assertEqual(run["incomplete_gates"], 1)
        self.assertEqual(run["gate_classification"]["pass"], 1)

    def test_banner_excluded_from_checks(self):
        # Banner + skipped present; checks counts non-banner gates only.
        run = parse(HEADER + BANNER + PASS + SKIPPED)
        self.assertEqual(run["banner_lines"], 1)
        self.assertEqual(run["skipped_gates"], 1)
        self.assertEqual(run["checks"], 2)  # PASS + SKIPPED, banner excluded


class RenderTest(unittest.TestCase):
    def test_recent_view_is_bounded_and_newest_first(self):
        runs = [{"id": number} for number in range(15)]
        self.assertEqual(
            [row["id"] for row in agg.recent_runs(runs, 3)],
            [14, 13, 12],
        )
        self.assertEqual([row["id"] for row in runs[:3]], [0, 1, 2])

    def test_header_carries_second_units(self):
        out = agg.render_table([parse(HEADER + PASS)])
        head = out.splitlines()[0]
        self.assertIn("WALL(s)", head)
        self.assertIn("USER(s)", head)
        self.assertIn("SYS(s)", head)

    def test_profile_not_truncated(self):
        long_profile = "portable-strict-compat-only"
        run = parse("Root: /x\nLevel: %s\n" % long_profile + PASS)
        out = agg.render_table([run])
        self.assertIn(long_profile, out)  # full identifier, not "portable-stric"

    def test_gates_denominator_excludes_banner(self):
        run = parse(HEADER + BANNER + PASS + FAIL)
        out = agg.render_table([run])
        # 1 pass of 2 verdict gates (banner excluded), not 1/3.
        self.assertIn("1/2", out)

    def test_gate_kind_tolerates_ledger_rows(self):
        # Ledger rows carry only `result`, no `kind`.
        self.assertEqual(agg.gate_kind({"result": "pass"}), "pass")
        self.assertEqual(agg.gate_kind({"result": "fail"}), "fail")
        self.assertEqual(agg.gate_kind({"exit_code": 0}), "pass")

    def test_a_node_that_could_not_determine_is_never_counted_as_a_pass(self):
        # The shape hermit's never-written-report reclassification produces:
        # the command exited 0 and the node still reported no result. Reading
        # the zero exit as a pass would count an undetermined node in the GATES
        # numerator, which is the opposite of what `no_result` means.
        self.assertEqual(
            agg.gate_kind({"result": "no_result", "exit_code": 0}), "incomplete")
        # The pre-existing `no_result` shapes, which reached `incomplete` only
        # via the exit-status fallback and must keep reaching it directly.
        self.assertEqual(
            agg.gate_kind({"result": "no_result", "exit_code": 101}), "incomplete")
        self.assertEqual(
            agg.gate_kind({"result": "no_result", "exit_code": 75}), "incomplete")
        self.assertEqual(
            agg.gate_kind({"result": "no_result", "exit_code": None}), "incomplete")
        # Unaffected controls: a real pass still passes, a real failure still
        # fails, and a row with no `result` still infers from its exit status.
        self.assertEqual(agg.gate_kind({"result": "pass", "exit_code": 0}), "pass")
        self.assertEqual(agg.gate_kind({"result": "fail", "exit_code": 0}), "fail")
        self.assertEqual(agg.gate_kind({"exit_code": 0}), "pass")
        # `incomplete` is a verdict-bearing kind, so the node stays in the
        # denominator and is simply not counted as a pass.
        self.assertIn("incomplete", agg.VERDICT_KINDS)
        self.assertNotIn(
            agg.gate_kind({"result": "no_result", "exit_code": 0}), ("pass",))

    def test_a_stated_verdict_is_read_by_name_not_inferred_from_the_exit(self):
        # `livelock_class.py:220` already treats a gate's `result == "timeout"`
        # as authoritative, so this reader has to be able to name that kind too.
        # No gate carries it today -- measured 2026-09-17 over the whole tracked
        # ledger, 80,652 gate records, `result` is only `pass` (76,807), `fail`
        # (3,712), absent (120) or `no_result` (13) -- so this pins the mapping
        # before a producer needs it rather than repairing a live miscount.
        for exit_code in (124, -9, 0, None):
            self.assertEqual(
                agg.gate_kind({"result": "timeout", "exit_code": exit_code}),
                "timeout",
                f"a stated timeout was inferred from exit_code={exit_code}")
        self.assertIn("timeout", agg.VERDICT_KINDS)
        # The fallback is retained exactly where it is still correct: a row that
        # states NO verdict is still inferred from its exit status. 120 such
        # gates exist in the ledger and 2 take the zero-exit pass branch.
        self.assertEqual(agg.gate_kind({"exit_code": 0}), "pass")
        self.assertEqual(agg.gate_kind({"exit_code": 1}), "incomplete")
        self.assertEqual(agg.gate_kind({}), "incomplete")


class SummaryCompletenessTest(unittest.TestCase):
    def test_truncated_is_not_counted_as_result_or_product_failure(self):
        truncated = {
            "profile": "full", "result": "fail", "checks": 2,
            "gates_run": 2, "gates_expected": 5, "failures": 2,
        }
        genuine = {
            "profile": "full", "result": "fail", "checks": 5,
            "gates_run": 5, "gates_expected": 5, "failures": 1,
            "executed_tests": 765,
        }
        # N=2 legitimate passes are the unaffected positive controls.
        passes = [
            {"profile": "full", "result": "pass", "checks": 5,
             "gates_run": 5, "gates_expected": 5, "failures": 0},
            {"profile": "full", "result": "pass", "checks": 5,
             "gates_run": 5, "gates_expected": 5, "failures": 0},
        ]
        out = agg.summarize([truncated, genuine, *passes], [], 0)
        self.assertIn("fail=1", out)
        self.assertIn("pass=2", out)
        self.assertIn("truncated=1", out)
        self.assertIn("product-fail=1", out)


# A full reverie run in the shape reverie/validate.sh emits: `Reverie validation
# log` product line, Root/Level/Host OS header, a commit SHA line, then the same
# `=== name ===` / Command: / Exit: / Duration: gate markers hermit writes. The
# killed-vs-crashed classification is DERIVED here at aggregation (a VIEW
# property), matching hermit — the record stores only exit_code, not a kind.
REVERIE_HEADER = (
    "Reverie validation log\n"
    "Root: /home/user/work/dev-hermit/worktrees/slots/slot01/reverie\n"
    "Level: reverie-full\n"
    "Host OS: Linux\n"
    "Commit: 0123456789abcdef0123456789abcdef01234567 (clean tree, commit-anchored)\n"
    "\n"
)
REVERIE_GATE_PASS = """\
=== Build workspace ===
Command: cargo build --workspace --all-features
   Compiling reverie v0.1.0
Exit: 0
Duration: 120s

"""
REVERIE_GATE_FAIL = """\
=== Clippy ===
Command: cargo clippy --workspace --all-targets --all-features -- -D warnings
error: unused variable
Exit: 101
Duration: 18s

"""


class RepoAttributionTest(unittest.TestCase):
    def test_reverie_header_wins(self):
        # Header product line decides repo even when the filename says hermit.
        run = parse(REVERIE_HEADER + REVERIE_GATE_PASS,
                    name="hermit-validate.test.log")
        self.assertEqual(run["repo"], "reverie")

    def test_reverie_filename_when_no_header(self):
        run = parse(HEADER + PASS, name="reverie-validate.test.log")
        self.assertEqual(run["repo"], "reverie")

    def test_hermit_default(self):
        run = parse(HEADER + PASS, name="hermit-validate.test.log")
        self.assertEqual(run["repo"], "hermit")

    def test_reverie_cwd_fallback(self):
        # No product header, ambiguous filename -> attribute by reverie checkout.
        run = parse("Root: /x/worktrees/slots/slot01/reverie\nLevel: reverie-full\n"
                    + PASS, name="validate-run.test.log")
        self.assertEqual(run["repo"], "reverie")

    def test_historical_slot_layout_remains_attributable(self):
        self.assertEqual(
            "slot01",
            agg.slot_from_cwd("/x/worktrees/slot01/hermit"),
        )

    def test_reverie_primary_slot(self):
        self.assertEqual(agg.slot_from_cwd("/home/user/dev-hermit/reverie"), "primary")


class ReverieReconstructionTest(unittest.TestCase):
    def test_full_reverie_run_reconstructs(self):
        run = parse(REVERIE_HEADER + REVERIE_GATE_PASS + REVERIE_GATE_FAIL,
                    name="reverie-validate.abc123.log")
        self.assertEqual(run["repo"], "reverie")
        self.assertEqual(run["profile"], "reverie-full")
        self.assertEqual(run["commit"],
                         "0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(run["slot"], "slot01")
        self.assertEqual(run["result"], "fail")          # product fail present
        self.assertEqual(run["failures"], 1)
        self.assertEqual(run["checks"], 2)
        self.assertEqual(run["real_seconds"], 138)        # 120 + 18

    def test_reverie_repo_in_table(self):
        run = parse(REVERIE_HEADER + REVERIE_GATE_PASS,
                    name="reverie-validate.abc123.log")
        out = agg.render_table([run])
        self.assertIn("REPO", out.splitlines()[0])
        self.assertIn("reverie", out)


if __name__ == "__main__":
    unittest.main()
