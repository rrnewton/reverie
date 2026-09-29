#!/usr/bin/env python3
"""An unreadable producer definition is a READER fault, never an evidence verdict.

Measured 2026-08-11 on devbig014: an actor holding the identical 1423-row shard,
with both ledger rows for the SHA present, reported `NOT-VALIDATED, 1
non-qualifying` purely because `hermit/` was an uninitialized submodule.
Initializing the submodule flipped the same receipt to VALIDATED with no other
change. The directory EXISTED, so it passed the `is_dir()` candidate filter,
failed resolution with rc 1, and was reported as unresolvable evidence.

That collapse is the defect: "the receipt does not qualify" blames the fleet's
evidence, while "I cannot read the definition" blames this checkout. An actor in
that state silently disbelieves every valid receipt in the fleet and never
suspects itself.

Bracketed here:
  - reader incomplete  -> ProducerDefinitionUndereferenceable
  - the merge-boundary consumer types a reader fault (rc 3) differently from a
    coherent receipt refusal (rc 1)
"""

from pathlib import Path
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]

sys.path.insert(0, str(ROOT / "ci-hub"))
import qualifying_receipt as qr  # noqa: E402


SHA = "a" * 40


def _git(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )


def _qualifying_row() -> dict[str, object]:
    return {
        "schema_version": 5,
        "profile": "full",
        "selection_mode": "full",
        "commit": SHA,
        "commit_anchored": True,
        "tree_dirty": False,
        "result": "pass",
        "failures": 0,
        "executed_tests": 427,
        "passed_tests": 427,
        "filtered_tests": 0,
        "checks": 1,
        "gates_run": 1,
        "gates_expected": 1,
        "gates": [{"name": "test", "result": "pass", "exit_code": 0}],
        "coverage": {
            "planned_test_nodes": 19,
            "executed_test_nodes": 19,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "base_sha": "b" * 40,
        "base_tree": "c" * 40,
        "reverie_base_sha": "d" * 40,
        "reverie_base_tree": "e" * 40,
    }


def _boundary_consumer(fake_bin: Path, mode: str) -> subprocess.CompletedProcess[str]:
    env = {
        **os.environ,
        "FAKE_BOUNDARY_MODE": mode,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    return subprocess.run(
        [
            str(ROOT / "ci-hub" / "ci-hub"),
            "receipt-digest",
            "--sha",
            SHA,
            "--require-qualifying",
            "--current-base",
            "b" * 40,
            "--current-reverie-base",
            "d" * 40,
            "--repo-checkout",
            str(fake_bin),
            "--reverie-checkout",
            str(fake_bin),
        ],
        input=json.dumps(_qualifying_row()),
        capture_output=True,
        text=True,
        check=False,
        env=env,
        timeout=600,
    )


class UndereferenceableProducerDefinitionTest(unittest.TestCase):
    def test_empty_submodule_directory_is_a_reader_fault_not_a_verdict(self) -> None:
        """The exact observed shape: a real directory with no object store."""
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "hermit"
            empty.mkdir()
            self.assertTrue(empty.is_dir(), "fixture must reproduce is_dir()==True")
            with self.assertRaises(qr.ProducerDefinitionUndereferenceable) as caught:
                qr.observe_producer_definition({}, SHA, repositories=[empty])
            detail = str(caught.exception)
            # The message must accuse the checkout, not the receipt, and must
            # say the receipt was never judged.
            self.assertIn("THIS CHECKOUT", detail)
            self.assertIn("has not been judged", detail)
            self.assertIn("uninitialized submodule", detail)

    def test_reader_fault_is_a_subtype_so_existing_consumers_still_fail_closed(
        self,
    ) -> None:
        """Consumers that already catch ProducerDefinitionError keep working."""
        self.assertTrue(
            issubclass(
                qr.ProducerDefinitionUndereferenceable, qr.ProducerDefinitionError
            )
        )

    def test_real_repo_without_the_commit_is_also_a_reader_fault(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            repo.mkdir()
            _git("init", "-q", ".", cwd=repo)
            with self.assertRaises(qr.ProducerDefinitionUndereferenceable):
                qr.observe_producer_definition({}, SHA, repositories=[repo])

    def test_rust_consumer_types_rc3_and_coherent_rc1_differently(self) -> None:
        """Drive the real Rust parser and consumer with controlled child reports."""
        if shutil.which("rust-script") is None:
            self.skipTest("rust-script unavailable; Rust consumer boundary cannot run")
        with tempfile.TemporaryDirectory() as tmp:
            fake_bin = Path(tmp)
            python = fake_bin / "python3"
            python.write_text(
                """#!/bin/sh
case "${2:-}" in
  --evidence-only)
    printf '%s\n' '{"accepted":true,"evidence_reason":"qualifies","admission_satisfied":true,"admission_status":"satisfied","base_satisfied":true,"base_status":"satisfied","coverage_schema_satisfied":true,"coverage_schema_status":"satisfied","cell_results_satisfied":true,"cell_results_status":"satisfied"}'
    exit 0
    ;;
  --merge-boundary)
    case "${FAKE_BOUNDARY_MODE:-}" in
      reader)
        exit 3
        ;;
      refused)
        printf '%s\n' '{"accepted":false,"base_satisfied":false,"base_status":"base-not-current"}'
        exit 1
        ;;
      inconsistent)
        printf '%s\n' '{"accepted":false,"base_satisfied":true,"base_status":"base-not-current"}'
        exit 1
        ;;
    esac
    ;;
esac
exit 2
"""
            )
            python.chmod(0o755)

            # rc 3 is classified before report parsing because it says the
            # authority could not judge the receipt at all.
            reader = _boundary_consumer(fake_bin, "reader")
            self.assertEqual(reader.returncode, 2)
            self.assertIn("CANNOT DEREFERENCE PRODUCER DEFINITION", reader.stderr)
            self.assertIn("rc 3", reader.stderr)
            self.assertNotIn("failed final merge-boundary", reader.stderr)

            # rc 1 is not enough by itself: a coherent typed report is parsed
            # before the receipt-refusal arm becomes reachable.
            refused = _boundary_consumer(fake_bin, "refused")
            self.assertEqual(refused.returncode, 2)
            self.assertIn("failed final merge-boundary base verification", refused.stderr)
            self.assertNotIn("CANNOT DEREFERENCE PRODUCER DEFINITION", refused.stderr)

            # The same rc 1 with a contradictory status/boolean pair is a
            # reader/authority fault, proving the coherence gate is load-bearing.
            inconsistent = _boundary_consumer(fake_bin, "inconsistent")
            self.assertEqual(inconsistent.returncode, 2)
            self.assertIn("CANNOT DEREFERENCE PRODUCER DEFINITION", inconsistent.stderr)
            self.assertIn("contradicted itself", inconsistent.stderr)
            self.assertNotIn("failed final merge-boundary", inconsistent.stderr)


if __name__ == "__main__":
    unittest.main()
